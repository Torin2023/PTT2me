//! One bounded background lane for completed microphone captures.
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::audio::CompletedCapture;
use crate::event_wake::{EventNotifier, TerminalSender};

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
    notifier: EventNotifier,
    commands: Option<SyncSender<PreparationJob>>,
    events: Option<Receiver<AudioPreparationResult>>,
    pending: Option<PendingPreparation>,
    worker: Option<JoinHandle<()>>,
}

impl AudioPreparationTask {
    #[cfg(test)]
    pub(crate) fn spawn() -> Self {
        Self::spawn_notified(EventNotifier::default())
    }

    pub(crate) fn spawn_notified(notifier: EventNotifier) -> Self {
        Self::spawn_with_notifier(CompletedCapture::prepare, notifier)
    }

    #[cfg(test)]
    fn spawn_with<F>(prepare: F) -> Self
    where
        F: FnMut(CompletedCapture) -> Option<Vec<f32>> + Send + 'static,
    {
        Self::spawn_with_notifier(prepare, EventNotifier::default())
    }

    fn spawn_with_notifier<F>(mut prepare: F, notifier: EventNotifier) -> Self
    where
        F: FnMut(CompletedCapture) -> Option<Vec<f32>> + Send + 'static,
    {
        let (commands, jobs) = mpsc::sync_channel::<PreparationJob>(1);
        let (results, events) = mpsc::sync_channel(1);
        // Retain liveness evidence for disconnect recovery; never join on AppKit.
        let wake = notifier.clone();
        let worker = thread::spawn(move || {
            let results = TerminalSender::new(results, wake.clone());
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
                wake.notify();
            }
        });
        let mut task = Self::new(commands, events, worker);
        task.notifier = notifier;
        task
    }

    fn new(
        commands: SyncSender<PreparationJob>,
        events: Receiver<AudioPreparationResult>,
        worker: JoinHandle<()>,
    ) -> Self {
        Self {
            notifier: EventNotifier::default(),
            commands: Some(commands),
            events: Some(events),
            pending: None,
            worker: Some(worker),
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
        if self.commands.is_none() && self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            // Channel disconnect alone is not proof of worker termination.
            // A stopped task has no handle and can never restart here.
            *self = Self::spawn_notified(self.notifier.clone());
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
                self.disconnect();
                Err(AudioPreparationError::Disconnected)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn poll(&mut self) -> Option<AudioPreparationResult> {
        self.poll_one().flatten()
    }

    // Outer Some reports consumption even when a cancelled result is suppressed.
    pub(crate) fn poll_one(&mut self) -> Option<Option<AudioPreparationResult>> {
        match self.events.as_ref()?.try_recv() {
            Ok(result) => Some(self.pending.take().and_then(|pending| {
                (!pending.cancelled && pending.generation == result.generation).then_some(result)
            })),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                let pending = self.pending.take();
                self.disconnect();
                Some(pending.filter(|pending| !pending.cancelled).map(|pending| {
                    AudioPreparationResult {
                        generation: pending.generation,
                        result: Err(AudioPreparationError::Disconnected),
                        elapsed: Duration::ZERO,
                        native_sample_count: 0,
                    }
                }))
            }
        }
    }

    pub(crate) fn cancel(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.cancelled = true;
        }
    }

    fn disconnect(&mut self) {
        self.commands.take();
        self.events.take();
        self.pending = None;
    }

    pub(crate) fn stop(&mut self) {
        self.disconnect();
        // Dropping a handle detaches even a blocked thread. Its absence also
        // disables replacement forever for this stopped task.
        self.worker.take();
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
    fn disconnect_restarts_only_after_old_worker_is_confirmed_finished() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (sender, events) = mpsc::sync_channel(1);
        let (disconnected, observed_disconnect) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let worker = thread::spawn(move || {
            receiver.recv().unwrap();
            drop(sender);
            drop(receiver);
            disconnected.send(()).unwrap();
            blocked.recv().unwrap();
        });
        let mut task = AudioPreparationTask::new(commands, events, worker);
        task.submit(7, capture()).unwrap();
        observed_disconnect
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let result = task.poll().unwrap();
        assert_eq!(result.generation, 7);
        assert_eq!(result.result, Err(AudioPreparationError::Disconnected));
        assert!(task.poll().is_none());
        assert_eq!(
            task.submit(8, capture()),
            Err(AudioPreparationError::Disconnected)
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !task.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        task.submit(9, capture()).unwrap();
        let result = wait(&mut task);
        assert_eq!(result.generation, 9);
        assert_eq!(result.result, Ok(None));
    }

    #[test]
    fn shutdown_never_spawns_a_replacement() {
        let mut task = AudioPreparationTask::spawn();
        task.stop();
        assert_eq!(
            task.submit(1, capture()),
            Err(AudioPreparationError::Disconnected)
        );
        assert!(task.worker.is_none());
        assert!(task.poll().is_none());
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

#[cfg(test)]
mod wake_tests {
    use super::*;
    use crate::audio::AudioRecorder;
    use crate::event_wake::{tests::pump_until, EventSource};
    use core_foundation::runloop::CFRunLoop;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[test]
    fn confirmed_worker_respawn_keeps_its_event_notifier() {
        let source = EventSource::new(CFRunLoop::get_current());
        let (commands, requests) = mpsc::sync_channel(1);
        let (results, events) = mpsc::sync_channel(1);
        let notifier = source.notifier();
        let wake = notifier.clone();
        let worker = thread::spawn(move || {
            let _results = TerminalSender::new(results, wake);
            requests.recv().unwrap();
        });
        let mut task = AudioPreparationTask::new(commands, events, worker);
        task.notifier = notifier;
        task.submit(1, AudioRecorder::new().finish().unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !task.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(matches!(
            task.poll_one(),
            Some(Some(AudioPreparationResult {
                result: Err(AudioPreparationError::Disconnected),
                ..
            }))
        ));
        let task = Rc::new(RefCell::new(task));
        let consumer = task.clone();
        let seen = Rc::new(Cell::new(false));
        let completed = seen.clone();
        source.set_handler(move || {
            if let Some(Some(result)) = consumer.borrow_mut().poll_one() {
                assert_eq!(result.generation, 2);
                assert_eq!(result.result, Ok(None));
                completed.set(true);
            }
        });
        source.attach();
        CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::ZERO,
            true,
        );
        task.borrow_mut()
            .submit(2, AudioRecorder::new().finish().unwrap())
            .unwrap();
        pump_until(|| seen.get());
    }

    #[test]
    fn cancelled_completion_notifies_and_releases_slot_without_poll_timer() {
        let source = EventSource::new(CFRunLoop::get_current());
        let (entered, entry) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut task = AudioPreparationTask::spawn_with_notifier(
            move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                None
            },
            source.notifier(),
        );
        task.submit(1, AudioRecorder::new().finish().unwrap())
            .unwrap();
        entry.recv_timeout(Duration::from_secs(1)).unwrap();
        task.cancel();
        let task = Rc::new(RefCell::new(task));
        let consumer = task.clone();
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        source.set_handler(move || {
            seen.set(seen.get() + 1);
            if let Some(output) = consumer.borrow_mut().poll_one() {
                assert!(output.is_none());
            }
        });
        source.attach();
        pump_until(|| calls.get() == 1);
        release.send(()).unwrap();
        pump_until(|| task.borrow().pending.is_none());
        assert!(
            calls.get() >= 2,
            "worker completion needed its own event wake"
        );
        task.borrow_mut()
            .submit(2, AudioRecorder::new().finish().unwrap())
            .unwrap();
        release.send(()).unwrap();
        source.close();
        task.borrow_mut().stop();
    }
}
