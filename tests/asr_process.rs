use ptt2me::asr::{AsrCommand, AsrEvent};
use ptt2me::asr_test_support::{
    AsrOperation, AsrTask, AsrTaskError, MAX_SAMPLES, MODEL_LOAD_TIMEOUT, TRANSCRIPTION_TIMEOUT,
};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const LOAD: Duration = Duration::from_secs(2);
const TRANSCRIBE: Duration = Duration::from_millis(250);
fn task(scenario: &str, log: &Path) -> AsrTask {
    AsrTask::for_process_test(
        env!("CARGO_BIN_EXE_asr-test-worker").into(),
        vec![scenario.into(), log.as_os_str().into()],
        LOAD,
        TRANSCRIBE,
    )
}
fn until(mut check: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(4);
    while !check() {
        assert!(Instant::now() < limit, "fixture watchdog");
        thread::sleep(Duration::from_millis(2));
    }
}
fn result(task: &mut AsrTask) -> Result<AsrEvent, AsrTaskError> {
    let mut value = None;
    until(|| {
        value = task.poll(Instant::now());
        value.is_some()
    });
    value.unwrap()
}
fn load(task: &mut AsrTask) {
    task.load_fixture_directory(Path::new("/tmp/synthetic-asr-model"), Instant::now())
        .unwrap();
    assert_eq!(result(task), Ok(AsrEvent::Loaded(Ok(()))));
}
fn pids(log: &Path) -> Vec<i32> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("spawn "))
        .map(|line| line.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect()
}
fn assert_reaped(pid: i32) {
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "child remains alive or zombie"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD),
        "supervisor must already have reaped"
    );
}
fn stop(task: &mut AsrTask, log: &Path) {
    let started = Instant::now();
    task.stop();
    assert!(started.elapsed() < Duration::from_millis(100));
    until(|| task.cleanup_complete());
    for pid in pids(log) {
        assert_reaped(pid);
    }
}

#[test]
fn real_worker_hidden_mode_rejects_early_eof_and_combined_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_ptt2me"))
        .arg("--asr-worker")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let output = Command::new(env!("CARGO_BIN_EXE_ptt2me"))
        .args(["--asr-worker", "--smoke-model"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    // Real child handshake occurs without loading model/TCC/AppKit.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ptt2me"))
        .arg("--asr-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let frame =
        ptt2me::asr_test_support::Frame::new(ptt2me::asr_test_support::Kind::Hello, 9, 0, vec![])
            .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&frame.encode().unwrap())
        .unwrap();
    let ack = ptt2me::asr_test_support::Frame::read(child.stdout.as_mut().unwrap()).unwrap();
    assert_eq!(ack.header.kind, ptt2me::asr_test_support::Kind::HelloAck);
    child.stdin.take();
    assert!(!child.wait().unwrap().success());
}
#[test]
fn normal_process_unicode_and_native_stdout_isolation() {
    assert_eq!(MODEL_LOAD_TIMEOUT, Duration::from_secs(180));
    assert_eq!(TRANSCRIPTION_TIMEOUT, Duration::from_secs(60));
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events");
    let mut task = task("normal", &log);
    load(&mut task);
    task.send(
        AsrCommand::Transcribe(vec![0.0, -0.2, 1.25]),
        Instant::now(),
    )
    .unwrap();
    assert_eq!(
        task.send(AsrCommand::Transcribe(vec![0.0]), Instant::now()),
        Err(AsrTaskError::UnexpectedOperation)
    );
    assert_eq!(
        result(&mut task),
        Ok(AsrEvent::Recognized(Ok("тест Ω".into())))
    );
    stop(&mut task, &log);
    assert_eq!(pids(&log).len(), 1);
}
#[test]
fn hangs_and_full_stdin_pipe_obey_operation_deadlines() {
    for scenario in [
        "handshake_hang",
        "load_hang",
        "write_hang",
        "transcribe_hang",
        "partial_header",
        "partial_body",
        "late_success",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events");
        let mut task = task(scenario, &log);
        let expected = if matches!(scenario, "handshake_hang" | "load_hang") {
            task.load_fixture_directory(Path::new("/tmp/fixture"), Instant::now())
                .unwrap();
            AsrOperation::Load
        } else {
            load(&mut task);
            task.send(
                AsrCommand::Transcribe(vec![0.1; MAX_SAMPLES]),
                Instant::now(),
            )
            .unwrap();
            AsrOperation::Transcribe
        };
        assert_eq!(
            result(&mut task),
            Err(AsrTaskError::TimedOut(expected)),
            "{scenario}"
        );
        stop(&mut task, &log);
        assert!(
            !pids(&log).is_empty(),
            "fixture must really launch: {scenario}"
        );
    }
}
#[test]
fn malformed_truncated_crashed_and_unsolicited_responses_invalidate_process() {
    for scenario in [
        "partial_eof",
        "oversized",
        "bad_session",
        "bad_request",
        "bad_version",
        "bad_kind",
        "invalid_utf8",
        "crash_transcribe",
        "duplicate",
        "idle_crash",
        "crash_load",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events");
        let mut task = task(scenario, &log);
        task.load_fixture_directory(Path::new("/tmp/fixture"), Instant::now())
            .unwrap();
        if scenario == "crash_load" {
            assert!(result(&mut task).is_err());
        } else {
            assert_eq!(result(&mut task), Ok(AsrEvent::Loaded(Ok(()))));
            if scenario != "idle_crash" {
                task.send(AsrCommand::Transcribe(vec![0.1]), Instant::now())
                    .unwrap();
                if scenario == "duplicate" {
                    assert!(matches!(result(&mut task), Ok(AsrEvent::Recognized(_))));
                }
            }
            assert!(result(&mut task).is_err(), "{scenario}");
        }
        stop(&mut task, &log);
    }
}
#[test]
fn shutdown_interrupts_load_read_and_full_pipe_write_and_acknowledges_reap() {
    for scenario in [
        "load_hang",
        "transcribe_hang",
        "write_hang",
        "partial_header",
        "partial_body",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events");
        let mut task = task(scenario, &log);
        if scenario == "load_hang" {
            task.load_fixture_directory(Path::new("/tmp/fixture"), Instant::now())
                .unwrap();
        } else {
            load(&mut task);
            task.send(
                AsrCommand::Transcribe(vec![0.1; MAX_SAMPLES]),
                Instant::now(),
            )
            .unwrap();
        }
        until(|| !pids(&log).is_empty());
        thread::sleep(Duration::from_millis(25));
        stop(&mut task, &log);
        assert_eq!(task.poll(Instant::now()), None);
    }
}
#[test]
fn failed_operation_is_not_replayed_and_old_pid_is_reaped_before_reload() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events");
    let mut task = task("recover", &log);
    load(&mut task);
    task.send(AsrCommand::Transcribe(vec![0.1]), Instant::now())
        .unwrap();
    assert_eq!(
        result(&mut task),
        Err(AsrTaskError::TimedOut(AsrOperation::Transcribe))
    );
    load(&mut task);
    let history = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        history.lines().filter(|l| *l == "transcribe").count(),
        1,
        "no replay"
    );
    assert!(history
        .lines()
        .filter(|l| l.starts_with("spawn "))
        .all(|line| line.ends_with("true")));
    assert_eq!(pids(&log).len(), 2);
    assert_reaped(pids(&log)[0]);
    task.send(AsrCommand::Transcribe(vec![0.2]), Instant::now())
        .unwrap();
    assert_eq!(
        result(&mut task),
        Ok(AsrEvent::Recognized(Ok("тест Ω".into())))
    );
    stop(&mut task, &log);
}
#[test]
fn repeated_reloads_never_accumulate_children() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events");
    let mut task = task("normal", &log);
    for _ in 0..4 {
        load(&mut task);
    }
    let history = std::fs::read_to_string(&log).unwrap();
    assert_eq!(pids(&log).len(), 4);
    assert!(history
        .lines()
        .filter(|l| l.starts_with("spawn "))
        .all(|line| line.ends_with("true")));
    for pid in pids(&log).iter().take(3) {
        assert_reaped(*pid);
    }
    stop(&mut task, &log);
}
#[test]
fn dropping_busy_handle_signals_cleanup_without_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events");
    let mut task = task("load_hang", &log);
    task.load_fixture_directory(Path::new("/tmp/fixture"), Instant::now())
        .unwrap();
    until(|| !pids(&log).is_empty());
    let pid = pids(&log)[0];
    let start = Instant::now();
    drop(task);
    assert!(start.elapsed() < Duration::from_millis(100));
    until(|| unsafe { libc::kill(pid, 0) } == -1);
    assert_reaped(pid);
}

#[test]
fn deadlines_precede_already_queued_success_and_stale_results_cannot_cross_reload() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events");
    let mut task = task("normal", &log);
    let started = Instant::now();
    task.load_fixture_directory(Path::new("/tmp/fixture"), started)
        .unwrap();
    until(|| task.completions_sent_for_test() == 1);
    assert_eq!(
        task.poll(started + LOAD),
        Some(Err(AsrTaskError::TimedOut(AsrOperation::Load)))
    );
    load(&mut task);
    let started = Instant::now();
    task.send(AsrCommand::Transcribe(vec![0.2]), started)
        .unwrap();
    until(|| task.completions_sent_for_test() == 3);
    assert_eq!(
        task.poll(started + TRANSCRIBE),
        Some(Err(AsrTaskError::TimedOut(AsrOperation::Transcribe)))
    );
    load(&mut task);
    assert_eq!(task.poll(Instant::now()), None);
    task.send(AsrCommand::Transcribe(vec![0.3]), Instant::now())
        .unwrap();
    assert_eq!(
        result(&mut task),
        Ok(AsrEvent::Recognized(Ok("тест Ω".into())))
    );
    stop(&mut task, &log);
}

#[test]
fn native_child_reverifies_model_directory_before_backend_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let mut task = AsrTask::for_process_test(
        env!("CARGO_BIN_EXE_ptt2me").into(),
        vec!["--asr-worker".into()],
        LOAD,
        TRANSCRIBE,
    );
    // Empty fixture directory cannot be promoted to trusted native paths.
    task.load_fixture_directory(dir.path(), Instant::now())
        .unwrap();
    assert_eq!(result(&mut task), Err(AsrTaskError::WorkerFailed));
    task.stop();
    until(|| task.cleanup_complete());
}
