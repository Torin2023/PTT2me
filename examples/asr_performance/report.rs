use std::fs::File;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
pub(super) struct EnvironmentReport {
    observed_unix_seconds: u64,
    hardware_model: Option<String>,
    cpu: Option<String>,
    physical_cpus: Option<String>,
    logical_cpus: Option<String>,
    memory_bytes: Option<String>,
    macos_version: Option<String>,
    macos_build: Option<String>,
    runtime_rustc: Option<String>,
    power: Option<String>,
    thermal: Option<String>,
}

#[derive(Serialize)]
pub(super) struct BuildIdentityReport {
    source_commit: &'static str,
    source_dirty: bool,
    rustc: &'static str,
    scope: &'static str,
}

#[derive(Serialize)]
pub(super) struct ExecutableIdentityReport {
    sha256: String,
    size_bytes: u64,
    scope: &'static str,
}

#[derive(Serialize)]
pub(super) struct RuntimeCheckoutReport {
    head: Option<String>,
    dirty: Option<bool>,
    scope: &'static str,
}

#[derive(Serialize)]
pub(super) struct ProvenanceReport {
    build: BuildIdentityReport,
    executable: ExecutableIdentityReport,
    runtime_checkout: RuntimeCheckoutReport,
}

#[derive(Serialize)]
pub(super) struct ModelReport {
    pub(super) id: String,
    pub(super) manifest_sha256: &'static str,
    pub(super) verification_ms: f64,
}

#[derive(Serialize)]
pub(super) struct CorpusCaseReport {
    pub(super) id: String,
    pub(super) source: String,
    pub(super) duration_seconds: f64,
    pub(super) preparation_fast_path_ms: f64,
}

#[derive(Serialize)]
pub(super) struct CorpusReport {
    pub(super) cases: Vec<CorpusCaseReport>,
    pub(super) preparation_scope: &'static str,
}

#[derive(Serialize)]
pub(super) struct Quantiles {
    pub(super) p50_ms: f64,
    pub(super) p95_ms: f64,
    pub(super) p50_rtf: f64,
    pub(super) p95_rtf: f64,
}

#[derive(Serialize)]
pub(super) struct CasePerformance {
    pub(super) id: String,
    pub(super) warmup_p50_ms: f64,
    pub(super) warmup_p95_ms: f64,
    pub(super) measured: Quantiles,
}

#[derive(Serialize)]
pub(super) struct CpuDelta {
    pub(super) user_ms: f64,
    pub(super) system_ms: f64,
    pub(super) scope: &'static str,
}

#[derive(Serialize)]
pub(super) struct ConfigurationReport {
    pub(super) cpu_threads: i32,
    pub(super) native_recognizer_load_ms: f64,
    pub(super) configuration_wall_ms: f64,
    pub(super) cpu_delta: CpuDelta,
    pub(super) cases: Vec<CasePerformance>,
}

#[derive(Serialize)]
pub(super) struct EqualityReport {
    pub(super) full_trimmed_output_equal: bool,
    pub(super) scope: &'static str,
}

#[derive(Serialize)]
pub(super) struct MethodReport {
    pub(super) warmups_per_case: u32,
    pub(super) measured_repeats_per_case: u32,
    pub(super) quantiles: &'static str,
    pub(super) transport: &'static str,
}

#[derive(Serialize)]
pub(super) struct BenchmarkReport {
    pub(super) schema: u32,
    pub(super) status: &'static str,
    pub(super) benchmark: &'static str,
    pub(super) ptt2me_version: &'static str,
    pub(super) provenance: ProvenanceReport,
    pub(super) environment: EnvironmentReport,
    pub(super) model: ModelReport,
    pub(super) corpus: CorpusReport,
    pub(super) method: MethodReport,
    pub(super) configurations: Vec<ConfigurationReport>,
    pub(super) equality: EqualityReport,
    pub(super) whole_process_peak_rss_bytes: u64,
    pub(super) whole_process_peak_rss_scope: &'static str,
    pub(super) privacy: &'static str,
    pub(super) limitations: [&'static str; 3],
}

#[derive(Clone, Copy)]
pub(super) struct UsageSnapshot {
    pub(super) user_ms: f64,
    pub(super) system_ms: f64,
    pub(super) peak_rss_bytes: u64,
}

pub(super) fn provenance() -> Result<ProvenanceReport, String> {
    let build = validate_build_identity(
        option_env!("PTT2ME_BENCHMARK_BUILD_COMMIT")
            .ok_or_else(|| "build identity is missing PTT2ME_BENCHMARK_BUILD_COMMIT".to_owned())?,
        option_env!("PTT2ME_BENCHMARK_BUILD_DIRTY")
            .ok_or_else(|| "build identity is missing PTT2ME_BENCHMARK_BUILD_DIRTY".to_owned())?,
        option_env!("PTT2ME_BENCHMARK_BUILD_RUSTC")
            .ok_or_else(|| "build identity is missing PTT2ME_BENCHMARK_BUILD_RUSTC".to_owned())?,
    )?;
    Ok(ProvenanceReport {
        build,
        executable: executable_identity()?,
        runtime_checkout: RuntimeCheckoutReport {
            head: command_output("git", &["rev-parse", "HEAD"], false),
            dirty: command_output(
                "git",
                &["status", "--porcelain=v1", "--untracked-files=normal"],
                true,
            )
            .map(|status| !status.is_empty()),
            scope: "current working directory observation at benchmark run time; not executable build provenance",
        },
    })
}

fn validate_build_identity(
    commit: &'static str,
    dirty: &'static str,
    rustc: &'static str,
) -> Result<BuildIdentityReport, String> {
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("build commit must be a lowercase 40-character Git object id".to_owned());
    }
    let source_dirty = match dirty {
        "true" => true,
        "false" => false,
        _ => return Err("build dirty state must be true or false".to_owned()),
    };
    if rustc.is_empty() || rustc.len() > 256 || rustc.chars().any(char::is_control) {
        return Err("build rustc identity must be bounded single-line text".to_owned());
    }
    Ok(BuildIdentityReport {
        source_commit: commit,
        source_dirty,
        rustc,
        scope: "embedded by rustc at compile time from explicit benchmark build variables",
    })
}

fn executable_identity() -> Result<ExecutableIdentityReport, String> {
    let path = std::env::current_exe()
        .map_err(|error| format!("could not resolve current executable: {error}"))?;
    let mut file =
        File::open(path).map_err(|error| format!("could not open current executable: {error}"))?;
    let size_bytes = file
        .metadata()
        .map_err(|error| format!("could not inspect current executable: {error}"))?
        .len();
    let mut digest = Sha256::new();
    let mut observed = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("could not hash current executable: {error}"))?;
        if count == 0 {
            break;
        }
        observed = observed
            .checked_add(count as u64)
            .ok_or_else(|| "current executable size overflow".to_owned())?;
        digest.update(&buffer[..count]);
    }
    if observed != size_bytes {
        return Err("current executable changed while hashing".to_owned());
    }
    Ok(ExecutableIdentityReport {
        sha256: format!("{:x}", digest.finalize()),
        size_bytes,
        scope: "SHA-256 and byte length of the executable opened by this benchmark process",
    })
}

pub(super) fn environment() -> EnvironmentReport {
    EnvironmentReport {
        observed_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        hardware_model: command_output("sysctl", &["-n", "hw.model"], false),
        cpu: command_output("sysctl", &["-n", "machdep.cpu.brand_string"], false),
        physical_cpus: command_output("sysctl", &["-n", "hw.physicalcpu"], false),
        logical_cpus: command_output("sysctl", &["-n", "hw.logicalcpu"], false),
        memory_bytes: command_output("sysctl", &["-n", "hw.memsize"], false),
        macos_version: command_output("sw_vers", &["-productVersion"], false),
        macos_build: command_output("sw_vers", &["-buildVersion"], false),
        runtime_rustc: command_output("rustc", &["--version"], false),
        power: command_output("pmset", &["-g", "batt"], false),
        thermal: command_output("pmset", &["-g", "therm"], false),
    }
}

fn command_output(program: &str, arguments: &[&str], allow_empty: bool) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (allow_empty || !value.is_empty()).then_some(value)
}

pub(super) fn usage_snapshot() -> Result<UsageSnapshot, String> {
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

pub(super) fn isolate_stdout() -> std::io::Result<File> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_identity_requires_exact_commit_dirty_and_toolchain_values() {
        let identity = validate_build_identity(
            "0123456789abcdef0123456789abcdef01234567",
            "false",
            "rustc 1.94.0 (test)",
        )
        .unwrap();
        assert!(!identity.source_dirty);

        assert!(validate_build_identity("HEAD", "false", "rustc").is_err());
        assert!(validate_build_identity(
            "0123456789abcdef0123456789abcdef01234567",
            "unknown",
            "rustc"
        )
        .is_err());
        assert!(validate_build_identity(
            "0123456789abcdef0123456789abcdef01234567",
            "true",
            "rustc\nother"
        )
        .is_err());
    }
}
