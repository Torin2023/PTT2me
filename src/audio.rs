use std::mem;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::constants::SAMPLE_RATE;

#[derive(Debug, PartialEq, Eq)]
pub enum AudioError {
    AlreadyRecording,
    NoInputDevice,
    DefaultInputConfig(String),
    BuildStream(String),
    StartStream(String),
    UnsupportedSampleFormat,
}

/// A short-lived microphone capture. It must stay on the AppKit main thread,
/// where its `cpal::Stream` is created and dropped.
pub struct AudioRecorder {
    samples: Arc<Mutex<Vec<f32>>>,
    stream: Option<cpal::Stream>,
    source_rate: u32,
    active: bool,
    #[cfg(test)]
    test_input: Option<TestInput>,
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
            samples: Arc::new(Mutex::new(Vec::new())),
            stream: None,
            source_rate: SAMPLE_RATE,
            active: false,
            #[cfg(test)]
            test_input: None,
        }
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        if self.active {
            return Err(AudioError::AlreadyRecording);
        }

        #[cfg(test)]
        if let Some(input) = &self.test_input {
            self.replace_samples(downmix(&input.frames, input.channels));
            self.source_rate = input.sample_rate;
            self.active = true;
            return Ok(());
        }

        let device = cpal::default_host()
            .default_input_device()
            .ok_or(AudioError::NoInputDevice)?;
        let supported_config = device
            .default_input_config()
            .map_err(|error| AudioError::DefaultInputConfig(error.to_string()))?;
        let sample_format = supported_config.sample_format();
        let config: cpal::StreamConfig = supported_config.into();

        self.replace_samples(Vec::new());
        self.source_rate = config.sample_rate.0;
        let channels = config.channels;
        let callback_samples = Arc::clone(&self.samples);
        let error_callback = |error| tracing::warn!(%error, "microphone input stream error");

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    append_frames(&callback_samples, data, channels, |sample| sample)
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    append_frames(&callback_samples, data, channels, normalize_i16)
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    append_frames(&callback_samples, data, channels, normalize_u16)
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::F64 => device.build_input_stream(
                &config,
                move |data: &[f64], _| {
                    append_frames(&callback_samples, data, channels, |sample| sample as f32)
                },
                error_callback,
                None,
            ),
            _ => return Err(AudioError::UnsupportedSampleFormat),
        }
        .map_err(|error| AudioError::BuildStream(error.to_string()))?;

        stream
            .play()
            .map_err(|error| AudioError::StartStream(error.to_string()))?;
        self.stream = Some(stream);
        self.active = true;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        self.stream.take();
        self.active = false;
        let samples = self.take_samples();
        Ok(prepare_capture(samples, self.source_rate))
    }

    pub fn abort(&mut self) {
        self.stream.take();
        self.active = false;
        self.replace_samples(Vec::new());
    }

    fn replace_samples(&self, samples: Vec<f32>) {
        if let Ok(mut stored) = self.samples.lock() {
            *stored = samples;
        }
    }

    fn take_samples(&self) -> Vec<f32> {
        self.samples
            .lock()
            .map(|mut samples| mem::take(&mut *samples))
            .unwrap_or_default()
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
}

impl Default for AudioRecorder {
    fn default() -> Self {
        Self::new()
    }
}

fn append_frames<T>(
    destination: &Arc<Mutex<Vec<f32>>>,
    data: &[T],
    channels: u16,
    convert: impl Fn(T) -> f32,
) where
    T: Copy,
{
    let channels = usize::from(channels);
    if channels == 0 {
        return;
    }

    let converted = data.iter().copied().map(convert).collect::<Vec<_>>();
    let mono = downmix(&converted, channels as u16);
    if let Ok(mut samples) = destination.lock() {
        samples.extend(mono);
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

fn downmix(samples: &[f32], channels: u16) -> Vec<f32> {
    let channels = usize::from(channels);
    if channels == 0 {
        return Vec::new();
    }

    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

fn prepare_capture(samples: Vec<f32>, source_rate: u32) -> Option<Vec<f32>> {
    (!samples.is_empty()).then(|| resample_linear(&samples, source_rate, SAMPLE_RATE))
}

fn resample_linear(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate == 0 || target_rate == 0 {
        return Vec::new();
    }
    if source_rate == target_rate {
        return samples.to_vec();
    }

    let output_len = samples.len() * target_rate as usize / source_rate as usize;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * source_rate as f64 / target_rate as f64;
            let left = position as usize;
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            samples[left] + (samples[right] - samples[left]) * fraction
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        downmix, normalize_i16, normalize_u16, prepare_capture, resample_linear, AudioError,
        AudioRecorder,
    };

    #[test]
    fn stereo_is_averaged_to_mono() {
        assert_eq!(downmix(&[1.0, -1.0, 0.5, 0.5], 2), vec![0.0, 0.5]);
    }

    #[test]
    fn resample_48k_to_16k_has_expected_length() {
        let input = vec![0.25; 48_000];
        assert_eq!(resample_linear(&input, 48_000, 16_000).len(), 16_000);
    }

    #[test]
    fn empty_capture_returns_none() {
        assert_eq!(prepare_capture(Vec::new(), 48_000), None);
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
}
