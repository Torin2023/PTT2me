# ASR performance measurement

PTT2me keeps the production GigaAM recognizer at two CPU threads. The
measurement below found a useful four-thread latency improvement on one Apple
M5, but the synthetic corpus and single machine do not establish recognition
quality or compatibility across supported Macs.

## Reproduce the measurement

Build the benchmark from the locked dependencies on Apple Silicon macOS. It
accepts an existing verified model and an explicit consented mono 16 kHz PCM16
WAV corpus; it never downloads either input.

```bash
if test -n "$(git status --porcelain=v1 --untracked-files=normal)"; then
  echo "benchmark source checkout must be clean" >&2
  exit 1
fi

export PTT2ME_BENCHMARK_BUILD_COMMIT="$(git rev-parse HEAD)"
export PTT2ME_BENCHMARK_BUILD_DIRTY=false
export PTT2ME_BENCHMARK_BUILD_RUSTC="$(rustc --version)"
export PTT2ME_BENCH_TARGET="$(mktemp -d /private/tmp/ptt2me-asr-benchmark.XXXXXX)"
export CARGO_TARGET_DIR="$PTT2ME_BENCH_TARGET"

cargo build --release --example asr_performance --locked

PTT2ME_MODEL_DIR=/path/to/gigaam-v3-rnnt-v1
PTT2ME_CORPUS_MANIFEST=/path/to/verified-manifest.json
PTT2ME_BENCH_OUTPUT=/private/tmp/ptt2me-asr-performance.json

DYLD_LIBRARY_PATH="$PTT2ME_BENCH_TARGET/release/examples" \
  "$PTT2ME_BENCH_TARGET/release/examples/asr_performance" \
  --model "$PTT2ME_MODEL_DIR" \
  --corpus "$PTT2ME_CORPUS_MANIFEST" \
  --consent-to-process-audio \
  --threads 1,2,4 \
  --warmups 1 \
  --repeats 5 > "$PTT2ME_BENCH_OUTPUT"
```

Run the same command once for each of `--threads 1`, `--threads 2`, and
`--threads 4` to obtain an independent process high-water RSS value. Keep raw
JSON and corpus audio outside Git. The tool emits no transcript, audio sample,
input path, or recognized-output digest.

An actual measurement fails closed unless the build command embeds a source
commit, dirty state, and compiler identity. Schema 2 records those compile-time
values under `provenance.build`, plus the SHA-256 and size of the executable
opened by the benchmark process under `provenance.executable`. The separate
`provenance.runtime_checkout` and `environment.runtime_rustc` fields describe
the run-time working directory and installed toolchain only; they are not
compiler or executable provenance.

The corpus manifest is bounded to 64 KiB and 16 cases. Every WAV is opened as
a non-symlink regular file, bounded to the production capture allowance,
checked against its declared SHA-256, and decoded only if it is mono 16 kHz
PCM16. Warmups are limited to 1..=3 and measured repeats to 1..=20.

## Recorded result

The historical run on 2026-09-06 used the earlier schema 1 tool. Its
`source_commit` value was the run-time working-directory HEAD
`5d8b4af2b3915195f035edba64028bc7832086cd`; that field alone cannot prove
which source produced the executable. The controlled task record places the
release build immediately after that commit and before the measurements. A
later observation of the retained 520,336-byte executable found SHA-256
`622da31414f6dd13c6e6fdbd843878d1a14972486064a9bb99251db08241fd74`, but
because schema 1 did not record an executable hash, this later observation is
not independent proof that every historical measurement used those bytes.

The machine was a Mac17,4 with Apple M5, 10 physical/logical CPUs, 16 GiB
memory, macOS 26.6.2 build 25G83, and a run-time Rust 1.94.0 observation. It
was on battery power with no recorded thermal or performance warning. Moderate
background GUI activity was present.

The fixed model was `gigaam-v3-rnnt-v1`; its embedded manifest SHA-256 was
`d012004c0706adafdcfa05677f0c10679ef844810e2ebc297f9dc9689150b239`.
Model-directory verification took 392.7 ms before the recognizer comparisons.
The synthetic Apple Milena 150 wpm corpus had four cases lasting 3.2243,
6.6672, 7.6346, and 20.6028 seconds. Its manifest SHA-256 was
`3a81dbea97f6167c3031f35269e7730db8d1aad29f7407fefae42d3a4e66d57d`.

One warmup and five measured repeats were run for every fixed case and CPU
configuration. p50 and p95 use nearest rank within the five repeats for that
case. CPU time is the user plus system delta for recognizer load, warmups, and
measured transcriptions in the shared comparison process. Peak RSS comes from
the corresponding single-configuration process.

| CPU threads | Native load (ms) | Configuration wall (ms) | CPU time (ms) | Peak RSS (MiB) |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 788.8 | 32,966.1 | 32,946.8 | 910.8 |
| 2 | 740.5 | 17,994.7 | 35,231.6 | 915.5 |
| 4 | 753.0 | 10,943.3 | 41,497.2 | 916.3 |

| CPU threads | Case | p50 (ms) | p95 (ms) | p50 RTF | p95 RTF |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | short | 455.7 | 456.1 | 0.1413 | 0.1414 |
| 1 | numbers | 925.4 | 929.9 | 0.1388 | 0.1395 |
| 1 | names | 1,062.8 | 1,068.8 | 0.1392 | 0.1400 |
| 1 | long | 2,932.6 | 2,939.4 | 0.1423 | 0.1427 |
| 2 | short | 246.4 | 246.7 | 0.0764 | 0.0765 |
| 2 | numbers | 497.6 | 498.3 | 0.0746 | 0.0747 |
| 2 | names | 568.8 | 570.6 | 0.0745 | 0.0747 |
| 2 | long | 1,561.4 | 1,571.4 | 0.0758 | 0.0763 |
| 4 | short | 147.5 | 149.8 | 0.0457 | 0.0464 |
| 4 | numbers | 291.4 | 297.2 | 0.0437 | 0.0446 |
| 4 | names | 334.1 | 340.5 | 0.0438 | 0.0446 |
| 4 | long | 916.2 | 928.3 | 0.0445 | 0.0451 |

The production 16 kHz preparation fast path took less than 0.001 ms per case.
This measures the production resampler fast path after WAV decoding; it does
not measure microphone ring drain or 44.1/48 kHz resampling. The benchmark is
direct in-process recognition and does not include the product worker/process
transport.

Full trimmed recognizer output matched exactly in memory across every warmup,
repeat, and CPU configuration. Four threads reduced configuration wall time by
about 39.2% relative to two threads on this machine, while using about 17.8%
more CPU time and 0.8 MiB more peak RSS. This is useful experiment evidence,
but exact output equality on four synthetic phrases is not human-corpus WER.
Two CPU threads therefore remain the supported production default.

## Runtime phase diagnostics

Debug diagnostics record only `phase`, integer `elapsed_ms`, and `outcome`.
They never include recognized text, audio, content hashes, paths, or arbitrary
target/window metadata.

| Phase | Measured scope |
| --- | --- |
| `audio_preparation` | Background production capture preparation |
| `asr_worker_load_round_trip` | Load command submission through worker result |
| `asr_worker_transcription_round_trip` | Transcription command submission through worker result |
| `insertion_target_snapshot_preparation` | Initial target validation and clipboard snapshot preparation |
| `insertion_pre_command_v_security_probe` | Existing security recheck immediately before Command-V |
| `clipboard_restoration` | Existing clipboard restoration attempt |

These timings preserve the existing TCC, secure-field, pasteboard ownership,
and restoration flow and add no probes solely for metrics.

## Limits and next experiment

The result describes one short synthetic run on one M5 while other GUI
processes were active. Five repeats do not estimate population tail latency,
synthetic speech does not establish human recognition accuracy, and the run
does not cover older supported Apple Silicon Macs or battery impact.

A future experiment can compare the current CPU backend with a new runtime or
CoreML on the same consented human corpus and supported-device matrix. It must
hold model and text normalization constant, report WER and latency together,
measure energy/thermal behavior, and preserve full-output evidence. No engine
or model switch is supported without that quality and compatibility evidence.
