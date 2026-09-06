use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_CORPUS_CASES: usize = 16;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_WARMUPS: u32 = 3;
const MAX_REPEATS: u32 = 20;
const MAX_LABEL_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 64;
const MAX_AUDIO_FRAMES: usize = ((ptt2me::constants::MAX_CAPTURE_MS
    + ptt2me::constants::CAPTURE_BUFFER_MARGIN_MS
    + ptt2me::constants::RELEASE_GRACE_MS)
    * ptt2me::constants::SAMPLE_RATE as u64
    / 1_000) as usize;
const MAX_WAV_BYTES: u64 = (MAX_AUDIO_FRAMES as u64 * 2) + 4_096;

#[derive(Debug, PartialEq, Eq)]
struct Arguments {
    model: PathBuf,
    corpus: PathBuf,
    threads: Vec<i32>,
    warmups: u32,
    repeats: u32,
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut arguments = arguments.into_iter();
    let mut model = None;
    let mut corpus = None;
    let mut consent = false;
    let mut threads = vec![1, 2, 4];
    let mut warmups = 1;
    let mut repeats = 5;
    let mut threads_set = false;
    let mut warmups_set = false;
    let mut repeats_set = false;

    while let Some(argument) = arguments.next() {
        let option = argument
            .to_str()
            .ok_or_else(|| "option name is not valid UTF-8".to_owned())?;
        match option {
            "--model" if model.is_none() => model = Some(required_path(&mut arguments, option)?),
            "--corpus" if corpus.is_none() => corpus = Some(required_path(&mut arguments, option)?),
            "--consent-to-process-audio" if !consent => consent = true,
            "--threads" if !threads_set => {
                let raw = required_utf8(&mut arguments, option)?;
                threads = parse_threads(&raw)?;
                threads_set = true;
            }
            "--warmups" if !warmups_set => {
                let raw = required_utf8(&mut arguments, option)?;
                warmups = parse_count("warmups", &raw, 1, MAX_WARMUPS)?;
                warmups_set = true;
            }
            "--repeats" if !repeats_set => {
                let raw = required_utf8(&mut arguments, option)?;
                repeats = parse_count("repeats", &raw, 1, MAX_REPEATS)?;
                repeats_set = true;
            }
            _ => return Err(format!("unknown or repeated option: {option}")),
        }
    }

    if !consent {
        return Err("--consent-to-process-audio is required".to_owned());
    }
    Ok(Arguments {
        model: model.ok_or_else(|| "--model is required".to_owned())?,
        corpus: corpus.ok_or_else(|| "--corpus is required".to_owned())?,
        threads,
        warmups,
        repeats,
    })
}

fn required_path(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(
        arguments
            .next()
            .ok_or_else(|| format!("{option} requires a value"))?,
    );
    if path.as_os_str().is_empty() {
        Err(format!("{option} requires a non-empty path"))
    } else {
        Ok(path)
    }
}

fn required_utf8(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))?
        .into_string()
        .map_err(|_| format!("{option} value is not valid UTF-8"))
}

fn parse_threads(raw: &str) -> Result<Vec<i32>, String> {
    let mut parsed = Vec::new();
    for value in raw.split(',') {
        let value = value
            .parse::<i32>()
            .map_err(|_| format!("invalid thread count: {value}"))?;
        if ![1, 2, 4].contains(&value) || parsed.contains(&value) {
            return Err("threads must be a unique comma-separated subset of 1,2,4".to_owned());
        }
        parsed.push(value);
    }
    if parsed.is_empty() {
        Err("at least one thread count is required".to_owned())
    } else {
        Ok(parsed)
    }
}

fn parse_count(name: &str, raw: &str, minimum: u32, maximum: u32) -> Result<u32, String> {
    let count = raw
        .parse::<u32>()
        .map_err(|_| format!("invalid {name}: {raw}"))?;
    if (minimum..=maximum).contains(&count) {
        Ok(count)
    } else {
        Err(format!("{name} must be in {minimum}..={maximum}"))
    }
}

#[derive(Debug, PartialEq)]
struct WavData {
    samples: Vec<f32>,
    frames: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusEntry {
    id: String,
    wav: PathBuf,
    #[serde(rename = "reference")]
    _reference: serde::de::IgnoredAny,
    duration_seconds: f64,
    source: String,
    sha256: String,
    frames: usize,
    format: String,
}

struct CorpusCase {
    id: String,
    source: String,
    duration_seconds: f64,
    samples: Vec<f32>,
}

fn load_corpus(manifest_path: &std::path::Path) -> Result<Vec<CorpusCase>, String> {
    let manifest_bytes =
        read_bounded_regular(manifest_path, MAX_MANIFEST_BYTES, "corpus manifest")?;
    let entries: Vec<CorpusEntry> = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("invalid corpus manifest JSON: {error}"))?;
    if entries.is_empty() || entries.len() > MAX_CORPUS_CASES {
        return Err(format!("corpus must contain 1..={MAX_CORPUS_CASES} cases"));
    }

    let base = manifest_path
        .parent()
        .ok_or_else(|| "corpus manifest has no parent directory".to_owned())?;
    let mut ids = std::collections::HashSet::with_capacity(entries.len());
    let mut corpus = Vec::with_capacity(entries.len());
    for entry in entries {
        let CorpusEntry {
            id,
            wav,
            _reference: _,
            duration_seconds,
            source,
            sha256,
            frames,
            format,
        } = entry;
        if id.is_empty()
            || id.len() > MAX_ID_BYTES
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || !ids.insert(id.clone())
        {
            return Err("corpus case ids must be unique bounded ASCII identifiers".to_owned());
        }
        if source.is_empty()
            || source.len() > MAX_LABEL_BYTES
            || source.chars().any(char::is_control)
        {
            return Err(
                "corpus source labels must be non-empty bounded single-line text".to_owned(),
            );
        }
        if format != "mono 16000 Hz PCM16 WAV" {
            return Err(format!("corpus case {id} has an unsupported format label"));
        }
        if !is_lowercase_sha256(&sha256) {
            return Err(format!("corpus case {id} has an invalid SHA-256"));
        }
        let wav_path = if wav.is_absolute() {
            wav
        } else {
            base.join(wav)
        };
        let bytes = read_bounded_regular(&wav_path, MAX_WAV_BYTES, "WAV file")?;
        if format!("{:x}", Sha256::digest(&bytes)) != sha256 {
            return Err(format!("corpus case {id} WAV digest mismatch"));
        }
        let decoded = decode_mono_pcm16_wav(&bytes)?;
        if frames == 0 || frames > MAX_AUDIO_FRAMES || frames != decoded.frames {
            return Err(format!("corpus case {id} frame count mismatch or overflow"));
        }
        if !duration_seconds.is_finite()
            || (duration_seconds - frames as f64 / 16_000.0).abs() > 0.5 / 16_000.0
        {
            return Err(format!("corpus case {id} duration mismatch"));
        }
        corpus.push(CorpusCase {
            id,
            source,
            duration_seconds,
            samples: decoded.samples,
        });
    }
    Ok(corpus)
}

fn read_bounded_regular(path: &std::path::Path, limit: u64, kind: &str) -> Result<Vec<u8>, String> {
    let path_metadata =
        fs::symlink_metadata(path).map_err(|error| format!("could not inspect {kind}: {error}"))?;
    if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
        return Err(format!("{kind} must be a regular non-symlink file"));
    }
    if path_metadata.len() > limit {
        return Err(format!("{kind} exceeds {limit} bytes"));
    }
    let mut file =
        open_read_only_nofollow(path).map_err(|error| format!("could not open {kind}: {error}"))?;
    let descriptor_metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect open {kind}: {error}"))?;
    if !descriptor_metadata.file_type().is_file() || descriptor_metadata.len() > limit {
        return Err(format!("{kind} changed or exceeds {limit} bytes"));
    }
    let mut bytes = Vec::with_capacity(descriptor_metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {kind}: {error}"))?;
    if bytes.len() as u64 > limit || bytes.len() as u64 != descriptor_metadata.len() {
        return Err(format!("{kind} changed or exceeds {limit} bytes"));
    }
    Ok(bytes)
}

fn open_read_only_nofollow(path: &std::path::Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_mono_pcm16_wav(bytes: &[u8]) -> Result<WavData, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("file is not a RIFF/WAVE stream".to_owned());
    }
    let declared_size = read_u32(bytes, 4)? as usize;
    if declared_size.checked_add(8) != Some(bytes.len()) {
        return Err("RIFF size does not match file size".to_owned());
    }

    let mut offset = 12;
    let mut format = None;
    let mut data = None;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(8)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "truncated WAV chunk header".to_owned())?;
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_len = read_u32(bytes, offset + 4)? as usize;
        let chunk_start = header_end;
        let chunk_end = chunk_start
            .checked_add(chunk_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "truncated WAV chunk".to_owned())?;
        match chunk_id {
            b"fmt " if format.is_none() => {
                if chunk_len < 16 {
                    return Err("WAV fmt chunk is too short".to_owned());
                }
                format = Some((
                    read_u16(bytes, chunk_start)?,
                    read_u16(bytes, chunk_start + 2)?,
                    read_u32(bytes, chunk_start + 4)?,
                    read_u32(bytes, chunk_start + 8)?,
                    read_u16(bytes, chunk_start + 12)?,
                    read_u16(bytes, chunk_start + 14)?,
                ));
            }
            b"data" if data.is_none() => data = Some(&bytes[chunk_start..chunk_end]),
            _ => {}
        }
        offset = chunk_end
            .checked_add(chunk_len % 2)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "invalid WAV chunk padding".to_owned())?;
    }

    let (encoding, channels, sample_rate, byte_rate, block_align, bits) =
        format.ok_or_else(|| "WAV fmt chunk is missing".to_owned())?;
    if encoding != 1
        || channels != 1
        || sample_rate != 16_000
        || byte_rate != 32_000
        || block_align != 2
        || bits != 16
    {
        return Err("WAV must be mono 16000 Hz PCM16".to_owned());
    }
    let data = data.ok_or_else(|| "WAV data chunk is missing".to_owned())?;
    if data.len() % 2 != 0 {
        return Err("WAV PCM16 data has an odd byte count".to_owned());
    }
    let samples = data
        .chunks_exact(2)
        .map(|sample| normalize_i16(i16::from_le_bytes([sample[0], sample[1]])))
        .collect::<Vec<_>>();
    Ok(WavData {
        frames: samples.len(),
        samples,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .and_then(|value| value.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| "truncated little-endian u16".to_owned())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "truncated little-endian u32".to_owned())
}

fn normalize_i16(sample: i16) -> f32 {
    if sample < 0 {
        sample as f32 / -(i16::MIN as f32)
    } else {
        sample as f32 / i16::MAX as f32
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

fn outputs_equal(reference: &[String], candidate: &[String]) -> bool {
    reference == candidate
}

#[derive(Serialize)]
struct EnvironmentReport {
    observed_unix_seconds: u64,
    hardware_model: Option<String>,
    cpu: Option<String>,
    physical_cpus: Option<String>,
    logical_cpus: Option<String>,
    memory_bytes: Option<String>,
    macos_version: Option<String>,
    macos_build: Option<String>,
    rustc: Option<String>,
    power: Option<String>,
    thermal: Option<String>,
}

#[derive(Serialize)]
struct ModelReport {
    id: String,
    manifest_sha256: &'static str,
    verification_ms: f64,
}

#[derive(Serialize)]
struct CorpusCaseReport {
    id: String,
    source: String,
    duration_seconds: f64,
    preparation_fast_path_ms: f64,
}

#[derive(Serialize)]
struct CorpusReport {
    cases: Vec<CorpusCaseReport>,
    preparation_scope: &'static str,
}

#[derive(Serialize)]
struct Quantiles {
    p50_ms: f64,
    p95_ms: f64,
    p50_rtf: f64,
    p95_rtf: f64,
}

#[derive(Serialize)]
struct CasePerformance {
    id: String,
    warmup_p50_ms: f64,
    warmup_p95_ms: f64,
    measured: Quantiles,
}

#[derive(Serialize)]
struct CpuDelta {
    user_ms: f64,
    system_ms: f64,
    scope: &'static str,
}

#[derive(Serialize)]
struct ConfigurationReport {
    cpu_threads: i32,
    native_recognizer_load_ms: f64,
    configuration_wall_ms: f64,
    cpu_delta: CpuDelta,
    cases: Vec<CasePerformance>,
}

#[derive(Serialize)]
struct EqualityReport {
    full_trimmed_output_equal: bool,
    scope: &'static str,
}

#[derive(Serialize)]
struct BenchmarkReport {
    schema: u32,
    status: &'static str,
    benchmark: &'static str,
    ptt2me_version: &'static str,
    source_commit: Option<String>,
    environment: EnvironmentReport,
    model: ModelReport,
    corpus: CorpusReport,
    method: MethodReport,
    configurations: Vec<ConfigurationReport>,
    equality: EqualityReport,
    whole_process_peak_rss_bytes: u64,
    whole_process_peak_rss_scope: &'static str,
    privacy: &'static str,
    limitations: [&'static str; 3],
}

#[derive(Serialize)]
struct MethodReport {
    warmups_per_case: u32,
    measured_repeats_per_case: u32,
    quantiles: &'static str,
    transport: &'static str,
}

struct PreparedCase {
    id: String,
    source: String,
    duration_seconds: f64,
    preparation_ms: f64,
    samples: Vec<f32>,
}

#[derive(Clone, Copy)]
struct UsageSnapshot {
    user_ms: f64,
    system_ms: f64,
    peak_rss_bytes: u64,
}

fn execute(arguments: Arguments) -> Result<(BenchmarkReport, File), String> {
    let environment = environment_report();
    let source_commit = command_output("git", &["rev-parse", "HEAD"]);
    let corpus = load_corpus(&arguments.corpus)?;

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
    let output = isolate_stdout().map_err(|error| format!("could not isolate stdout: {error}"))?;
    let mut canonical_outputs = vec![None; prepared.len()];
    let mut equality = true;
    let mut configurations = Vec::with_capacity(arguments.threads.len());
    for threads in arguments.threads.iter().copied() {
        let usage_before = usage_snapshot()?;
        let configuration_started = Instant::now();
        let load_started = Instant::now();
        let mut recognizer = ptt2me::asr::load_sherpa_recognizer(verified.paths(), threads)?;
        let native_recognizer_load_ms = milliseconds(load_started.elapsed());
        let mut warmup_times = vec![Vec::new(); prepared.len()];
        let mut measured_times = vec![Vec::new(); prepared.len()];

        for _ in 0..arguments.warmups {
            for (index, case) in prepared.iter().enumerate() {
                let started = Instant::now();
                let output = recognizer
                    .transcribe(ptt2me::constants::SAMPLE_RATE, &case.samples)
                    .trim()
                    .to_owned();
                warmup_times[index].push(milliseconds(started.elapsed()));
                equality &= observe_output(&mut canonical_outputs[index], output);
            }
        }
        for _ in 0..arguments.repeats {
            for (index, case) in prepared.iter().enumerate() {
                let started = Instant::now();
                let output = recognizer
                    .transcribe(ptt2me::constants::SAMPLE_RATE, &case.samples)
                    .trim()
                    .to_owned();
                measured_times[index].push(milliseconds(started.elapsed()));
                equality &= observe_output(&mut canonical_outputs[index], output);
            }
        }

        let cases = prepared
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
            .collect();
        let usage_after = usage_snapshot()?;
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
    let peak_rss = usage_snapshot()?.peak_rss_bytes;
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
            schema: 1,
            status,
            benchmark: "ptt2me direct native ASR performance",
            ptt2me_version: env!("CARGO_PKG_VERSION"),
            source_commit,
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

fn observe_output(slot: &mut Option<String>, output: String) -> bool {
    match slot {
        Some(reference) => outputs_equal(
            std::slice::from_ref(reference),
            std::slice::from_ref(&output),
        ),
        None => {
            *slot = Some(output);
            true
        }
    }
}

fn milliseconds(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn usage_snapshot() -> Result<UsageSnapshot, String> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage initializes the provided rusage on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(format!(
            "getrusage failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let usage = unsafe { usage.assume_init() };
    Ok(UsageSnapshot {
        user_ms: timeval_milliseconds(usage.ru_utime),
        system_ms: timeval_milliseconds(usage.ru_stime),
        peak_rss_bytes: usage.ru_maxrss.max(0) as u64,
    })
}

fn timeval_milliseconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 * 1_000.0 + value.tv_usec as f64 / 1_000.0
}

fn isolate_stdout() -> std::io::Result<File> {
    use std::os::fd::AsRawFd;
    let null = File::options().write(true).open("/dev/null")?;
    // SAFETY: fcntl creates a fresh owned descriptor; dup2 replaces only fd 1.
    let fd = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let output = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(File::from(output))
}

fn environment_report() -> EnvironmentReport {
    EnvironmentReport {
        observed_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        hardware_model: command_output("sysctl", &["-n", "hw.model"]),
        cpu: command_output("sysctl", &["-n", "machdep.cpu.brand_string"]),
        physical_cpus: command_output("sysctl", &["-n", "hw.physicalcpu"]),
        logical_cpus: command_output("sysctl", &["-n", "hw.logicalcpu"]),
        memory_bytes: command_output("sysctl", &["-n", "hw.memsize"]),
        macos_version: command_output("sw_vers", &["-productVersion"]),
        macos_build: command_output("sw_vers", &["-buildVersion"]),
        rustc: command_output("rustc", &["--version"]),
        power: command_output("pmset", &["-g", "batt"]),
        thermal: command_output("pmset", &["-g", "therm"]),
    }
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn main() {
    let result = parse_arguments(std::env::args_os().skip(1)).and_then(execute);
    match result {
        Ok((report, mut output)) => {
            if serde_json::to_writer_pretty(&mut output, &report).is_err()
                || output.write_all(b"\n").is_err()
            {
                eprintln!("PTT2me ASR performance benchmark failed: could not write JSON report");
                std::process::exit(2);
            }
            if report.status != "PASS" {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("PTT2me ASR performance benchmark failed: {error}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;

    fn valid_arguments() -> Vec<OsString> {
        [
            "--model",
            "/tmp/model",
            "--corpus",
            "/tmp/corpus.json",
            "--consent-to-process-audio",
            "--threads",
            "1,2,4",
            "--warmups",
            "1",
            "--repeats",
            "5",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    fn wav(channels: u16, sample_rate: u32, bits_per_sample: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    fn corpus_fixture() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let wav_path = directory.path().join("sample.wav");
        let bytes = wav(1, 16_000, 16, &[0; 16_000]);
        fs::write(&wav_path, &bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let manifest_path = directory.path().join("corpus.json");
        let manifest = serde_json::json!([{
            "id": "sample",
            "wav": "sample.wav",
            "reference": "synthetic private reference",
            "duration_seconds": 1.0,
            "source": "synthetic fixture; not human speech corpus",
            "sha256": digest,
            "frames": 16_000,
            "format": "mono 16000 Hz PCM16 WAV"
        }]);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        (directory, manifest_path)
    }

    #[test]
    fn accepts_explicit_bounded_arguments() {
        assert_eq!(
            parse_arguments(valid_arguments()).unwrap(),
            Arguments {
                model: PathBuf::from("/tmp/model"),
                corpus: PathBuf::from("/tmp/corpus.json"),
                threads: vec![1, 2, 4],
                warmups: 1,
                repeats: 5,
            }
        );
    }

    #[test]
    fn rejects_missing_consent_unknown_options_and_unbounded_counts() {
        let mut missing_consent = valid_arguments();
        missing_consent.retain(|value| value != "--consent-to-process-audio");
        assert!(parse_arguments(missing_consent).is_err());

        for replacement in ["0", "21"] {
            let mut arguments = valid_arguments();
            let index = arguments.iter().position(|value| value == "5").unwrap();
            arguments[index] = replacement.into();
            assert!(parse_arguments(arguments).is_err());
        }

        let mut unknown = valid_arguments();
        unknown.push("--download".into());
        assert!(parse_arguments(unknown).is_err());

        let mut repeated = valid_arguments();
        repeated.extend([OsString::from("--model"), OsString::from("/tmp/other")]);
        assert!(parse_arguments(repeated).is_err());
    }

    #[test]
    fn accepts_only_unique_supported_cpu_thread_counts() {
        for value in ["0", "3", "8", "1,1", "1,2,3"] {
            let mut arguments = valid_arguments();
            let index = arguments.iter().position(|item| item == "1,2,4").unwrap();
            arguments[index] = value.into();
            assert!(parse_arguments(arguments).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn decodes_only_mono_16khz_pcm16_wav() {
        let decoded = decode_mono_pcm16_wav(&wav(1, 16_000, 16, &[-32_768, 0, 32_767])).unwrap();
        assert_eq!(decoded.frames, 3);
        assert_eq!(decoded.samples, vec![-1.0, 0.0, 1.0]);

        assert!(decode_mono_pcm16_wav(&wav(2, 16_000, 16, &[0, 0])).is_err());
        assert!(decode_mono_pcm16_wav(&wav(1, 48_000, 16, &[0])).is_err());
        assert!(decode_mono_pcm16_wav(&wav(1, 16_000, 8, &[0])).is_err());
        assert!(decode_mono_pcm16_wav(b"not a wave").is_err());
    }

    #[test]
    fn constants_keep_manifest_and_repeat_work_bounded() {
        assert_eq!(MAX_CORPUS_CASES, 16);
        assert_eq!(MAX_MANIFEST_BYTES, 65_536);
        assert_eq!(MAX_WARMUPS, 3);
        assert_eq!(MAX_REPEATS, 20);
        assert_eq!(MAX_AUDIO_FRAMES, 418_880);
        assert_eq!(MAX_WAV_BYTES, 841_856);
    }

    #[test]
    fn loads_relative_verified_corpus_without_retaining_reference_text() {
        let (_directory, manifest) = corpus_fixture();
        let corpus = load_corpus(&manifest).unwrap();

        assert_eq!(corpus.len(), 1);
        assert_eq!(corpus[0].id, "sample");
        assert_eq!(
            corpus[0].source,
            "synthetic fixture; not human speech corpus"
        );
        assert_eq!(corpus[0].duration_seconds, 1.0);
        assert_eq!(corpus[0].samples.len(), 16_000);
    }

    #[test]
    fn rejects_changed_audio_and_manifest_size_overflow() {
        let (directory, manifest) = corpus_fixture();
        fs::write(
            directory.path().join("sample.wav"),
            wav(1, 16_000, 16, &[1; 16_000]),
        )
        .unwrap();
        assert!(load_corpus(&manifest).is_err());

        fs::write(&manifest, vec![b' '; MAX_MANIFEST_BYTES as usize + 1]).unwrap();
        assert!(load_corpus(&manifest).is_err());
    }

    #[test]
    fn nearest_rank_quantiles_and_full_output_equality_are_exact() {
        let values = [100.0, 2.0, 4.0, 3.0, 1.0];
        assert_eq!(nearest_rank(&values, 0.50), 3.0);
        assert_eq!(nearest_rank(&values, 0.95), 100.0);

        let reference = vec!["одинаковая длина а".to_owned()];
        let same = vec!["одинаковая длина а".to_owned()];
        let different = vec!["одинаковая длина б".to_owned()];
        assert!(outputs_equal(&reference, &same));
        assert!(!outputs_equal(&reference, &different));
    }
}
