//! Offline, band-limited conversion of the completed mono capture.
//!
//! A centered Blackman-windowed sinc filters before decimation and interpolates
//! fractional positions in one operation. Rational phases reuse their kernel;
//! only one kernel is stored, including for unusual device sample rates.

use std::f64::consts::PI;

// At 16 kHz the cutoff is 7.5 kHz, leaving a transition band between speech
// through 7 kHz and the 8 kHz Nyquist limit. Scale the support with the rate
// ratio so higher-rate microphones receive the same anti-alias protection.
const CUTOFF_RATIO: f64 = 0.9375;
const HALF_WIDTH: f64 = 48.0;

pub(super) fn resample(samples: Vec<f32>, source_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate == 0 || target_rate == 0 {
        return Vec::new();
    }
    if source_rate == target_rate {
        return samples;
    }

    // Preserve the capture duration to within one output sample, with no
    // appended filter tail or group delay. Captures are bounded by the recorder.
    let output_len =
        (samples.len() as u64 * u64::from(target_rate) / u64::from(source_rate)) as usize;
    if output_len == 0 {
        return Vec::new();
    }

    let divisor = gcd(source_rate, target_rate);
    let phase_count = (target_rate / divisor) as usize;
    let source_step = (source_rate / divisor) as usize;
    let ratio = (f64::from(target_rate) / f64::from(source_rate)).min(1.0);
    let half_width = HALF_WIDTH / ratio;
    let radius = half_width.ceil() as usize;
    let mut kernel = vec![0.0; 2 * radius + 1];
    let mut output = vec![0.0; output_len];

    // Every phase_count outputs the fractional source position repeats. Integer
    // positioning avoids cumulative drift at rates such as 44.1 kHz. Kernel
    // construction happens once per phase, not once per output sample.
    for phase in 0..phase_count.min(output_len) {
        let position = phase * source_step;
        let mut center = position / phase_count;
        let fraction = (position % phase_count) as f64 / phase_count as f64;
        make_kernel(&mut kernel, radius, fraction, ratio, half_width);

        for destination in output.iter_mut().skip(phase).step_by(phase_count) {
            let value = if center >= radius && center + radius < samples.len() {
                samples[center - radius..=center + radius]
                    .iter()
                    .zip(&kernel)
                    .map(|(&sample, &weight)| f64::from(sample) * weight)
                    .sum::<f64>()
            } else {
                // Constant extension keeps DC and very short captures intact.
                // Unlike zero padding, it does not create a step at a nonzero
                // endpoint. Only the filter support near each edge is affected.
                kernel
                    .iter()
                    .enumerate()
                    .map(|(tap, &weight)| {
                        let source = (center + tap).saturating_sub(radius).min(samples.len() - 1);
                        f64::from(samples[source]) * weight
                    })
                    .sum::<f64>()
            };
            // No peak normalization, noise gate or clipping: preserve levels
            // and quiet speech. A band-limited signal can overshoot its input.
            *destination = value as f32;
            center += source_step;
        }
    }
    output
}

fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn make_kernel(kernel: &mut [f64], radius: usize, fraction: f64, ratio: f64, half_width: f64) {
    let cutoff = CUTOFF_RATIO * ratio;
    for (tap, weight) in kernel.iter_mut().enumerate() {
        let distance = tap as f64 - radius as f64 - fraction;
        if distance.abs() > half_width {
            *weight = 0.0;
            continue;
        }
        let argument = PI * cutoff * distance;
        let sinc = if argument.abs() < f64::EPSILON {
            1.0
        } else {
            argument.sin() / argument
        };
        let window_position = PI * distance / half_width;
        let window = 0.42 + 0.5 * window_position.cos() + 0.08 * (2.0 * window_position).cos();
        *weight = cutoff * sinc * window;
    }
    // Unity DC gain for every fractional phase avoids level modulation.
    let sum: f64 = kernel.iter().sum();
    for weight in kernel {
        *weight /= sum;
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::resample;

    const TARGET_RATE: u32 = 16_000;
    // Exclude endpoint extension when measuring the steady-state response.
    const EDGE: usize = 160;

    fn tone(rate: u32, length: usize, frequency: f64) -> Vec<f32> {
        (0..length)
            .map(|index| (TAU * frequency * index as f64 / f64::from(rate) + 0.37).cos() as f32)
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn relative_error(actual: &[f32], expected: &[f32]) -> f64 {
        assert_eq!(actual.len(), expected.len());
        let actual = &actual[EDGE..actual.len() - EDGE];
        let expected = &expected[EDGE..expected.len() - EDGE];
        let error: Vec<_> = actual.iter().zip(expected).map(|(a, b)| a - b).collect();
        rms(&error) / rms(expected)
    }

    #[test]
    fn empty_or_invalid_rate_returns_no_samples() {
        assert!(resample(Vec::new(), 48_000, TARGET_RATE).is_empty());
        assert!(resample(vec![1.0], 0, TARGET_RATE).is_empty());
        assert!(resample(vec![1.0], 48_000, 0).is_empty());
    }

    #[test]
    fn native_16k_keeps_samples_and_allocation() {
        let input = vec![-1.0, -0.001, 0.0, 0.25, 1.0];
        let pointer = input.as_ptr();
        let output = resample(input, TARGET_RATE, TARGET_RATE);
        assert_eq!(output, [-1.0, -0.001, 0.0, 0.25, 1.0]);
        assert_eq!(output.as_ptr(), pointer);
    }

    #[test]
    fn duration_and_dc_are_preserved_including_short_captures() {
        for rate in [
            8_000, 16_000, 22_050, 32_000, 44_100, 47_999, 48_000, 96_000, 192_000,
        ] {
            for length in [1, 2, 3, 7, 257, rate as usize / 10 + 1] {
                let output = resample(vec![0.25; length], rate, TARGET_RATE);
                assert_eq!(output.len(), length * TARGET_RATE as usize / rate as usize);
                assert!(output.iter().all(|sample| (*sample - 0.25).abs() < 1e-6));
            }
        }
    }

    #[test]
    fn silence_stays_exactly_silent() {
        for rate in [8_000, 44_100, 48_000, 96_000] {
            let output = resample(vec![0.0; rate as usize / 10], rate, TARGET_RATE);
            assert!(output.iter().all(|&sample| sample == 0.0));
        }
    }

    #[test]
    fn speech_band_keeps_waveform_and_level() {
        for rate in [22_050, 44_100, 47_999, 48_000, 96_000, 192_000] {
            for frequency in [100.0, 1_000.0, 4_000.0, 6_000.0, 7_000.0] {
                let input = tone(rate, rate as usize / 10, frequency);
                let output = resample(input, rate, TARGET_RATE);
                let expected = tone(TARGET_RATE, output.len(), frequency);
                let error = relative_error(&output, &expected);
                assert!(error < 0.001, "{rate} Hz, {frequency} Hz: error {error}");
            }
        }
    }

    #[test]
    fn quiet_speech_keeps_its_level() {
        let input = tone(48_000, 4_800, 1_000.0)
            .into_iter()
            .map(|sample| sample * 0.00001)
            .collect();
        let output = resample(input, 48_000, TARGET_RATE);
        let expected: Vec<_> = tone(TARGET_RATE, output.len(), 1_000.0)
            .into_iter()
            .map(|sample| sample * 0.00001)
            .collect();
        assert!(relative_error(&output, &expected) < 0.001);
    }

    #[test]
    fn above_nyquist_tones_are_rejected_by_at_least_60_db() {
        for rate in [44_100, 48_000, 96_000] {
            for frequency in [
                8_000.0, 8_100.0, 8_500.0, 9_000.0, 12_000.0, 15_000.0, 20_000.0,
            ] {
                let input = tone(rate, rate as usize / 10, frequency);
                let input_rms = rms(&input);
                let output = resample(input, rate, TARGET_RATE);
                let ratio = rms(&output[EDGE..output.len() - EDGE]) / input_rms;
                assert!(ratio < 0.001, "{rate} Hz, {frequency} Hz: ratio {ratio}");
            }
        }
    }

    #[test]
    fn mixed_speech_band_survives_loud_out_of_band_noise() {
        for rate in [44_100, 48_000, 96_000] {
            let mix = |sample_rate, length| {
                let low = tone(sample_rate, length, 300.0);
                let mid = tone(sample_rate, length, 3_000.0);
                let high = tone(sample_rate, length, 6_800.0);
                low.iter()
                    .zip(mid)
                    .zip(high)
                    .map(|((low, mid), high)| 0.15 * low + 0.1 * mid + 0.05 * high)
                    .collect::<Vec<_>>()
            };
            let length = rate as usize / 10;
            let mut input = mix(rate, length);
            for (sample, noise) in input.iter_mut().zip(tone(rate, length, 12_000.0)) {
                *sample += 0.4 * noise;
            }
            let output = resample(input, rate, TARGET_RATE);
            let expected = mix(TARGET_RATE, output.len());
            assert!(relative_error(&output, &expected) < 0.001);
        }
    }

    #[test]
    fn low_rate_microphones_preserve_band_limited_waveforms() {
        for (rate, frequency) in [(8_000, 3_000.0), (11_025, 4_000.0)] {
            let input = tone(rate, rate as usize / 10, frequency);
            let output = resample(input, rate, TARGET_RATE);
            let expected = tone(TARGET_RATE, output.len(), frequency);
            assert!(relative_error(&output, &expected) < 0.001);
        }
    }

    #[test]
    fn centered_filter_does_not_delay_the_phrase() {
        for rate in [44_100, 48_000, 96_000] {
            let mut input = vec![0.0; rate as usize / 10];
            input[rate as usize / 20] = 1.0;
            let output = resample(input, rate, TARGET_RATE);
            let peak = output
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
                .unwrap()
                .0;
            assert_eq!(peak, TARGET_RATE as usize / 20);
        }
    }
}
