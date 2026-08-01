use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::update_manifest::{
    classify_release, verify_artifact, verify_envelope, InstalledBuild, ManifestError,
    ReleaseDisposition, VerifiedRelease,
};

pub const AUTOMATIC_CHECK_INTERVAL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckReason {
    Automatic,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateFailure {
    Network,
    UntrustedManifest,
    Storage,
    DigestMismatch,
    OpenDmg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterState {
    Idle,
    Checking(CheckReason),
    Current,
    Available(VerifiedRelease),
    Downloading(VerifiedRelease),
    ReadyToInstall(PathBuf),
    UnpublishedLocal,
    Failed(UpdateFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterCommand {
    PersistLastAttempt(u64),
    FetchManifest(CheckReason),
    DownloadAndVerify(VerifiedRelease),
    OpenDmgAndQuit(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterEvent {
    AutomaticCheckDue { now: u64, last_attempt: Option<u64> },
    ManualCheckRequested,
    ManifestReceived(Vec<u8>),
    ManifestFailed(UpdateFailure),
    DownloadRequested,
    DownloadVerified(PathBuf),
    DownloadFailed(UpdateFailure),
}

pub struct Updater {
    installed: InstalledBuild,
    public_key: [u8; 32],
    state: UpdaterState,
}

impl Updater {
    pub const fn new(installed: InstalledBuild, public_key: [u8; 32]) -> Self {
        Self {
            installed,
            public_key,
            state: UpdaterState::Idle,
        }
    }

    pub const fn state(&self) -> &UpdaterState {
        &self.state
    }

    pub fn handle(&mut self, event: UpdaterEvent) -> Vec<UpdaterCommand> {
        match event {
            UpdaterEvent::AutomaticCheckDue { now, last_attempt } => {
                if !automatic_check_due(now, last_attempt) || !self.can_start_check() {
                    return Vec::new();
                }
                self.state = UpdaterState::Checking(CheckReason::Automatic);
                vec![
                    UpdaterCommand::PersistLastAttempt(now),
                    UpdaterCommand::FetchManifest(CheckReason::Automatic),
                ]
            }
            UpdaterEvent::ManualCheckRequested => {
                if !self.can_start_check() {
                    return Vec::new();
                }
                self.state = UpdaterState::Checking(CheckReason::Manual);
                vec![UpdaterCommand::FetchManifest(CheckReason::Manual)]
            }
            UpdaterEvent::ManifestReceived(bytes) => self.receive_manifest(&bytes),
            UpdaterEvent::ManifestFailed(failure) => {
                let visible = matches!(self.state, UpdaterState::Checking(CheckReason::Manual));
                if matches!(self.state, UpdaterState::Checking(_)) {
                    self.state = if visible {
                        UpdaterState::Failed(failure)
                    } else {
                        UpdaterState::Idle
                    };
                }
                Vec::new()
            }
            UpdaterEvent::DownloadRequested => {
                let UpdaterState::Available(release) = &self.state else {
                    return Vec::new();
                };
                let release = release.clone();
                self.state = UpdaterState::Downloading(release.clone());
                vec![UpdaterCommand::DownloadAndVerify(release)]
            }
            UpdaterEvent::DownloadVerified(path) => {
                if !matches!(self.state, UpdaterState::Downloading(_)) {
                    return Vec::new();
                }
                self.state = UpdaterState::ReadyToInstall(path.clone());
                vec![UpdaterCommand::OpenDmgAndQuit(path)]
            }
            UpdaterEvent::DownloadFailed(failure) => {
                if matches!(self.state, UpdaterState::Downloading(_)) {
                    self.state = UpdaterState::Failed(failure);
                }
                Vec::new()
            }
        }
    }

    fn can_start_check(&self) -> bool {
        !matches!(
            self.state,
            UpdaterState::Checking(_) | UpdaterState::Downloading(_)
        )
    }

    fn receive_manifest(&mut self, bytes: &[u8]) -> Vec<UpdaterCommand> {
        let UpdaterState::Checking(reason) = self.state else {
            return Vec::new();
        };
        match verify_envelope(bytes, &self.public_key) {
            Ok(release) => {
                self.state = match classify_release(&self.installed, &release) {
                    ReleaseDisposition::Available => UpdaterState::Available(release),
                    ReleaseDisposition::Current => UpdaterState::Current,
                    ReleaseDisposition::UnpublishedLocal => UpdaterState::UnpublishedLocal,
                };
            }
            Err(_) => {
                self.state = if reason == CheckReason::Manual {
                    UpdaterState::Failed(UpdateFailure::UntrustedManifest)
                } else {
                    UpdaterState::Idle
                };
            }
        }
        Vec::new()
    }
}

pub const fn automatic_check_due(now: u64, last_attempt: Option<u64>) -> bool {
    match last_attempt {
        None => true,
        Some(previous) => now.saturating_sub(previous) >= AUTOMATIC_CHECK_INTERVAL_SECONDS,
    }
}

pub fn cache_verified_download(
    cache_root: &Path,
    release: &VerifiedRelease,
    mut reader: impl Read,
) -> Result<PathBuf, UpdateFailure> {
    fs::create_dir_all(cache_root).map_err(|_| UpdateFailure::Storage)?;
    let file_name = format!("PTT2me-{}-macos-arm64.dmg", release.version);
    let final_path = cache_root.join(&file_name);
    let partial_path = cache_root.join(format!("{file_name}.part"));

    if final_path.is_file() {
        let cached = File::open(&final_path).map_err(|_| UpdateFailure::Storage)?;
        if verify_artifact(cached, release).is_ok() {
            return Ok(final_path);
        }
        fs::remove_file(&final_path).map_err(|_| UpdateFailure::Storage)?;
    }
    if partial_path.exists() {
        fs::remove_file(&partial_path).map_err(|_| UpdateFailure::Storage)?;
    }

    let write_result = (|| {
        let file = File::create(&partial_path).map_err(|_| UpdateFailure::Storage)?;
        let mut writer = BufWriter::new(file);
        std::io::copy(&mut reader, &mut writer).map_err(|_| UpdateFailure::Network)?;
        writer.flush().map_err(|_| UpdateFailure::Storage)?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|_| UpdateFailure::Storage)?;
        drop(writer);

        let downloaded = File::open(&partial_path).map_err(|_| UpdateFailure::Storage)?;
        verify_artifact(downloaded, release).map_err(map_artifact_error)?;
        fs::rename(&partial_path, &final_path).map_err(|_| UpdateFailure::Storage)?;
        Ok(final_path.clone())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&partial_path);
    }
    write_result
}

fn map_artifact_error(error: ManifestError) -> UpdateFailure {
    match error {
        ManifestError::ArtifactSizeMismatch | ManifestError::ArtifactDigestMismatch => {
            UpdateFailure::DigestMismatch
        }
        _ => UpdateFailure::Storage,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read};
    use std::path::PathBuf;

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    use super::{
        cache_verified_download, CheckReason, UpdateFailure, Updater, UpdaterCommand, UpdaterEvent,
        UpdaterState,
    };
    use crate::update_manifest::{InstalledBuild, VerifiedRelease};

    const PRIVATE_KEY: [u8; 32] = [0x29; 32];
    const DAY_SECONDS: u64 = 24 * 60 * 60;

    fn manifest(version: &str, build: u64) -> (Vec<u8>, [u8; 32]) {
        let signing_key = SigningKey::from_bytes(&PRIVATE_KEY);
        let payload = serde_json::to_vec(&json!({
            "channel": "stable",
            "version": version,
            "build": build,
            "source_commit": "0123456789abcdef0123456789abcdef01234567",
            "minimum_macos": "13.0",
            "architecture": "arm64",
            "download_url": format!("https://github.com/Torin2023/PTT2me/releases/download/v{version}/PTT2me-{version}-macos-arm64.dmg"),
            "sha256": "024e5cfd5ac7dd791c40e312a4abd2f6b351324c0d8b6e6d4d41356e7f072d2a",
            "size": 28,
            "published_at": "2026-08-01T12:00:00Z"
        }))
        .unwrap();
        let signature = signing_key.sign(&payload);
        let envelope = serde_json::to_vec(&json!({
            "schema": 1,
            "payload": STANDARD.encode(payload),
            "signature": STANDARD.encode(signature.to_bytes())
        }))
        .unwrap();
        (envelope, signing_key.verifying_key().to_bytes())
    }

    fn updater() -> Updater {
        let (_, key) = manifest("1.0.6", 202608011200);
        Updater::new(InstalledBuild::parse("1.0.5", 202607310831).unwrap(), key)
    }

    #[test]
    fn automatic_check_runs_only_when_twenty_four_hours_have_elapsed() {
        let mut updater = updater();

        assert!(updater
            .handle(UpdaterEvent::AutomaticCheckDue {
                now: 200_000,
                last_attempt: Some(200_000 - DAY_SECONDS + 1),
            })
            .is_empty());
        assert_eq!(updater.state(), &UpdaterState::Idle);

        assert_eq!(
            updater.handle(UpdaterEvent::AutomaticCheckDue {
                now: 200_000,
                last_attempt: Some(200_000 - DAY_SECONDS),
            }),
            vec![
                UpdaterCommand::PersistLastAttempt(200_000),
                UpdaterCommand::FetchManifest(CheckReason::Automatic),
            ]
        );
        assert_eq!(
            updater.state(),
            &UpdaterState::Checking(CheckReason::Automatic)
        );
    }

    #[test]
    fn manual_check_bypasses_automatic_interval() {
        let mut updater = updater();

        assert_eq!(
            updater.handle(UpdaterEvent::ManualCheckRequested),
            vec![UpdaterCommand::FetchManifest(CheckReason::Manual)]
        );
        assert_eq!(
            updater.state(),
            &UpdaterState::Checking(CheckReason::Manual)
        );
    }

    #[test]
    fn automatic_failure_is_silent_but_manual_failure_is_visible() {
        let mut automatic = updater();
        automatic.handle(UpdaterEvent::AutomaticCheckDue {
            now: DAY_SECONDS,
            last_attempt: None,
        });
        assert!(automatic
            .handle(UpdaterEvent::ManifestFailed(UpdateFailure::Network))
            .is_empty());
        assert_eq!(automatic.state(), &UpdaterState::Idle);

        let mut manual = updater();
        manual.handle(UpdaterEvent::ManualCheckRequested);
        assert!(manual
            .handle(UpdaterEvent::ManifestFailed(UpdateFailure::Network))
            .is_empty());
        assert_eq!(
            manual.state(),
            &UpdaterState::Failed(UpdateFailure::Network)
        );
    }

    #[test]
    fn verified_newer_manifest_requires_explicit_download_request() {
        let mut updater = updater();
        let (manifest, _) = manifest("1.0.6", 202608011200);
        updater.handle(UpdaterEvent::ManualCheckRequested);

        assert!(updater
            .handle(UpdaterEvent::ManifestReceived(manifest))
            .is_empty());
        let UpdaterState::Available(release) = updater.state() else {
            panic!("newer signed release must become available");
        };
        assert_eq!(release.version.to_string(), "1.0.6");

        let commands = updater.handle(UpdaterEvent::DownloadRequested);
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], UpdaterCommand::DownloadAndVerify(_)));
        assert!(matches!(updater.state(), UpdaterState::Downloading(_)));
    }

    #[test]
    fn invalid_manifest_never_exposes_a_download_action() {
        let mut updater = updater();
        updater.handle(UpdaterEvent::ManualCheckRequested);

        assert!(updater
            .handle(UpdaterEvent::ManifestReceived(b"not signed json".to_vec()))
            .is_empty());
        assert_eq!(
            updater.state(),
            &UpdaterState::Failed(UpdateFailure::UntrustedManifest)
        );
        assert!(updater.handle(UpdaterEvent::DownloadRequested).is_empty());
    }

    #[test]
    fn verified_download_is_opened_and_app_quits_but_digest_failure_is_not() {
        let mut verified = updater();
        let (manifest, _) = manifest("1.0.6", 202608011200);
        verified.handle(UpdaterEvent::ManualCheckRequested);
        verified.handle(UpdaterEvent::ManifestReceived(manifest.clone()));
        verified.handle(UpdaterEvent::DownloadRequested);
        let dmg = PathBuf::from("/cache/PTT2me-1.0.6-macos-arm64.dmg");
        assert_eq!(
            verified.handle(UpdaterEvent::DownloadVerified(dmg.clone())),
            vec![UpdaterCommand::OpenDmgAndQuit(dmg.clone())]
        );
        assert_eq!(verified.state(), &UpdaterState::ReadyToInstall(dmg));

        let mut rejected = updater();
        rejected.handle(UpdaterEvent::ManualCheckRequested);
        rejected.handle(UpdaterEvent::ManifestReceived(manifest));
        rejected.handle(UpdaterEvent::DownloadRequested);
        assert!(rejected
            .handle(UpdaterEvent::DownloadFailed(UpdateFailure::DigestMismatch))
            .is_empty());
        assert_eq!(
            rejected.state(),
            &UpdaterState::Failed(UpdateFailure::DigestMismatch)
        );
    }

    #[test]
    fn local_build_newer_than_github_is_reported_without_downgrade() {
        let (_, key) = manifest("1.0.6", 202608011200);
        let mut updater = Updater::new(InstalledBuild::parse("1.0.7", 202608021200).unwrap(), key);
        let (manifest, _) = manifest("1.0.6", 202608011200);
        updater.handle(UpdaterEvent::ManualCheckRequested);

        assert!(updater
            .handle(UpdaterEvent::ManifestReceived(manifest))
            .is_empty());
        assert_eq!(updater.state(), &UpdaterState::UnpublishedLocal);
        assert!(updater.handle(UpdaterEvent::DownloadRequested).is_empty());
    }

    fn cache_release() -> VerifiedRelease {
        VerifiedRelease {
            version: semver::Version::parse("1.0.6").unwrap(),
            build: 202608011200,
            source_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            minimum_macos: "13.0".to_owned(),
            download_url: "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-macos-arm64.dmg".to_owned(),
            sha256: "024e5cfd5ac7dd791c40e312a4abd2f6b351324c0d8b6e6d4d41356e7f072d2a".to_owned(),
            size: b"verified PTT2me dmg fixture".len() as u64,
            published_at: "2026-08-01T12:00:00Z".to_owned(),
        }
    }

    #[test]
    fn verified_download_is_atomically_promoted_without_partial_file() {
        let cache = tempfile::tempdir().unwrap();
        let release = cache_release();

        let path = cache_verified_download(
            cache.path(),
            &release,
            Cursor::new(b"verified PTT2me dmg fixture"),
        )
        .unwrap();

        assert_eq!(path.file_name().unwrap(), "PTT2me-1.0.6-macos-arm64.dmg");
        assert_eq!(std::fs::read(path).unwrap(), b"verified PTT2me dmg fixture");
        assert!(!cache
            .path()
            .join("PTT2me-1.0.6-macos-arm64.dmg.part")
            .exists());
    }

    #[test]
    fn rejected_download_leaves_no_final_or_partial_file() {
        let cache = tempfile::tempdir().unwrap();
        let release = cache_release();

        assert_eq!(
            cache_verified_download(cache.path(), &release, Cursor::new(vec![b'x'; 28])),
            Err(UpdateFailure::DigestMismatch)
        );
        assert!(std::fs::read_dir(cache.path()).unwrap().next().is_none());
    }

    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("a valid cached artifact must be reused without reading the network body")
        }
    }

    #[test]
    fn valid_cached_artifact_is_reused_without_consuming_download() {
        let cache = tempfile::tempdir().unwrap();
        let release = cache_release();
        let first = cache_verified_download(
            cache.path(),
            &release,
            Cursor::new(b"verified PTT2me dmg fixture"),
        )
        .unwrap();

        let reused = cache_verified_download(cache.path(), &release, PanicReader).unwrap();

        assert_eq!(reused, first);
    }
}
