use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::constants::{CAPTURE_BUFFER_MARGIN_MS, MAX_CAPTURE_MS, RELEASE_GRACE_MS, SAMPLE_RATE};

mod resampler;

#[derive(Debug, PartialEq, Eq)]
pub enum AudioError {
    AlreadyRecording,
    NoInputDevice,
    DefaultInputConfig(String),
    BuildStream(String),
    StartStream(String),
    UnsupportedSampleFormat,
    StreamCallbackFailed,
    BufferOverflow,
}

#[derive(Default)]
struct CallbackFailures {
    next_generation: AtomicU64,
    active_generation: AtomicU64,
    failed_generation: AtomicU64,
}

impl CallbackFailures {
    fn begin(&self) -> u64 {
        let generation = self
            .next_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .max(1);
        self.failed_generation.store(0, Ordering::Release);
        self.active_generation.store(generation, Ordering::Release);
        generation
    }

    fn report(&self, generation: u64) {
        if self.active_generation.load(Ordering::Acquire) == generation {
            self.failed_generation.store(generation, Ordering::Release);
        }
    }

    fn finish(&self, generation: u64) -> bool {
        let active = self.active_generation.swap(0, Ordering::AcqRel);
        let failed = self.failed_generation.swap(0, Ordering::AcqRel);
        active == generation && failed == generation
    }
}

/// Frozen capture: no stream, callback flags or main-thread-only owners cross
/// the preparation boundary. The producer has stopped before this is created.
pub(crate) struct CompletedCapture {
    consumer: Option<Consumer<f32>>,
    source_rate: u32,
}

impl CompletedCapture {
    pub(crate) fn native_sample_count(&self) -> usize {
        self.consumer.as_ref().map_or(0, Consumer::slots)
    }

    pub(crate) fn prepare(self) -> Option<Vec<f32>> {
        let mut samples = Vec::with_capacity(self.native_sample_count());
        if let Some(mut consumer) = self.consumer {
            while let Ok(sample) = consumer.pop() {
                samples.push(sample);
            }
        }
        prepare_capture(samples, self.source_rate)
    }
}

/// A short-lived microphone capture. It must stay on the AppKit main thread,
/// where its `cpal::Stream` is created and dropped.
pub struct AudioRecorder {
    consumer: Option<Consumer<f32>>,
    stream: Option<cpal::Stream>,
    source_rate: u32,
    active: bool,
    active_generation: Option<u64>,
    callback_failures: Arc<CallbackFailures>,
    overflowed: Arc<AtomicBool>,
    #[cfg(test)]
    test_input: Option<TestInput>,
    #[cfg(test)]
    test_capacity: Option<usize>,
}

#[cfg(test)]
struct TestInput {
    frames: Vec<f32>,
    sample_rate: u32,
    channels: u16,
}

impl AudioRecorder {
    pub fn new() -> Self {
        Self {
            consumer: None,
            stream: None,
            source_rate: SAMPLE_RATE,
            active: false,
            active_generation: None,
            callback_failures: Arc::new(CallbackFailures::default()),
            overflowed: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            test_input: None,
            #[cfg(test)]
            test_capacity: None,
        }
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        if self.active {
            return Err(AudioError::AlreadyRecording);
        }
        let generation = self.callback_failures.begin();
        self.active_generation = Some(generation);
        self.overflowed.store(false, Ordering::Release);

        #[cfg(test)]
        if let Some(input) = &self.test_input {
            let capacity = self
                .test_capacity
                .unwrap_or_else(|| capture_capacity(input.sample_rate));
            let (mut producer, consumer) = capture_buffer(capacity);
            append_frames(
                &mut producer,
                &input.frames,
                input.channels,
                |sample| sample,
                &self.overflowed,
            );
            self.consumer = Some(consumer);
            self.source_rate = input.sample_rate;
            self.active = true;
            return Ok(());
        }

        let result = (|| {
            let device = cpal::default_host()
                .default_input_device()
                .ok_or(AudioError::NoInputDevice)?;
            let supported_config = device
                .default_input_config()
                .map_err(|error| AudioError::DefaultInputConfig(error.to_string()))?;
            let sample_format = supported_config.sample_format();
            let config: cpal::StreamConfig = supported_config.into();

            self.consumer = None;
            self.source_rate = config.sample_rate.0;
            let channels = config.channels;
            let capacity = capture_capacity(self.source_rate);

            let context = || CaptureStreamContext {
                channels,
                overflowed: Arc::clone(&self.overflowed),
                callback_failures: Arc::clone(&self.callback_failures),
                generation,
            };
            let (stream, consumer) = match sample_format {
                cpal::SampleFormat::F32 => {
                    build_capture_stream(&device, &config, capacity, context(), |sample: f32| {
                        sample
                    })
                }
                cpal::SampleFormat::I16 => {
                    build_capture_stream(&device, &config, capacity, context(), normalize_i16)
                }
                cpal::SampleFormat::U16 => {
                    build_capture_stream(&device, &config, capacity, context(), normalize_u16)
                }
                cpal::SampleFormat::F64 => {
                    build_capture_stream(&device, &config, capacity, context(), |sample: f64| {
                        sample as f32
                    })
                }
                _ => return Err(AudioError::UnsupportedSampleFormat),
            }
            .map_err(|error| AudioError::BuildStream(error.to_string()))?;

            stream
                .play()
                .map_err(|error| AudioError::StartStream(error.to_string()))?;
            self.consumer = Some(consumer);
            self.stream = Some(stream);
            self.active = true;
            Ok(())
        })();

        if result.is_err() {
            self.active_generation = None;
            self.callback_failures.finish(generation);
            self.consumer = None;
        }
        result
    }

    /// Synchronous compatibility for smoke callers and signal-level tests.
    pub fn stop(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        self.finish().map(CompletedCapture::prepare)
    }

    /// Stop/drop CPAL on the caller (AppKit) thread, without draining or filtering.
    pub(crate) fn finish(&mut self) -> Result<CompletedCapture, AudioError> {
        stop_stream(&mut self.stream);
        self.active = false;
        let callback_failed = self
            .active_generation
            .take()
            .is_some_and(|generation| self.callback_failures.finish(generation));
        let overflowed = self.overflowed.swap(false, Ordering::AcqRel);
        let consumer = self.consumer.take();
        if callback_failed {
            return Err(AudioError::StreamCallbackFailed);
        }
        if overflowed {
            return Err(AudioError::BufferOverflow);
        }
        Ok(CompletedCapture {
            consumer,
            source_rate: self.source_rate,
        })
    }

    pub fn abort(&mut self) {
        stop_stream(&mut self.stream);
        self.active = false;
        if let Some(generation) = self.active_generation.take() {
            self.callback_failures.finish(generation);
        }
        self.consumer = None;
        self.overflowed.store(false, Ordering::Release);
    }

    #[cfg(test)]
    fn with_test_frames(frames: Vec<f32>, sample_rate: u32, channels: u16) -> Self {
        Self {
            test_input: Some(TestInput {
                frames,
                sample_rate,
                channels,
            }),
            ..Self::new()
        }
    }

    #[cfg(test)]
    fn with_test_frames_and_capacity(
        frames: Vec<f32>,
        sample_rate: u32,
        channels: u16,
        capacity: usize,
    ) -> Self {
        Self {
            test_capacity: Some(capacity),
            ..Self::with_test_frames(frames, sample_rate, channels)
        }
    }
}

impl Default for AudioRecorder {
    fn default() -> Self {
        Self::new()
    }
}

// Keep destruction synchronous and shared by finish/abort. Generic ownership
// permits testing the drop-thread boundary without opening a real microphone.
fn stop_stream<T>(stream: &mut Option<T>) {
    drop(stream.take());
}

fn capture_buffer(capacity: usize) -> (Producer<f32>, Consumer<f32>) {
    RingBuffer::new(capacity.max(1))
}

fn capture_capacity(source_rate: u32) -> usize {
    let buffered_ms = MAX_CAPTURE_MS + RELEASE_GRACE_MS + CAPTURE_BUFFER_MARGIN_MS;
    let frames = u64::from(source_rate) * buffered_ms / 1_000;
    usize::try_from(frames).unwrap_or(usize::MAX).max(1)
}

struct CaptureStreamContext {
    channels: u16,
    overflowed: Arc<AtomicBool>,
    callback_failures: Arc<CallbackFailures>,
    generation: u64,
}

fn build_capture_stream<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    capacity: usize,
    context: CaptureStreamContext,
    convert: F,
) -> Result<(cpal::Stream, Consumer<f32>), cpal::BuildStreamError>
where
    T: cpal::SizedSample + Copy,
    F: Fn(T) -> f32 + Copy + Send + 'static,
{
    let (mut producer, consumer) = capture_buffer(capacity);
    let channels = context.channels;
    let overflowed = context.overflowed;
    let callback_failures = context.callback_failures;
    let generation = context.generation;
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            append_frames(&mut producer, data, channels, convert, &overflowed);
        },
        move |_error| callback_failures.report(generation),
        None,
    )?;
    Ok((stream, consumer))
}

fn append_frames<T>(
    destination: &mut Producer<f32>,
    data: &[T],
    channels: u16,
    convert: impl Fn(T) -> f32,
    overflowed: &AtomicBool,
) where
    T: Copy,
{
    let channels = usize::from(channels);
    if channels == 0 {
        return;
    }

    for frame in data.chunks_exact(channels) {
        let mono = frame.iter().copied().map(&convert).sum::<f32>() / channels as f32;
        if destination.push(mono).is_err() {
            overflowed.store(true, Ordering::Release);
            return;
        }
    }
}

fn normalize_i16(sample: i16) -> f32 {
    if sample < 0 {
        sample as f32 / -(i16::MIN as f32)
    } else {
        sample as f32 / i16::MAX as f32
    }
}

fn normalize_u16(sample: u16) -> f32 {
    const MIDPOINT: u16 = 1 << 15;

    if sample < MIDPOINT {
        (sample as f32 - MIDPOINT as f32) / MIDPOINT as f32
    } else {
        (sample - MIDPOINT) as f32 / (u16::MAX - MIDPOINT) as f32
    }
}

fn prepare_capture(samples: Vec<f32>, source_rate: u32) -> Option<Vec<f32>> {
    // Capture has stopped: no filtering or allocation runs in the CPAL callback.
    let samples = resampler::resample(samples, source_rate, SAMPLE_RATE);
    (!samples.is_empty()).then_some(samples)
}

#[cfg(test)]
mod tests {
    use super::{
        append_frames, capture_buffer, capture_capacity, normalize_i16, normalize_u16,
        prepare_capture, AudioError, AudioRecorder,
    };
    use crate::runtime::capture_result_event;
    use crate::state::{AppController, AppEvent, AppStatus, Effect, PermissionSnapshot};

    fn recognizing_controller() -> AppController {
        let mut controller = AppController::new();
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        controller.handle(AppEvent::TriggerPressed);
        controller.handle(AppEvent::TriggerReleased { short: false });
        assert_eq!(controller.status(), &AppStatus::Recognizing);
        controller
    }

    #[test]
    fn stream_stop_drops_owner_on_caller_thread_before_preparation() {
        struct DropProbe(std::sync::mpsc::Sender<std::thread::ThreadId>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.send(std::thread::current().id()).unwrap();
            }
        }
        let (stopped, stop_events) = std::sync::mpsc::channel();
        let mut stream = Some(DropProbe(stopped));
        super::stop_stream(&mut stream);
        assert!(stream.is_none());
        assert_eq!(stop_events.try_recv().unwrap(), std::thread::current().id());
    }

    #[test]
    fn background_preparation_matches_synchronous_signal_contract_exactly() {
        for rate in [8_000, 16_000, 44_100, 48_000, 96_000] {
            let frames: Vec<_> = (0..rate / 20)
                .flat_map(|i| {
                    let sample = ((std::f64::consts::TAU * 1500.0 * f64::from(i) / f64::from(rate))
                        .sin()
                        * 0.0001
                        + 0.125) as f32;
                    [sample, sample * 0.5]
                })
                .collect();
            let mut sync = AudioRecorder::with_test_frames(frames.clone(), rate, 2);
            let mut background = AudioRecorder::with_test_frames(frames, rate, 2);
            sync.start().unwrap();
            background.start().unwrap();
            let expected = sync.stop().unwrap();
            let completed = background.finish().unwrap();
            let actual = std::thread::spawn(move || completed.prepare())
                .join()
                .unwrap();
            assert_eq!(actual, expected, "different PCM at {rate} Hz");
        }
    }

    #[test]
    fn finish_transfers_undrained_capture_and_freezes_callback_outcome() {
        let mut recorder = AudioRecorder::with_test_frames(vec![0.25; 480], 48_000, 1);
        recorder.start().unwrap();
        let old_generation = recorder.active_generation.unwrap();
        let completed = recorder.finish().unwrap();
        assert!(!recorder.active);
        assert!(recorder.consumer.is_none());
        assert_eq!(completed.native_sample_count(), 480);
        recorder.start().unwrap();
        recorder.callback_failures.report(old_generation);
        let samples = std::thread::spawn(move || completed.prepare())
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(samples, prepare_capture(vec![0.25; 480], 48_000).unwrap());
        assert!(recorder.stop().unwrap().is_some());
    }

    #[test]
    fn finish_discards_failed_buffers_before_background_transfer() {
        let mut recorder =
            AudioRecorder::with_test_frames_and_capacity(vec![0.25; 3], 48_000, 1, 2);
        recorder.start().unwrap();
        assert!(matches!(recorder.finish(), Err(AudioError::BufferOverflow)));
        assert!(recorder.consumer.is_none());
        recorder.start().unwrap();
        recorder
            .callback_failures
            .report(recorder.active_generation.unwrap());
        assert!(matches!(
            recorder.finish(),
            Err(AudioError::StreamCallbackFailed)
        ));
        assert!(recorder.consumer.is_none());
    }

    #[test]
    fn resample_48k_to_16k_has_expected_length() {
        let input = vec![0.25; 48_000];
        assert_eq!(prepare_capture(input, 48_000).unwrap().len(), 16_000);
    }

    #[test]
    fn stop_filters_high_frequency_noise_before_recognition() {
        let frames = (0..4_800)
            .map(|index| (std::f64::consts::TAU * 12_000.0 * index as f64 / 48_000.0).cos() as f32)
            .collect();
        let mut recorder = AudioRecorder::with_test_frames(frames, 48_000, 1);
        recorder.start().unwrap();

        let samples = recorder.stop().unwrap().unwrap();

        assert_eq!(samples.len(), 1_600);
        assert!(samples[160..1_440]
            .iter()
            .all(|sample| sample.abs() < 0.001));
    }

    #[test]
    fn capture_capacity_covers_limit_release_grace_and_scheduling_margin() {
        assert_eq!(capture_capacity(48_000), 1_256_640);
        assert_eq!(capture_capacity(96_000), 2_513_280);
    }

    #[test]
    fn bounded_callback_downmixes_in_one_pass_and_reports_overflow() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (mut producer, mut consumer) = capture_buffer(2);
        let overflowed = AtomicBool::new(false);

        append_frames(
            &mut producer,
            &[1.0_f32, 3.0, 5.0, 7.0],
            2,
            |sample| sample,
            &overflowed,
        );
        assert_eq!(consumer.pop(), Ok(2.0));
        assert_eq!(consumer.pop(), Ok(6.0));
        assert!(!overflowed.load(Ordering::Acquire));

        append_frames(
            &mut producer,
            &[1.0_f32, 3.0, 5.0, 7.0, 9.0, 11.0],
            2,
            |sample| sample,
            &overflowed,
        );
        assert!(overflowed.load(Ordering::Acquire));
    }

    #[test]
    fn overflow_is_a_distinct_capture_error() {
        let mut recorder =
            AudioRecorder::with_test_frames_and_capacity(vec![0.25; 3], 48_000, 1, 2);
        recorder.start().unwrap();

        assert_eq!(recorder.stop(), Err(AudioError::BufferOverflow));
    }

    #[test]
    fn empty_capture_returns_none() {
        assert_eq!(prepare_capture(Vec::new(), 48_000), None);
        assert_eq!(prepare_capture(vec![0.25], 48_000), None);
    }

    #[test]
    fn signed_16_bit_samples_are_normalized() {
        assert_eq!(normalize_i16(i16::MIN), -1.0);
        assert_eq!(normalize_i16(i16::MAX), 1.0);
    }

    #[test]
    fn unsigned_16_bit_samples_are_normalized() {
        assert_eq!(normalize_u16(0), -1.0);
        assert_eq!(normalize_u16(u16::MAX), 1.0);
    }

    #[test]
    fn abort_discards_pending_frames() {
        let mut recorder = AudioRecorder::with_test_frames(vec![0.25; 48_000], 48_000, 1);
        recorder.start().unwrap();

        recorder.abort();

        assert_eq!(recorder.stop().unwrap(), None);
    }

    #[test]
    fn second_start_is_rejected() {
        let mut recorder = AudioRecorder::with_test_frames(vec![0.25], 48_000, 1);
        recorder.start().unwrap();

        assert_eq!(recorder.start(), Err(AudioError::AlreadyRecording));
    }

    #[test]
    fn stop_returns_resampled_mono_once() {
        let mut frames = Vec::with_capacity(96_000);
        for _ in 0..48_000 {
            frames.extend([1.0, -1.0]);
        }
        let mut recorder = AudioRecorder::with_test_frames(frames, 48_000, 2);
        recorder.start().unwrap();

        let samples = recorder.stop().unwrap().unwrap();

        assert_eq!(samples.len(), 16_000);
        assert!(samples.iter().all(|sample| *sample == 0.0));
        assert_eq!(recorder.stop().unwrap(), None);
    }

    #[test]
    fn callback_failure_discards_partial_audio_and_never_requests_recognition() {
        let mut recorder = AudioRecorder::with_test_frames(vec![0.25; 48_000], 48_000, 1);
        recorder.start().unwrap();
        let generation = recorder.active_generation.unwrap();
        recorder.callback_failures.report(generation);

        let stop_result = recorder.stop();

        assert_eq!(stop_result, Err(AudioError::StreamCallbackFailed));
        let mut controller = recognizing_controller();
        let effects = controller.handle(capture_result_event(stop_result));
        assert!(effects
            .iter()
            .all(|effect| !matches!(effect, Effect::Recognize(_))));
        assert!(matches!(
            controller.status(),
            AppStatus::Error {
                recoverable: true,
                ..
            }
        ));
    }

    #[test]
    fn stale_callback_failure_does_not_poison_later_capture() {
        let mut recorder = AudioRecorder::with_test_frames(vec![0.25; 48_000], 48_000, 1);
        recorder.start().unwrap();
        let stale_generation = recorder.active_generation.unwrap();
        recorder.abort();

        recorder.start().unwrap();
        recorder.callback_failures.report(stale_generation);

        let samples = recorder.stop().unwrap().unwrap();
        assert_eq!(samples.len(), 16_000);
    }
}
