use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use crate::asr::{AsrCommand, AsrEvent};
use crate::asr_process::{Request, SpawnSpec, Supervisor};
use crate::event_wake::EventNotifier;

pub const MODEL_LOAD_TIMEOUT: Duration = Duration::from_secs(180);
pub const TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrOperation {
    Load,
    Transcribe,
}
impl AsrOperation {
    const fn timeout(self) -> Duration {
        match self {
            Self::Load => MODEL_LOAD_TIMEOUT,
            Self::Transcribe => TRANSCRIPTION_TIMEOUT,
        }
    }
    fn accepts(self, event: &AsrEvent) -> bool {
        matches!(
            (self, event),
            (Self::Load, AsrEvent::Loaded(_)) | (Self::Transcribe, AsrEvent::Recognized(_))
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrTaskError {
    TimedOut(AsrOperation),
    Disconnected,
    UnexpectedOperation,
    Protocol,
    WorkerFailed,
}

/// Main-thread request identity/deadline guard. A failed PCM request is never
/// replayed; only a later explicit Load can create a new process session.
pub struct AsrTask {
    supervisor: Supervisor,
    pending: Option<(u64, AsrOperation, Instant)>,
    next_id: u64,
    ignore_through: u64,
    stopped: bool,
    #[cfg(feature = "test-support")]
    timeouts: Option<(Duration, Duration)>,
}
impl AsrTask {
    #[cfg(feature = "test-support")]
    pub fn spawn() -> Self {
        Self::spawn_notified(EventNotifier::default())
    }
    pub(crate) fn spawn_notified(notifier: EventNotifier) -> Self {
        Self::new(Supervisor::spawn_notified(
            std::env::current_exe().map(|program| SpawnSpec {
                program,
                args: vec!["--asr-worker".into()],
            }),
            notifier,
        ))
    }

    fn new(supervisor: Supervisor) -> Self {
        Self {
            supervisor,
            pending: None,
            next_id: 0,
            ignore_through: 0,
            stopped: false,
            #[cfg(feature = "test-support")]
            timeouts: None,
        }
    }
    pub fn send(&mut self, command: AsrCommand, now: Instant) -> Result<(), AsrTaskError> {
        if matches!(command, AsrCommand::Shutdown) {
            self.stop();
            return Ok(());
        }
        if self.stopped {
            return Err(AsrTaskError::Disconnected);
        }
        if self.pending.is_some() {
            return Err(AsrTaskError::UnexpectedOperation);
        }
        let operation = if matches!(command, AsrCommand::Load(_)) {
            AsrOperation::Load
        } else {
            AsrOperation::Transcribe
        };
        let timeout = operation.timeout();
        #[cfg(feature = "test-support")]
        let timeout = self.timeouts.map_or(timeout, |(load, transcribe)| {
            if operation == AsrOperation::Load {
                load
            } else {
                transcribe
            }
        });
        let deadline = now
            .checked_add(timeout)
            .ok_or(AsrTaskError::TimedOut(operation))?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(AsrTaskError::Disconnected)?;
        self.supervisor.submit(Request {
            id: self.next_id,
            deadline,
            operation,
            command,
        })?;
        self.pending = Some((self.next_id, operation, deadline));
        Ok(())
    }
    #[cfg(feature = "test-support")]
    pub fn poll(&mut self, now: Instant) -> Option<Result<AsrEvent, AsrTaskError>> {
        loop {
            if let Some(result) = self.poll_one(now)? {
                return Some(result);
            }
        }
    }
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending.map(|(_, _, deadline)| deadline)
    }
    pub(crate) fn poll_one(
        &mut self,
        now: Instant,
    ) -> Option<Option<Result<AsrEvent, AsrTaskError>>> {
        if self.stopped {
            return None;
        }
        // This runs before queued successes: AppKit delay does not extend budgets.
        if let Some((id, operation, deadline)) = self.pending {
            if now >= deadline {
                self.pending = None;
                self.ignore_through = id;
                self.supervisor.cancel(id);
                return Some(Some(Err(AsrTaskError::TimedOut(operation))));
            }
        }
        {
            match self.supervisor.events.try_recv() {
                Ok(completion)
                    if completion.id <= self.ignore_through || completion.id != self.next_id =>
                {
                    Some(None)
                }
                Ok(completion) => {
                    if let Ok(event) = &completion.result {
                        if !self
                            .pending
                            .is_some_and(|(_, operation, _)| operation.accepts(event))
                        {
                            self.supervisor.cancel(completion.id);
                            self.ignore_through = completion.id;
                            return Some(Some(Err(AsrTaskError::UnexpectedOperation)));
                        }
                    }
                    self.pending = None;
                    if completion.result.is_err() {
                        self.ignore_through = completion.id;
                    }
                    Some(Some(completion.result))
                }
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.stop();
                    Some(Some(Err(AsrTaskError::Disconnected)))
                }
            }
        }
    }
    pub(crate) fn retry_ready(&self) -> bool {
        self.supervisor.retry_ready(self.ignore_through)
    }
    pub(crate) fn prepare_explicit_retry(&mut self) -> bool {
        if !self.retry_ready() {
            return false;
        }
        if self.stopped {
            if !self.supervisor.restart_if_cleaned() {
                return false;
            }
            self.stopped = false;
            self.ignore_through = 0;
        }
        true
    }

    pub(crate) fn invalidate(&mut self) {
        self.pending = None;
        self.ignore_through = self.next_id;
        self.supervisor.cancel(self.next_id);
    }

    pub fn stop(&mut self) {
        self.stopped = true;
        self.pending = None;
        self.supervisor.stop();
    }
    pub fn cleanup_complete(&self) -> bool {
        self.supervisor.cleanup_complete()
    }
    pub fn cleanup_failed(&self) -> bool {
        self.supervisor.cleanup_failed()
    }

    #[cfg(feature = "test-support")]
    pub fn completions_sent_for_test(&self) -> u64 {
        self.supervisor.completions_sent()
    }

    #[cfg(feature = "test-support")]
    pub fn for_process_test(
        program: std::path::PathBuf,
        args: Vec<std::ffi::OsString>,
        load: Duration,
        transcribe: Duration,
    ) -> Self {
        let mut task = Self::new(Supervisor::spawn(Ok(SpawnSpec { program, args })));
        task.timeouts = Some((load, transcribe));
        task
    }
    #[cfg(feature = "test-support")]
    pub fn load_fixture_directory(
        &mut self,
        path: &std::path::Path,
        now: Instant,
    ) -> Result<(), AsrTaskError> {
        self.send(
            AsrCommand::Load(crate::model::ModelPaths::from_verified_directory(path)),
            now,
        )
    }
}
impl Drop for AsrTask {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr_process::held_after_cleanup_supervisor_for_test;

    #[test]
    fn production_timeouts_are_unchanged() {
        assert_eq!(AsrOperation::Load.timeout(), Duration::from_secs(180));
        assert_eq!(AsrOperation::Transcribe.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn shutdown_completion_waits_for_supervisor_thread_return_after_acknowledgment() {
        let (supervisor, release, acknowledged) = held_after_cleanup_supervisor_for_test();
        let task = AsrTask::new(supervisor);
        acknowledged.recv_timeout(Duration::from_secs(1)).unwrap();

        assert!(
            !task.cleanup_complete(),
            "cleanup acknowledgment alone must keep shutdown follow-up pending"
        );

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !task.cleanup_complete() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(task.cleanup_complete());
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use crate::asr_process::Completion;
    use std::sync::mpsc;

    fn task_with_queue() -> (AsrTask, mpsc::SyncSender<Completion>) {
        let mut task = AsrTask::new(Supervisor::spawn(Err(std::io::Error::other(
            "synthetic unavailable process",
        ))));
        let (sender, receiver) = mpsc::sync_channel(4);
        task.supervisor.events = receiver;
        (task, sender)
    }

    #[test]
    fn deadline_wins_over_queued_success_and_clears_its_watchdog() {
        let (mut task, sender) = task_with_queue();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        task.next_id = 1;
        task.pending = Some((1, AsrOperation::Load, deadline));
        sender
            .send(Completion {
                id: 1,
                result: Ok(AsrEvent::Loaded(Ok(()))),
            })
            .unwrap();
        assert_eq!(task.next_deadline(), Some(deadline));
        assert!(matches!(
            task.poll_one(deadline),
            Some(Some(Err(AsrTaskError::TimedOut(AsrOperation::Load))))
        ));
        assert_eq!(task.next_deadline(), None);
        assert!(
            matches!(task.poll_one(deadline), Some(None)),
            "late success is consumed, not mistaken for empty"
        );
        assert!(task.poll_one(deadline).is_none());
    }

    #[test]
    fn success_cancels_deadline_and_new_operation_has_new_identity() {
        let (mut task, sender) = task_with_queue();
        let now = Instant::now();
        task.next_id = 1;
        task.pending = Some((1, AsrOperation::Load, now + Duration::from_secs(1)));
        sender
            .send(Completion {
                id: 1,
                result: Ok(AsrEvent::Loaded(Ok(()))),
            })
            .unwrap();
        assert!(matches!(
            task.poll_one(now),
            Some(Some(Ok(AsrEvent::Loaded(Ok(())))))
        ));
        assert_eq!(task.next_deadline(), None);
        task.next_id = 2;
        task.pending = Some((2, AsrOperation::Transcribe, now + Duration::from_secs(2)));
        sender
            .send(Completion {
                id: 1,
                result: Ok(AsrEvent::Loaded(Ok(()))),
            })
            .unwrap();
        assert!(matches!(task.poll_one(now), Some(None)));
        assert_eq!(task.next_deadline(), Some(now + Duration::from_secs(2)));
        task.stop();
        assert_eq!(task.next_deadline(), None);
    }

    #[test]
    fn pending_operation_times_out_without_any_sender_event() {
        let (mut task, _sender) = task_with_queue();
        let now = Instant::now();
        task.next_id = 1;
        task.pending = Some((1, AsrOperation::Transcribe, now));
        assert!(matches!(
            task.poll_one(now),
            Some(Some(Err(AsrTaskError::TimedOut(AsrOperation::Transcribe))))
        ));
        assert_eq!(task.next_deadline(), None);
    }
}
