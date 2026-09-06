# Audio conversion and recognition quality

GigaAM receives mono floating-point PCM at 16 kHz. The microphone callback
downmixes native-rate frames into the existing bounded ring buffer. After
capture stops, `src/audio/resampler.rs` converts that complete recording.
The callback does not allocate filters or perform convolution.

The old linear interpolator did not remove frequencies above the new Nyquist
limit. For example, 48 kHz input containing a 12 kHz tone became a 4 kHz tone
at 16 kHz, with no attenuation. Such aliases can contaminate speech features.

## Conversion contract

- A centered Blackman-windowed sinc performs low-pass filtering and sample
  interpolation together. The cutoff is 0.9375 times the lower Nyquist limit:
  7.5 kHz when downsampling to 16 kHz. The half-width is 48 samples at the
  lower rate (3 ms for downsampling to 16 kHz), scaled to the device rate.
- Rational source positions avoid accumulated timing drift. A filter kernel
  is reused for each repeating fractional phase: one phase at 48/96 kHz,
  160 at 44.1 kHz. Only one kernel is stored at a time. No dependency is added.
- Each phase has unity DC gain. Constant endpoint extension preserves levels
  for short recordings; it affects only the filter support near each edge.
- The output contains `floor(input_length * 16000 / input_rate)` samples.
  Centering compensates filter delay without appending a tail. No speech is
  trimmed by a silence detector. A capture shorter than one output sample
  returns no audio for recognition.
- Native 16 kHz samples and their allocation pass through unchanged.
  There is no automatic gain, noise gate, peak normalization, or hard clipping.
  Filtered transients can overshoot the original peaks.
- Low-rate devices are interpolated with a cutoff below their own Nyquist
  limit. Converting an 8 kHz microphone to 16 kHz cannot recover the missing
  high-frequency speech information.

This follows the usual low-pass FIR/polyphase approach to rational resampling;
see the [SciPy resample_poly documentation](https://docs.scipy.org/doc/scipy/reference/generated/scipy.signal.resample_poly.html)
for background. PTT2me's implementation has its own duration and edge policy.

## Automated signal checks

The existing macOS CI runs these tests without microphones or model assets:

- Waveform error below 0.1% RMS for test tones at 100 Hz, 1/4/6/7 kHz, covering
  22.05, 44.1, 47.999, 48, 96 and 192 kHz input. This detects phase as well as
  level errors. A separate quiet-tone test checks that low levels are preserved.
- At least 60 dB RMS rejection of test tones at 8/8.1/8.5/9/12/15/20 kHz for
  44.1, 48 and 96 kHz input. These are sampled frequencies, not a continuous
  stopband measurement. Steady-state tests exclude endpoint transients.
- A mixture of three speech-band tones survives a louder 12 kHz interferer
  with less than 0.1% relative RMS error.
- Low-rate interpolation, exact silence, unchanged native-rate samples,
  constant levels at both edges, short/empty recordings and unusual 47,999 Hz
  input preserve their expected contracts.
- An impulse stays at the expected time. The recorder integration test
  verifies that a 12 kHz interferer is filtered before audio reaches ASR.

To run the focused checks on an Apple Silicon Mac:

```bash
cargo test --features test-support audio:: -- --test-threads=1
```

## Recognition comparison before release

Signal tests establish cleaner conversion, not a measured improvement in
GigaAM word error rate (WER). Use the same consented recordings and fixed
model/decoder configuration for the old and new conversion paths:

1. Retain native-rate recordings and verbatim transcripts for a fixed Russian
   corpus. Include quiet speech, room noise, names, numbers, sibilants, quiet
   endings, and phrases near the 25-second capture limit. Cover the microphones
   and native rates actually used; do not rerecord the phrases for each version.
2. Transcribe every recording with both paths. Apply the same documented text
   normalization to reference and hypothesis (case, punctuation, numbers and
   `ё`/`е`), then report corpus WER as `(substitutions + deletions + insertions)
   / reference words`, along with counts and per-condition results. Keep raw
   hypotheses so a lower average cannot hide lost endings or damaged names.
3. Listen to converted samples around both endpoints and loud transients.
   Check there are no missing syllables, clipping artifacts or audible clicks.
4. Measure conversion and end-to-text latency in an optimized build for 1-,
   10- and 25-second recordings at 44.1/48/96 kHz. Conversion currently runs
   after stopping the stream on the AppKit thread; verify menu responsiveness
   on the slowest supported Mac before release.
5. Record the tested commit, model, microphones, corpus size, WER counts and
   latency distribution. Investigate regressions by recording/condition before
   claiming a recognition-quality improvement.

No speech recordings, model assets, or recognized user text are logged or
committed by this change. Manual listening, WER and latency results must be
reported separately from automated macOS test results.
