use std::fs::File;
use std::time::Instant;

use super::args::Arguments;
use super::corpus;
use super::report::{
    self, BenchmarkReport, CasePerformance, ConfigurationReport, CorpusCaseReport, CorpusReport,
    CpuDelta, EqualityReport, MethodReport, ModelReport, Quantiles,
};

struct PreparedCase {
    id: String,
    source: String,
    duration_seconds: f64,
    preparation_ms: f64,
    samples: Vec<f32>,
}

pub(super) fn execute(arguments: Arguments) -> Result<(BenchmarkReport, File), String> {
    let provenance = report::provenance()?;
    let environment = report::environment();
    let corpus = corpus::load(&arguments.corpus)?;

    let verification_started = Instant::now();
    let manifest = ptt2me::model_store::embedded_model_manifest()
        .map_err(|error| format!("embedded model manifest rejected: {error}"))?;
    let verified = ptt2me::model_store::verify_model_directory(&arguments.model, &manifest)
        .map_err(|error| format!("model verification failed: {error}"))?;
    let verification_ms = milliseconds(verification_started.elapsed());

    let mut prepared = Vec::with_capacity(corpus.len());
    for case in corpus {
        let started = Instant::now();
        let samples = ptt2me::audio::prepare_samples_for_performance_measurement(
            case.samples,
            ptt2me::constants::SAMPLE_RATE,
        )
        .ok_or_else(|| format!("corpus case {} became empty during preparation", case.id))?;
        prepared.push(PreparedCase {
            id: case.id,
            source: case.source,
            duration_seconds: case.duration_seconds,
            preparation_ms: milliseconds(started.elapsed()),
            samples,
        });
    }

    // Keep JSON on the original stdout descriptor and silence any native stdout.
    let output =
        report::isolate_stdout().map_err(|error| format!("could not isolate stdout: {error}"))?;
    let mut canonical_outputs = vec![None; prepared.len()];
    let mut equality = true;
    let mut configurations = Vec::with_capacity(arguments.threads.len());
    for threads in arguments.threads.iter().copied() {
        let usage_before = report::usage_snapshot()?;
        let configuration_started = Instant::now();
        let load_started = Instant::now();
        let mut recognizer = ptt2me::asr::load_sherpa_recognizer(verified.paths(), threads)?;
        let native_recognizer_load_ms = milliseconds(load_started.elapsed());
        let mut warmup_times = vec![Vec::new(); prepared.len()];
        let mut measured_times = vec![Vec::new(); prepared.len()];

        for _ in 0..arguments.warmups {
            transcribe_cases(
                &mut recognizer,
                &prepared,
                &mut warmup_times,
                &mut canonical_outputs,
                &mut equality,
            );
        }
        for _ in 0..arguments.repeats {
            transcribe_cases(
                &mut recognizer,
                &prepared,
                &mut measured_times,
                &mut canonical_outputs,
                &mut equality,
            );
        }

        let cases = case_reports(&prepared, &warmup_times, &measured_times);
        let usage_after = report::usage_snapshot()?;
        configurations.push(ConfigurationReport {
            cpu_threads: threads,
            native_recognizer_load_ms,
            configuration_wall_ms: milliseconds(configuration_started.elapsed()),
            cpu_delta: CpuDelta {
                user_ms: (usage_after.user_ms - usage_before.user_ms).max(0.0),
                system_ms: (usage_after.system_ms - usage_before.system_ms).max(0.0),
                scope: "delta for recognizer load, warmups, and measured transcriptions in this configuration",
            },
            cases,
        });
    }
    let peak_rss = report::usage_snapshot()?.peak_rss_bytes;
    let status = if equality { "PASS" } else { "FAIL" };
    let corpus_report = CorpusReport {
        cases: prepared
            .into_iter()
            .map(|case| CorpusCaseReport {
                id: case.id,
                source: case.source,
                duration_seconds: case.duration_seconds,
                preparation_fast_path_ms: case.preparation_ms,
            })
            .collect(),
        preparation_scope: "production 16 kHz resampler fast path after bounded WAV PCM decode; not microphone ring drain or non-16 kHz resampling",
    };
    Ok((
        BenchmarkReport {
            schema: 2,
            status,
            benchmark: "ptt2me direct native ASR performance",
            ptt2me_version: env!("CARGO_PKG_VERSION"),
            provenance,
            environment,
            model: ModelReport {
                id: verified.id().to_owned(),
                manifest_sha256: ptt2me::model_store::PRODUCTION_MODEL_MANIFEST_SHA256,
                verification_ms,
            },
            corpus: corpus_report,
            method: MethodReport {
                warmups_per_case: arguments.warmups,
                measured_repeats_per_case: arguments.repeats,
                quantiles: "nearest-rank over measured repeats for each fixed case",
                transport: "direct in-process recognizer; worker/process transport is not measured",
            },
            configurations,
            equality: EqualityReport {
                full_trimmed_output_equal: equality,
                scope: "exact in-memory comparison across every warmup, repeat, and requested CPU configuration",
            },
            whole_process_peak_rss_bytes: peak_rss,
            whole_process_peak_rss_scope: "ru_maxrss whole-process high-water mark; use one CPU configuration per invocation for an independent memory observation",
            privacy: "no transcript, audio sample, input path, or recognized-output digest is emitted",
            limitations: [
                "synthetic corpus timing does not establish human recognition accuracy or WER",
                "one machine does not establish cross-device performance",
                "few repeats describe this run and do not estimate population tail latency",
            ],
        },
        output,
    ))
}

fn transcribe_cases(
    recognizer: &mut sherpa_rs::transducer::TransducerRecognizer,
    prepared: &[PreparedCase],
    times: &mut [Vec<f64>],
    canonical_outputs: &mut [Option<String>],
    equality: &mut bool,
) {
    for (index, case) in prepared.iter().enumerate() {
        let started = Instant::now();
        let output = recognizer
            .transcribe(ptt2me::constants::SAMPLE_RATE, &case.samples)
            .trim()
            .to_owned();
        times[index].push(milliseconds(started.elapsed()));
        *equality &= observe_output(&mut canonical_outputs[index], output);
    }
}

fn case_reports(
    prepared: &[PreparedCase],
    warmup_times: &[Vec<f64>],
    measured_times: &[Vec<f64>],
) -> Vec<CasePerformance> {
    prepared
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let rtf = measured_times[index]
                .iter()
                .map(|duration_ms| duration_ms / (case.duration_seconds * 1_000.0))
                .collect::<Vec<_>>();
            CasePerformance {
                id: case.id.clone(),
                warmup_p50_ms: nearest_rank(&warmup_times[index], 0.50),
                warmup_p95_ms: nearest_rank(&warmup_times[index], 0.95),
                measured: Quantiles {
                    p50_ms: nearest_rank(&measured_times[index], 0.50),
                    p95_ms: nearest_rank(&measured_times[index], 0.95),
                    p50_rtf: nearest_rank(&rtf, 0.50),
                    p95_rtf: nearest_rank(&rtf, 0.95),
                },
            }
        })
        .collect()
}

fn observe_output(slot: &mut Option<String>, output: String) -> bool {
    match slot {
        Some(reference) => reference == &output,
        None => {
            *slot = Some(output);
            true
        }
    }
}

fn nearest_rank(values: &[f64], percentile: f64) -> f64 {
    debug_assert!(!values.is_empty());
    debug_assert!((0.0..=1.0).contains(&percentile));
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (percentile * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank - 1]
}

fn milliseconds(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_quantiles_and_full_output_equality_are_exact() {
        let values = [100.0, 2.0, 4.0, 3.0, 1.0];
        assert_eq!(nearest_rank(&values, 0.50), 3.0);
        assert_eq!(nearest_rank(&values, 0.95), 100.0);

        let mut reference = None;
        assert!(observe_output(
            &mut reference,
            "одинаковая длина а".to_owned()
        ));
        assert!(observe_output(
            &mut reference,
            "одинаковая длина а".to_owned()
        ));
        assert!(!observe_output(
            &mut reference,
            "одинаковая длина б".to_owned()
        ));
    }
}
