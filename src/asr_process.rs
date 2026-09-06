//! A single supervisor owns every child, pipe and reap. AppKit only queues work.
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::asr::{AsrCommand, AsrEvent};
use crate::asr_protocol::{self as wire, Frame, Header, Kind, HEADER_LEN};
use crate::asr_task::{AsrOperation, AsrTaskError};
use crate::event_wake::{EventNotifier, TerminalSender};

pub(crate) struct Request {
    pub id: u64,
    pub deadline: Instant,
    pub operation: AsrOperation,
    pub command: AsrCommand,
}
pub(crate) struct Completion {
    pub id: u64,
    pub result: Result<AsrEvent, AsrTaskError>,
}

#[derive(Default)]
struct Control {
    stop: AtomicBool,
    cancel_through: AtomicU64,
    cleaned: AtomicBool,
    cleanup_failed: AtomicBool,
    retired_through: AtomicU64,
    #[cfg(feature = "test-support")]
    completions_sent: AtomicU64,
}

pub(crate) struct Supervisor {
    notifier: EventNotifier,
    commands: SyncSender<Request>,
    pub events: Receiver<Completion>,
    wake: UnixStream,
    control: Arc<Control>,
    worker: thread::JoinHandle<()>,
    spec: Option<SpawnSpec>,
}

#[derive(Clone)]
pub(crate) struct SpawnSpec {
    pub program: std::path::PathBuf,
    pub args: Vec<std::ffi::OsString>,
}

impl Supervisor {
    #[cfg(any(test, feature = "test-support"))]
    pub fn spawn(spec: io::Result<SpawnSpec>) -> Self {
        Self::spawn_notified(spec, EventNotifier::default())
    }
    pub(crate) fn spawn_notified(spec: io::Result<SpawnSpec>, notifier: EventNotifier) -> Self {
        let (commands, requests) = mpsc::sync_channel(1);
        // One accepted result plus one idle failure; never an audio backlog.
        let (events, completions) = mpsc::sync_channel(2);
        let control = Arc::new(Control::default());
        let (wake, wake_reader) = UnixStream::pair().expect("ASR supervisor wake pipe");
        wake.set_nonblocking(true).expect("ASR wake nonblocking");
        wake_reader
            .set_nonblocking(true)
            .expect("ASR wake nonblocking");
        let worker_control = control.clone();
        let saved_spec = spec.as_ref().ok().cloned();
        let wake_main = notifier.clone();
        let worker = thread::spawn(move || {
            let events = TerminalSender::new(events, wake_main.clone());
            acknowledge_after_cleanup(&worker_control, || {
                supervise(
                    spec,
                    requests,
                    &events,
                    &wake_reader,
                    &worker_control,
                    &wake_main,
                );
            });
            wake_main.notify();
        });
        Self {
            notifier,
            commands,
            events: completions,
            wake,
            control,
            worker,
            spec: saved_spec,
        }
    }
    pub fn submit(&self, request: Request) -> Result<(), AsrTaskError> {
        if self.control.stop.load(Ordering::Acquire) {
            return Err(AsrTaskError::Disconnected);
        }
        self.commands
            .try_send(request)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => AsrTaskError::UnexpectedOperation,
                mpsc::TrySendError::Disconnected(_) => AsrTaskError::Disconnected,
            })?;
        self.wake();
        Ok(())
    }
    fn wake(&self) {
        let _ = (&self.wake).write(&[1]);
    }
    pub fn cancel(&self, id: u64) {
        self.control.cancel_through.fetch_max(id, Ordering::Release);
        self.wake();
    }
    pub fn stop(&self) {
        self.control.stop.store(true, Ordering::Release);
        self.wake();
    }
    pub fn retry_ready(&self, ignored: u64) -> bool {
        (self.cleaned() && self.worker.is_finished())
            || (!self.cleaned()
                && !self.control.stop.load(Ordering::Acquire)
                && self.control.retired_through.load(Ordering::Acquire) >= ignored)
    }
    pub fn restart_if_cleaned(&mut self) -> bool {
        if !self.cleaned() || !self.worker.is_finished() {
            return false;
        }
        let Some(spec) = self.spec.clone() else {
            return false;
        };
        *self = Self::spawn_notified(Ok(spec), self.notifier.clone());
        true
    }
    pub fn cleaned(&self) -> bool {
        self.control.cleaned.load(Ordering::Acquire)
    }
    #[cfg(feature = "test-support")]
    pub fn completions_sent(&self) -> u64 {
        self.control.completions_sent.load(Ordering::Acquire)
    }

    pub fn cleanup_failed(&self) -> bool {
        self.control.cleanup_failed.load(Ordering::Acquire)
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn acknowledge_after_cleanup(control: &Control, body: impl FnOnce()) {
    // Child guards finish reaping during unwinding before this acknowledgment.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    control.cleaned.store(true, Ordering::Release);
}

fn supervise(
    spec: io::Result<SpawnSpec>,
    requests: Receiver<Request>,
    events: &SyncSender<Completion>,
    wake: &UnixStream,
    control: &Arc<Control>,
    notifier: &EventNotifier,
) {
    let mut process: Option<WorkerProcess> = None;
    let mut last_id = 0;
    let mut cancelled = 0;
    loop {
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        let cancel = control.cancel_through.load(Ordering::Acquire);
        if cancel > cancelled {
            cleanup(&mut process, control);
            cancelled = cancel;
            control.retired_through.fetch_max(cancel, Ordering::Release);
            notifier.notify();
        }
        match requests.try_recv() {
            Ok(request) => {
                if request.id <= cancelled {
                    continue;
                }
                last_id = request.id;
                let result = perform(&spec, &mut process, &request, wake, control);
                if result.is_err() {
                    cleanup(&mut process, control);
                    control
                        .retired_through
                        .fetch_max(request.id, Ordering::Release);
                }
                if control.stop.load(Ordering::Acquire) {
                    break;
                }
                if events
                    .try_send(Completion {
                        id: request.id,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
                notifier.notify();
                #[cfg(feature = "test-support")]
                control.completions_sent.fetch_add(1, Ordering::Release);
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {
                let stdout = process.as_ref().map(|p| p.output.as_raw_fd());
                if wait_fds(wake, stdout, None, -1).is_err() {
                    break;
                }
                if let Some(child) = &mut process {
                    // Any idle bytes, EOF or exit are unsolicited: invalidate readiness.
                    let mut byte = [0];
                    let idle_failed = match child.output.read(&mut byte) {
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            child.child.try_wait().map_or(true, |exit| exit.is_some())
                        }
                        _ => true,
                    };
                    if idle_failed {
                        cleanup(&mut process, control);
                        control
                            .retired_through
                            .fetch_max(last_id, Ordering::Release);
                        if events
                            .try_send(Completion {
                                id: last_id,
                                result: Err(AsrTaskError::Disconnected),
                            })
                            .is_err()
                        {
                            break;
                        }
                        notifier.notify();
                    }
                }
            }
        }
    }
    cleanup(&mut process, control);
}

fn perform(
    spec: &io::Result<SpawnSpec>,
    process: &mut Option<WorkerProcess>,
    request: &Request,
    wake: &UnixStream,
    control: &Arc<Control>,
) -> Result<AsrEvent, AsrTaskError> {
    check_budget(request, control)?;
    if matches!(request.command, AsrCommand::Load(_)) {
        cleanup(process, control);
        check_budget(request, control)?;
        let spec = spec.as_ref().map_err(|_| AsrTaskError::Disconnected)?;
        *process = Some(
            WorkerProcess::spawn(spec, request.id, control.clone())
                .map_err(|_| AsrTaskError::Disconnected)?,
        );
        let child = process.as_mut().unwrap();
        let hello = Frame::new(Kind::Hello, child.session, 0, vec![]).map_err(protocol_error)?;
        child.exchange(hello, Kind::HelloAck, request, wake, control)?;
    }
    let child = process.as_mut().ok_or(AsrTaskError::Disconnected)?;
    let (kind, expected, payload) = match &request.command {
        AsrCommand::Load(paths) => (
            Kind::Load,
            Kind::Loaded,
            wire::encode_directory(paths).map_err(protocol_error)?,
        ),
        AsrCommand::Transcribe(samples) => (
            Kind::Transcribe,
            Kind::Recognized,
            wire::encode_samples(samples).map_err(protocol_error)?,
        ),
        AsrCommand::Shutdown => return Err(AsrTaskError::Disconnected),
    };
    let frame = Frame::new(kind, child.session, request.id, payload).map_err(protocol_error)?;
    let response = child.exchange(frame, expected, request, wake, control)?;
    check_budget(request, control)?;
    match expected {
        Kind::Loaded => Ok(AsrEvent::Loaded(Ok(()))),
        Kind::Recognized => Ok(AsrEvent::Recognized(Ok(String::from_utf8(
            response.payload,
        )
        .map_err(|_| AsrTaskError::Protocol)?))),
        _ => Err(AsrTaskError::Protocol),
    }
}
fn protocol_error(_: io::Error) -> AsrTaskError {
    AsrTaskError::Protocol
}
fn check_budget(request: &Request, control: &Control) -> Result<(), AsrTaskError> {
    if control.stop.load(Ordering::Acquire)
        || control.cancel_through.load(Ordering::Acquire) >= request.id
    {
        return Err(AsrTaskError::Disconnected);
    }
    if Instant::now() >= request.deadline {
        return Err(AsrTaskError::TimedOut(request.operation));
    }
    Ok(())
}

struct WorkerProcess {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    session: u64,
    control: Arc<Control>,
}
impl WorkerProcess {
    fn spawn(spec: &SpawnSpec, session: u64, control: Arc<Control>) -> io::Result<Self> {
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let process = Self {
            child,
            input,
            output,
            session,
            control,
        };
        nonblocking(process.input.as_raw_fd())?;
        nonblocking(process.output.as_raw_fd())?;
        Ok(process)
    }
    fn exchange(
        &mut self,
        frame: Frame,
        expected: Kind,
        request: &Request,
        wake: &UnixStream,
        control: &Control,
    ) -> Result<Frame, AsrTaskError> {
        let bytes = frame.encode().map_err(protocol_error)?;
        let mut written = 0;
        while written < bytes.len() {
            check_budget(request, control)?;
            match self
                .input
                .write(&bytes[written..bytes.len().min(written + 64 * 1024)])
            {
                Ok(0) => return Err(AsrTaskError::Disconnected),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.wait(request, wake, control, true)?
                }
                Err(_) => return Err(AsrTaskError::Disconnected),
            }
        }
        let mut header = [0; HEADER_LEN];
        self.read_exact(&mut header, request, wake, control)?;
        let header = Header::decode(&header).map_err(protocol_error)?;
        if header.session != self.session
            || header.request != frame.header.request
            || (header.kind != expected && header.kind != Kind::Failure)
        {
            return Err(AsrTaskError::Protocol);
        }
        let mut payload = vec![0; header.len];
        self.read_exact(&mut payload, request, wake, control)?;
        let response = Frame::new(header.kind, header.session, header.request, payload)
            .map_err(protocol_error)?;
        check_budget(request, control)?;
        if response.header.kind == Kind::Failure {
            return Err(AsrTaskError::WorkerFailed);
        }
        Ok(response)
    }
    fn read_exact(
        &mut self,
        bytes: &mut [u8],
        request: &Request,
        wake: &UnixStream,
        control: &Control,
    ) -> Result<(), AsrTaskError> {
        let mut read = 0;
        while read < bytes.len() {
            check_budget(request, control)?;
            match self.output.read(&mut bytes[read..]) {
                Ok(0) => return Err(AsrTaskError::Disconnected),
                Ok(count) => read += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.wait(request, wake, control, false)?
                }
                Err(_) => return Err(AsrTaskError::Disconnected),
            }
        }
        check_budget(request, control)
    }
    fn wait(
        &mut self,
        request: &Request,
        wake: &UnixStream,
        control: &Control,
        writing: bool,
    ) -> Result<(), AsrTaskError> {
        check_budget(request, control)?;
        let remaining = request
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(20) as i32;
        wait_fds(
            wake,
            if writing {
                None
            } else {
                Some(self.output.as_raw_fd())
            },
            if writing {
                Some(self.input.as_raw_fd())
            } else {
                None
            },
            remaining.max(1),
        )
        .map_err(|_| AsrTaskError::Disconnected)?;
        check_budget(request, control)?;
        if self
            .child
            .try_wait()
            .map_err(|_| AsrTaskError::Disconnected)?
            .is_some()
        {
            return Err(AsrTaskError::Disconnected);
        }
        Ok(())
    }
}
impl Drop for WorkerProcess {
    fn drop(&mut self) {
        // Only the supervisor thread runs this guard, including unwinding.
        // No acknowledgment or replacement is possible before successful reap.
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) | Err(_) => {
                    let _ = self.child.kill();
                }
            }
            if started.elapsed() >= Duration::from_secs(3) {
                self.control.cleanup_failed.store(true, Ordering::Release);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
fn cleanup(process: &mut Option<WorkerProcess>, _control: &Control) {
    process.take();
}
fn nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fd is an owned, live pipe; flags preserve all existing options.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn wait_fds(
    wake: &UnixStream,
    read: Option<RawFd>,
    write: Option<RawFd>,
    timeout: i32,
) -> io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: read.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: write.unwrap_or(-1),
            events: libc::POLLOUT,
            revents: 0,
        },
    ];
    // SAFETY: initialized, writable pollfd array lives through this call.
    if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    if fds[0].revents != 0 {
        let mut bytes = [0; 64];
        while matches!((&*wake).read(&mut bytes), Ok(1..)) {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_supervisor_replacement_waits_for_ack_and_thread_completion() {
        let control = Arc::new(Control::default());
        let cloned = control.clone();
        let (release, blocked) = mpsc::channel();
        let (ack, acknowledged) = mpsc::channel();
        let worker = thread::spawn(move || {
            acknowledge_after_cleanup(&cloned, || {});
            ack.send(()).unwrap();
            blocked.recv().unwrap();
        });
        let (commands, _requests) = mpsc::sync_channel(1);
        let (_events, completions) = mpsc::sync_channel(2);
        let (wake, _reader) = UnixStream::pair().unwrap();
        let mut supervisor = Supervisor {
            notifier: EventNotifier::default(),
            commands,
            events: completions,
            wake,
            control,
            worker,
            spec: Some(SpawnSpec {
                program: "/bin/sleep".into(),
                args: vec!["60".into()],
            }),
        };
        acknowledged.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(supervisor.cleaned());
        assert!(
            !supervisor.restart_if_cleaned(),
            "ack alone must not replace a live supervisor"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !supervisor.worker.is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(supervisor.restart_if_cleaned());
        supervisor.stop();
        while !supervisor.cleaned() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn supervisor_unwind_reaps_real_child_before_cleanup_acknowledgment() {
        let control = Arc::new(Control::default());
        let cloned = control.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            acknowledge_after_cleanup(&cloned, || {
                let child = WorkerProcess::spawn(
                    &SpawnSpec {
                        program: "/bin/sleep".into(),
                        args: vec!["60".into()],
                    },
                    1,
                    cloned.clone(),
                )
                .unwrap();
                sender.send(child.child.id()).unwrap();
                panic!("synthetic supervisor unwind");
            });
        });
        let pid = receiver.recv_timeout(Duration::from_secs(2)).unwrap() as i32;
        worker.join().unwrap();
        assert!(control.cleaned.load(Ordering::Acquire));
        assert!(!control.cleanup_failed.load(Ordering::Acquire));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    use crate::event_wake::{tests::pump_until, EventSource};
    use core_foundation::runloop::CFRunLoop;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[test]
    fn terminal_wake_before_thread_return_requires_later_completion_check() {
        let source = EventSource::new(CFRunLoop::get_current());
        source.attach();
        CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::ZERO,
            true,
        );
        let control = Arc::new(Control::default());
        let cloned = control.clone();
        let notifier = source.notifier();
        let terminal = notifier.clone();
        let (release, blocked) = mpsc::channel();
        let (ack, acknowledged) = mpsc::channel();
        let (events, completions) = mpsc::sync_channel(2);
        let worker = thread::spawn(move || {
            let sender = TerminalSender::new(events, terminal);
            acknowledge_after_cleanup(&cloned, || {});
            drop(sender); // observable disconnect + wake, but not thread completion
            ack.send(()).unwrap();
            blocked.recv().unwrap();
        });
        let (commands, _requests) = mpsc::sync_channel(1);
        let (wake, _reader) = UnixStream::pair().unwrap();
        let supervisor = Rc::new(RefCell::new(Supervisor {
            notifier: notifier.clone(),
            commands,
            events: completions,
            wake,
            control,
            worker,
            spec: Some(SpawnSpec {
                program: "/bin/false".into(),
                args: vec![],
            }),
        }));
        let consumer = supervisor.clone();
        let checks = Rc::new(Cell::new(0));
        let seen = checks.clone();
        let retry_ready = Rc::new(Cell::new(false));
        let ready = retry_ready.clone();
        source.set_handler(move || {
            let supervisor = consumer.borrow();
            assert!(supervisor.cleaned());
            ready.set(supervisor.retry_ready(0));
            seen.set(seen.get() + 1);
        });
        acknowledged.recv_timeout(Duration::from_secs(1)).unwrap();
        pump_until(|| checks.get() == 1);
        assert!(!retry_ready.get());
        assert!(!supervisor.borrow_mut().restart_if_cleaned());
        // Arm a pending-only one-shot before the held thread may return.
        use core_foundation::runloop::{
            kCFRunLoopCommonModes, CFRunLoopTimer, CFRunLoopTimerContext, CFRunLoopTimerRef,
        };
        extern "C" fn completion_check(_timer: CFRunLoopTimerRef, info: *mut std::ffi::c_void) {
            unsafe { &*info.cast::<EventNotifier>() }.notify();
        }
        let mut timer_notifier = notifier.clone();
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: (&mut timer_notifier as *mut EventNotifier).cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer = CFRunLoopTimer::new(
            core_foundation::date::CFDate::now().abs_time() + 0.01,
            0.0,
            0,
            0,
            completion_check,
            &mut context,
        );
        CFRunLoop::get_current().add_timer(&timer, unsafe { kCFRunLoopCommonModes });
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !supervisor.borrow().worker.is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        // No second terminal notification exists: the one-shot drives retry.
        pump_until(|| checks.get() == 2);
        CFRunLoop::get_current().remove_timer(&timer, unsafe { kCFRunLoopCommonModes });
        assert!(retry_ready.get());
        assert!(supervisor.borrow_mut().restart_if_cleaned());
        source.close();
        supervisor.borrow().stop();
    }

    #[test]
    fn supervisor_result_and_shutdown_acknowledgment_wake_main_without_ticks() {
        let source = EventSource::new(CFRunLoop::get_current());
        let supervisor = Rc::new(RefCell::new(Supervisor::spawn_notified(
            Err(io::Error::other("synthetic process failure")),
            source.notifier(),
        )));
        let consumer = supervisor.clone();
        let results = Rc::new(Cell::new(0));
        let seen = results.clone();
        let cleaned = Rc::new(Cell::new(false));
        let cleanup = cleaned.clone();
        source.set_handler(move || {
            let supervisor = consumer.borrow();
            if let Ok(completion) = supervisor.events.try_recv() {
                assert!(completion.result.is_err());
                seen.set(seen.get() + 1);
            }
            cleanup.set(supervisor.cleaned());
        });
        source.attach();
        supervisor
            .borrow()
            .submit(Request {
                id: 1,
                deadline: Instant::now() + Duration::from_secs(1),
                operation: AsrOperation::Load,
                command: AsrCommand::Load(crate::model::ModelPaths::from_verified_directory(
                    std::path::Path::new("/synthetic-unused-model"),
                )),
            })
            .unwrap();
        pump_until(|| results.get() == 1);
        supervisor.borrow().stop();
        pump_until(|| cleaned.get());
    }
}
