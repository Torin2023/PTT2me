use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::asr::{spawn_asr_worker, AsrCommand, AsrEvent};

const MODEL_LOAD_TIMEOUT: Duration = Duration::from_secs(180);
const TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AsrOperation {
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
pub(crate) enum AsrTaskError {
    TimedOut(AsrOperation),
    Disconnected,
    UnexpectedOperation,
}

/// One serial worker session. A timeout permanently closes this session,
/// including its result receiver, so a late native result can never be pasted.
/// Recovery requires an app restart: replacing an uninterruptible native thread
/// here could accumulate recognizers and model memory after repeated timeouts.
pub(crate) struct AsrTask {
    commands: Option<Sender<AsrCommand>>,
    events: Option<Receiver<AsrEvent>>,
    worker: Option<JoinHandle<()>>,
    pending: Option<(AsrOperation, Instant)>,
}

impl AsrTask {
    pub(crate) fn spawn() -> Self {
        let (commands, command_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        let worker = spawn_asr_worker(command_receiver, event_sender);
        Self::new(commands, events, worker)
    }

    fn new(
        commands: Sender<AsrCommand>,
        events: Receiver<AsrEvent>,
        worker: JoinHandle<()>,
    ) -> Self {
        Self {
            commands: Some(commands),
            events: Some(events),
            worker: Some(worker),
            pending: None,
        }
    }

    pub(crate) fn send(&mut self, command: AsrCommand, now: Instant) -> Result<(), AsrTaskError> {
        let operation = match command {
            AsrCommand::Load(_) => AsrOperation::Load,
            AsrCommand::Transcribe(_) => AsrOperation::Transcribe,
            AsrCommand::Shutdown => {
                self.stop();
                return Ok(());
            }
        };
        if self.pending.is_some() {
            self.stop();
            return Err(AsrTaskError::UnexpectedOperation);
        }
        let Some(deadline) = now.checked_add(operation.timeout()) else {
            self.stop();
            return Err(AsrTaskError::TimedOut(operation));
        };
        let Some(commands) = &self.commands else {
            return Err(AsrTaskError::Disconnected);
        };
        if commands.send(command).is_err() {
            self.stop();
            return Err(AsrTaskError::Disconnected);
        }
        self.pending = Some((operation, deadline));
        Ok(())
    }

    pub(crate) fn poll(&mut self, now: Instant) -> Option<Result<AsrEvent, AsrTaskError>> {
        let events = self.events.as_ref()?;
        // Check the deadline before reading queued results. A delayed event
        // must not revive a request whose time budget has already expired.
        if let Some((operation, deadline)) = self.pending {
            if now >= deadline {
                self.stop();
                return Some(Err(AsrTaskError::TimedOut(operation)));
            }
        }
        match events.try_recv() {
            Ok(event) => {
                if !self
                    .pending
                    .is_some_and(|(operation, _)| operation.accepts(&event))
                {
                    self.stop();
                    return Some(Err(AsrTaskError::UnexpectedOperation));
                }
                self.pending = None;
                Some(Ok(event))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.stop();
                Some(Err(AsrTaskError::Disconnected))
            }
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(AsrCommand::Shutdown);
        }
        self.events.take();
        self.pending = None;
        // Dropping the handle detaches the worker. AppKit must never join a
        // thread that may be stuck inside a native recognizer call.
        self.worker.take();
    }
}

impl Drop for AsrTask {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::thread;

    use super::*;
    use crate::model::ModelPaths;

    fn load_command() -> AsrCommand {
        AsrCommand::Load(ModelPaths::for_test(
            PathBuf::from("encoder.int8.onnx"),
            PathBuf::from("decoder.onnx"),
            PathBuf::from("joiner.onnx"),
            PathBuf::from("tokens.txt"),
        ))
    }

    fn controlled_task() -> (AsrTask, Receiver<AsrCommand>, Sender<AsrEvent>) {
        let (commands, command_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        let worker = thread::spawn(|| {});
        (
            AsrTask::new(commands, events, worker),
            command_receiver,
            event_sender,
        )
    }

    #[test]
    fn successful_operations_clear_their_deadlines() {
        let (mut task, commands, events) = controlled_task();
        let start = Instant::now();
        task.send(load_command(), start).unwrap();
        assert!(matches!(commands.recv().unwrap(), AsrCommand::Load(_)));
        events.send(AsrEvent::Loaded(Ok(()))).unwrap();
        assert_eq!(task.poll(start), Some(Ok(AsrEvent::Loaded(Ok(())))));

        let later = start + MODEL_LOAD_TIMEOUT;
        assert_eq!(task.poll(later), None);
        task.send(AsrCommand::Transcribe(vec![0.25]), later)
            .unwrap();
        assert!(matches!(commands.recv().unwrap(), AsrCommand::Transcribe(_)));
        events
            .send(AsrEvent::Recognized(Ok("привет".into())))
            .unwrap();
        assert_eq!(
            task.poll(later),
            Some(Ok(AsrEvent::Recognized(Ok("привет".into()))))
        );
        assert_eq!(task.poll(later + TRANSCRIPTION_TIMEOUT), None);
    }

    #[test]
    fn load_timeout_closes_the_session_and_is_reported_once() {
        let (mut task, commands, events) = controlled_task();
        let start = Instant::now();
        task.send(load_command(), start).unwrap();
        commands.recv().unwrap();
        assert_eq!(
            task.poll(start + MODEL_LOAD_TIMEOUT - Duration::from_millis(1)),
            None
        );
        assert_eq!(
            task.poll(start + MODEL_LOAD_TIMEOUT),
            Some(Err(AsrTaskError::TimedOut(AsrOperation::Load)))
        );
        assert!(matches!(commands.recv().unwrap(), AsrCommand::Shutdown));
        assert!(events.send(AsrEvent::Loaded(Ok(()))).is_err());
        assert_eq!(task.poll(start + MODEL_LOAD_TIMEOUT), None);
        assert_eq!(
            task.send(load_command(), start),
            Err(AsrTaskError::Disconnected)
        );
    }

    #[test]
    fn transcription_timeout_discards_even_a_queued_result() {
        let (mut task, commands, events) = controlled_task();
        let start = Instant::now();
        task.send(AsrCommand::Transcribe(vec![0.25]), start)
            .unwrap();
        commands.recv().unwrap();
        events
            .send(AsrEvent::Recognized(Ok("late result".into())))
            .unwrap();

        assert_eq!(
            task.poll(start + TRANSCRIPTION_TIMEOUT),
            Some(Err(AsrTaskError::TimedOut(AsrOperation::Transcribe)))
        );
        assert_eq!(task.poll(start + TRANSCRIPTION_TIMEOUT), None);
        assert_eq!(
            task.send(AsrCommand::Transcribe(vec![0.5]), start),
            Err(AsrTaskError::Disconnected)
        );
    }

    #[test]
    fn disconnected_worker_is_reported_once() {
        let (mut task, _commands, events) = controlled_task();
        drop(events);
        let now = Instant::now();
        assert_eq!(task.poll(now), Some(Err(AsrTaskError::Disconnected)));
        assert_eq!(task.poll(now), None);
    }

    #[test]
    fn unsolicited_result_cannot_reach_the_runtime() {
        let (mut task, _commands, events) = controlled_task();
        events
            .send(AsrEvent::Recognized(Ok("unexpected".into())))
            .unwrap();
        assert_eq!(
            task.poll(Instant::now()),
            Some(Err(AsrTaskError::UnexpectedOperation))
        );
    }

    #[test]
    fn dropping_a_busy_task_does_not_wait_for_the_worker() {
        let (commands, _command_receiver) = mpsc::channel();
        let (_event_sender, events) = mpsc::channel();
        let (release, wait_for_release) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = wait_for_release.recv();
        });
        let task = AsrTask::new(commands, events, worker);
        let (dropped, wait_for_drop) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(task);
            dropped.send(()).unwrap();
        });

        let result = wait_for_drop.recv_timeout(Duration::from_secs(1));
        let _ = release.send(());
        dropper.join().unwrap();
        assert!(result.is_ok());
    }
}
