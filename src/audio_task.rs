//! One bounded background lane for completed microphone captures.
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use crate::audio::CompletedCapture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioPreparationError {
    Busy,
    Panicked,
    Disconnected,
}

pub(crate) struct AudioPreparationResult {
    pub(crate) generation: u64,
    pub(crate) result: Result<Option<Vec<f32>>, AudioPreparationError>,
    pub(crate) elapsed: Duration,
    pub(crate) native_sample_count: usize,
}

struct PreparationJob {
    generation: u64,
    capture: CompletedCapture,
}

struct PendingPreparation {
    generation: u64,
    cancelled: bool,
}

/// Cancellation discards relevance, not physical work. Until its completion is
/// observed a cancelled job still owns the sole slot; no replacement is spawned.
pub(crate) struct AudioPreparationTask {
    commands: Option<SyncSender<PreparationJob>>,
    events: Option<Receiver<AudioPreparationResult>>,
    pending: Option<PendingPreparation>,
}

impl AudioPreparationTask {
    pub(crate) fn spawn() -> Self {
        Self::spawn_with(CompletedCapture::prepare)
    }

    fn spawn_with<F>(mut prepare: F) -> Self
    where
        F: FnMut(CompletedCapture) -> Option<Vec<f32>> + Send + 'static,
    {
        let (commands, jobs) = mpsc::sync_channel::<PreparationJob>(1);
        let (results, events) = mpsc::sync_channel(1);
        // Deliberately detach: stop/Drop never join potentially blocked work.
        thread::spawn(move || {
            while let Ok(job) = jobs.recv() {
                let start = Instant::now();
                let native_sample_count = job.capture.native_sample_count();
                let result = catch_unwind(AssertUnwindSafe(|| prepare(job.capture)))
                    .map_err(|_| AudioPreparationError::Panicked);
                if results
                    .send(AudioPreparationResult {
                        generation: job.generation,
                        result,
                        elapsed: start.elapsed(),
                        native_sample_count,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self::new(commands, events)
    }

    fn new(commands: SyncSender<PreparationJob>, events: Receiver<AudioPreparationResult>) -> Self {
        Self {
            commands: Some(commands),
            events: Some(events),
            pending: None,
        }
    }

    pub(crate) fn submit(
        &mut self,
        generation: u64,
        capture: CompletedCapture,
    ) -> Result<(), AudioPreparationError> {
        if self.pending.is_some() {
            return Err(AudioPreparationError::Busy);
        }
        let Some(commands) = &self.commands else {
            return Err(AudioPreparationError::Disconnected);
        };
        match commands.try_send(PreparationJob {
            generation,
            capture,
        }) {
            Ok(()) => {
                self.pending = Some(PendingPreparation {
                    generation,
                    cancelled: false,
                });
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(AudioPreparationError::Busy),
            Err(TrySendError::Disconnected(_)) => {
                self.stop();
                Err(AudioPreparationError::Disconnected)
            }
        }
    }

    pub(crate) fn poll(&mut self) -> Option<AudioPreparationResult> {
        match self.events.as_ref()?.try_recv() {
            Ok(result) => {
                let pending = self.pending.take()?;
                (!pending.cancelled && pending.generation == result.generation).then_some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                let pending = self.pending.take();
                self.stop();
                pending
                    .filter(|pending| !pending.cancelled)
                    .map(|pending| AudioPreparationResult {
                        generation: pending.generation,
                        result: Err(AudioPreparationError::Disconnected),
                        elapsed: Duration::ZERO,
                        native_sample_count: 0,
                    })
            }
        }
    }

    pub(crate) fn cancel(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.cancelled = true;
        }
    }

    pub(crate) fn stop(&mut self) {
        self.commands.take();
        self.events.take();
        self.pending = None;
    }
}

impl Drop for AudioPreparationTask {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioRecorder;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn capture() -> CompletedCapture {
        AudioRecorder::new().finish().unwrap()
    }

    fn wait(task: &mut AudioPreparationTask) -> AudioPreparationResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = task.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "preparation did not complete");
            thread::yield_now();
        }
    }

    #[test]
    fn jobs_run_off_caller_on_one_persistent_worker() {
        let (ids, received_ids) = mpsc::channel();
        let mut task = AudioPreparationTask::spawn_with(move |capture| {
            ids.send(thread::current().id()).unwrap();
            capture.prepare()
        });
        task.submit(1, capture()).unwrap();
        let first = wait(&mut task);
        assert_eq!(first.generation, 1);
        assert_eq!(first.result, Ok(None));
        task.submit(2, capture()).unwrap();
        assert_eq!(wait(&mut task).generation, 2);
        let first_id = received_ids.recv().unwrap();
        assert_ne!(first_id, thread::current().id());
        assert_eq!(first_id, received_ids.recv().unwrap());
    }

    #[test]
    fn cancel_keeps_physical_slot_busy_and_discards_late_output() {
        let (entered, entry) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut task = AudioPreparationTask::spawn_with(move |_| {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            Some(vec![0.25])
        });
        task.submit(1, capture()).unwrap();
        entry.recv_timeout(Duration::from_secs(1)).unwrap();
        task.cancel();
        assert_eq!(task.submit(2, capture()), Err(AudioPreparationError::Busy));
        assert!(task.poll().is_none());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while task.pending.is_some() {
            assert!(task.poll().is_none(), "cancelled PCM escaped");
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        task.submit(3, capture()).unwrap();
        entry.recv_timeout(Duration::from_secs(1)).unwrap();
        release.send(()).unwrap();
        assert_eq!(wait(&mut task).generation, 3);
    }

    #[test]
    fn preparation_panic_is_recoverable_on_the_same_worker() {
        let mut first = true;
        let mut task = AudioPreparationTask::spawn_with(move |_| {
            if first {
                first = false;
                panic!("synthetic preparation failure");
            }
            None
        });
        task.submit(1, capture()).unwrap();
        assert_eq!(wait(&mut task).result, Err(AudioPreparationError::Panicked));
        task.submit(2, capture()).unwrap();
        assert_eq!(wait(&mut task).result, Ok(None));
    }

    #[test]
    fn disconnected_pending_worker_reports_failure_once() {
        let (commands, _receiver) = mpsc::sync_channel(1);
        let (sender, events) = mpsc::sync_channel(1);
        let mut task = AudioPreparationTask::new(commands, events);
        task.submit(7, capture()).unwrap();
        drop(sender);
        let result = task.poll().unwrap();
        assert_eq!(result.generation, 7);
        assert_eq!(result.result, Err(AudioPreparationError::Disconnected));
        assert!(task.poll().is_none());
        assert_eq!(
            task.submit(8, capture()),
            Err(AudioPreparationError::Disconnected)
        );
    }

    #[test]
    fn dropping_blocked_worker_never_joins() {
        let (entered, entry) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut task = AudioPreparationTask::spawn_with(move |_| {
            entered.send(()).unwrap();
            let _ = blocked.recv();
            None
        });
        task.submit(1, capture()).unwrap();
        entry.recv_timeout(Duration::from_secs(1)).unwrap();
        let (dropped, done) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(task);
            dropped.send(()).unwrap();
        });
        let result = done.recv_timeout(Duration::from_secs(1));
        let _ = release.send(());
        dropper.join().unwrap();
        assert!(result.is_ok());
    }
}
