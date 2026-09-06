//! Scripted process fixture. This binary is excluded from production builds.
use ptt2me::asr_test_support::{decode_samples, isolate_stdout, Frame, Kind};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;

fn hang() -> ! {
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
fn log(path: &Path, event: &str) {
    writeln!(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap(),
        "{event}"
    )
    .unwrap();
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let scenario = &args[1];
    let path = Path::new(&args[2]);
    let prior = std::fs::read_to_string(path).unwrap_or_default();
    let prior_pid = prior
        .lines()
        .rfind(|line| line.starts_with("spawn "))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|pid| pid.parse::<i32>().ok());
    let previous_gone = prior_pid.is_none_or(|pid| unsafe { libc::kill(pid, 0) } == -1);
    log(
        path,
        &format!("spawn {} {}", std::process::id(), previous_gone),
    );
    let launch = prior
        .lines()
        .filter(|line| line.starts_with("spawn "))
        .count();
    let scenario = match scenario.as_str() {
        "recover" if launch > 0 => "normal",
        "recover" => "transcribe_hang",
        "reload_fail" if launch > 0 => "load_hang",
        "reload_fail" => "transcribe_hang",
        other => other,
    };
    let mut output = isolate_stdout().unwrap();
    // Native stdout contamination must not corrupt the private frame transport.
    println!("synthetic native stdout");
    std::io::stdout().flush().unwrap();
    let mut input = std::io::stdin().lock();
    if scenario == "handshake_hang" {
        hang();
    }
    let hello = Frame::read(&mut input).unwrap();
    assert_eq!(hello.header.kind, Kind::Hello);
    let session = hello.header.session;
    Frame::new(Kind::HelloAck, session, 0, vec![])
        .unwrap()
        .write(&mut output)
        .unwrap();
    loop {
        let request = Frame::read(&mut input).unwrap();
        let id = request.header.request;
        match request.header.kind {
            Kind::Load => {
                log(path, "load");
                if scenario == "load_hang" {
                    hang();
                }
                if scenario == "crash_load" {
                    std::process::exit(4);
                }
                Frame::new(Kind::Loaded, session, id, vec![])
                    .unwrap()
                    .write(&mut output)
                    .unwrap();
                log(path, "loaded");
                if scenario == "idle_crash" {
                    thread::sleep(Duration::from_millis(80));
                    std::process::exit(5);
                }
                if scenario == "write_hang" {
                    log(path, "not_reading");
                    hang();
                }
            }
            Kind::Transcribe => {
                let samples = decode_samples(&request.payload).unwrap();
                assert!(samples.iter().all(|v| v.is_finite()));
                log(path, "transcribe");
                if scenario == "transcribe_hang" {
                    hang();
                }
                if scenario == "late_success" {
                    thread::sleep(Duration::from_millis(700));
                }
                let mut bytes =
                    Frame::new(Kind::Recognized, session, id, "тест Ω".as_bytes().to_vec())
                        .unwrap()
                        .encode()
                        .unwrap();
                match scenario {
                    "partial_header" => {
                        output.write_all(&bytes[..12]).unwrap();
                        hang();
                    }
                    "partial_body" => {
                        output.write_all(&bytes[..29]).unwrap();
                        hang();
                    }
                    "partial_eof" => {
                        output.write_all(&bytes[..12]).unwrap();
                        return;
                    }
                    "oversized" => {
                        bytes[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
                        bytes.truncate(28);
                    }
                    "bad_session" => bytes[8..16].copy_from_slice(&(session + 1).to_le_bytes()),
                    "bad_request" => bytes[16..24].copy_from_slice(&(id + 1).to_le_bytes()),
                    "bad_version" => bytes[4] = 255,
                    "bad_kind" => bytes[6] = 255,
                    "invalid_utf8" => bytes[28] = 255,
                    "crash_transcribe" => std::process::exit(6),
                    _ => {}
                }
                output.write_all(&bytes).unwrap();
                output.flush().unwrap();
                if scenario == "duplicate" {
                    output.write_all(&bytes).unwrap();
                    output.flush().unwrap();
                }
            }
            Kind::Shutdown => return,
            _ => panic!("invalid fixture operation"),
        }
    }
}
