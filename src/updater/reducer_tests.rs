use std::cell::Cell;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

use super::{
    cache_verified_download, cache_verified_download_with_promoter, next_automatic_check_at,
    read_bounded_manifest, validate_http_response_metadata, ArtifactKind, ArtifactWorker,
    CheckReason, DownloadResponse, FileUpdateStorage, OperationId, QuarantineChecker, RetryAction,
    SelectedArtifact, UpdateFailure, UpdateFetch, Updater, UpdaterCommand, UpdaterEvent,
    UpdaterState, VerifiedDownload, AUTOMATIC_CHECK_INTERVAL_SECONDS,
    FIRST_AUTOMATIC_CHECK_DELAY_SECONDS,
};
use crate::constants::MAX_UPDATE_ARTIFACT_BYTES;
use crate::update_manifest::{
    verify_envelope, ArtifactDescriptor, InstalledBuild, MacOsVersion, ModelAvailability,
    VerifiedRelease,
};

const PRIVATE_KEY: [u8; 32] = [0x29; 32];
const FULL_SHA256: &str = "80530994d8ca7568fcba045b34d82b6f0c31188a07aae38de1fede676e08a1a4";
const UPDATE_SHA256: &str = "79a45998882238cd19dfefda21805a21e2769e6750a8bffab9f3443101d2b5f6";

fn manifest(
    version: &str,
    build: u64,
    source_commit: &str,
    minimum_macos: &str,
) -> (Vec<u8>, [u8; 32]) {
    let signing_key = SigningKey::from_bytes(&PRIVATE_KEY);
    let payload = serde_json::to_vec(&json!({
        "channel": "stable",
        "version": version,
        "build": build,
        "source_commit": source_commit,
        "minimum_macos": minimum_macos,
        "architecture": "arm64",
        "required_model": {
            "id": "gigaam-v3-rnnt-v1",
            "manifest_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        },
        "fresh_install": {
            "url": format!("https://github.com/Torin2023/PTT2me/releases/download/v{version}/PTT2me-{version}-full-macos-arm64.dmg"),
            "sha256": FULL_SHA256,
            "size": 32
        },
        "application_update": {
            "url": format!("https://github.com/Torin2023/PTT2me/releases/download/v{version}/PTT2me-{version}-update-macos-arm64.dmg"),
            "sha256": UPDATE_SHA256,
            "size": 34
        },
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

fn updater_with_installed(
    version: &str,
    build: u64,
    source_commit: &str,
    running_macos: &str,
) -> Updater {
    let (_, key) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );
    Updater::new(
        InstalledBuild::parse(version, build, source_commit).unwrap(),
        key,
        MacOsVersion::parse(running_macos).unwrap(),
    )
}

fn updater() -> Updater {
    updater_with_installed(
        "1.0.5",
        202607310831,
        "1111111111111111111111111111111111111111",
        "13.0",
    )
}

fn manual_manifest_result(
    updater: &mut Updater,
    bytes: Vec<u8>,
    model: ModelAvailability,
) -> OperationId {
    let commands = updater.handle(UpdaterEvent::ManualCheckRequested { now: 1_000 });
    let operation_id = match commands.as_slice() {
        [UpdaterCommand::PersistLastAttempt(1_000), UpdaterCommand::ScheduleAutomaticCheck(87_400), UpdaterCommand::FetchManifest {
            operation_id,
            reason: CheckReason::Manual,
        }] => *operation_id,
        unexpected => panic!("unexpected manual-check commands: {unexpected:?}"),
    };
    assert!(updater
        .handle(UpdaterEvent::ManifestReceived {
            operation_id,
            bytes,
            model,
        })
        .is_empty());
    operation_id
}

fn offered_artifact(state: &UpdaterState) -> (&SelectedArtifact, &str) {
    match state {
        UpdaterState::Available { artifact, .. } => (artifact, "available"),
        UpdaterState::DivergedLocal { artifact, .. } => (artifact, "diverged"),
        UpdaterState::RepairRequired { artifact, .. } => (artifact, "repair"),
        unexpected => panic!("expected an offered artifact, got {unexpected:?}"),
    }
}

#[test]
fn scheduler_applies_launch_floor_recent_attempt_and_future_clock_clamp() {
    assert_eq!(
        next_automatic_check_at(1_000, 1_000, None),
        1_000 + FIRST_AUTOMATIC_CHECK_DELAY_SECONDS
    );
    assert_eq!(
        next_automatic_check_at(1_000, 1_000, Some(950)),
        950 + AUTOMATIC_CHECK_INTERVAL_SECONDS
    );
    assert_eq!(
        next_automatic_check_at(100_000, 100_000, Some(100)),
        100_000 + FIRST_AUTOMATIC_CHECK_DELAY_SECONDS
    );
    assert_eq!(
        next_automatic_check_at(1_000, 1_000, Some(2_000)),
        1_000 + AUTOMATIC_CHECK_INTERVAL_SECONDS
    );
}

#[test]
fn launch_and_suppressed_automatic_check_only_schedule_the_due_time() {
    let mut updater = updater();

    assert_eq!(
        updater.handle(UpdaterEvent::Launched {
            launch_at: 1_000,
            now: 1_000,
            last_attempt: None,
        }),
        vec![UpdaterCommand::ScheduleAutomaticCheck(1_060)]
    );
    assert_eq!(
        updater.handle(UpdaterEvent::AutomaticCheckDue {
            launch_at: 1_000,
            now: 1_010,
            last_attempt: Some(950),
        }),
        vec![UpdaterCommand::ScheduleAutomaticCheck(87_350)]
    );
    assert_eq!(updater.state(), &UpdaterState::Idle);
}

#[test]
fn automatic_and_manual_attempts_persist_before_fetch_and_reschedule() {
    let mut automatic = updater();
    let commands = automatic.handle(UpdaterEvent::AutomaticCheckDue {
        launch_at: 1_000,
        now: 1_060,
        last_attempt: None,
    });
    assert!(matches!(
        commands.as_slice(),
        [
            UpdaterCommand::PersistLastAttempt(1_060),
            UpdaterCommand::ScheduleAutomaticCheck(87_460),
            UpdaterCommand::FetchManifest {
                reason: CheckReason::Automatic,
                ..
            },
        ]
    ));

    let mut manual = updater();
    let commands = manual.handle(UpdaterEvent::ManualCheckRequested { now: 2_000 });
    assert!(matches!(
        commands.as_slice(),
        [
            UpdaterCommand::PersistLastAttempt(2_000),
            UpdaterCommand::ScheduleAutomaticCheck(88_400),
            UpdaterCommand::FetchManifest {
                reason: CheckReason::Manual,
                ..
            },
        ]
    ));
}

#[test]
fn verified_matching_model_selects_update_and_missing_or_invalid_selects_full() {
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );

    for (model, expected_kind, expected_size) in [
        (
            ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
            ArtifactKind::Update,
            34,
        ),
        (ModelAvailability::Missing, ArtifactKind::Full, 32),
        (ModelAvailability::Invalid, ArtifactKind::Full, 32),
        (
            ModelAvailability::Verified("another-model".to_owned()),
            ArtifactKind::Full,
            32,
        ),
    ] {
        let mut updater = updater();
        manual_manifest_result(&mut updater, bytes.clone(), model);
        let (artifact, disposition) = offered_artifact(updater.state());
        assert_eq!(disposition, "available");
        assert_eq!(artifact.kind, expected_kind);
        assert_eq!(artifact.descriptor.size, expected_size);
    }
}

#[test]
fn current_diverged_repair_incompatible_and_unpublished_states_are_distinct() {
    let remote_commit = "0123456789abcdef0123456789abcdef01234567";
    let (current_bytes, _) = manifest("1.0.6", 202608011200, remote_commit, "13.0");

    let mut current = updater_with_installed("1.0.6", 202608011200, remote_commit, "13.0");
    manual_manifest_result(
        &mut current,
        current_bytes.clone(),
        ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
    );
    assert_eq!(current.state(), &UpdaterState::Current);

    let mut repair = updater_with_installed("1.0.6", 202608011200, remote_commit, "13.0");
    manual_manifest_result(
        &mut repair,
        current_bytes.clone(),
        ModelAvailability::Missing,
    );
    let (artifact, disposition) = offered_artifact(repair.state());
    assert_eq!(disposition, "repair");
    assert_eq!(artifact.kind, ArtifactKind::Full);

    let mut diverged = updater_with_installed(
        "1.0.6",
        202608011200,
        "1111111111111111111111111111111111111111",
        "13.0",
    );
    manual_manifest_result(
        &mut diverged,
        current_bytes,
        ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
    );
    let (artifact, disposition) = offered_artifact(diverged.state());
    assert_eq!(disposition, "diverged");
    assert_eq!(artifact.kind, ArtifactKind::Update);

    let (incompatible_bytes, _) = manifest("1.0.6", 202608011200, remote_commit, "14.0");
    let mut incompatible = updater();
    manual_manifest_result(
        &mut incompatible,
        incompatible_bytes,
        ModelAvailability::Missing,
    );
    assert!(matches!(
        incompatible.state(),
        UpdaterState::Incompatible { required_macos, .. }
            if required_macos.to_string() == "14.0"
    ));

    let mut unpublished = updater_with_installed(
        "1.0.7",
        202608021200,
        "1111111111111111111111111111111111111111",
        "13.0",
    );
    let (bytes, _) = manifest("1.0.6", 202608011200, remote_commit, "13.0");
    manual_manifest_result(&mut unpublished, bytes, ModelAvailability::Missing);
    assert_eq!(unpublished.state(), &UpdaterState::UnpublishedLocal);
}

#[test]
fn incompatible_automatic_check_stays_silent() {
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "14.0",
    );
    let mut updater = updater();
    let commands = updater.handle(UpdaterEvent::AutomaticCheckDue {
        launch_at: 1_000,
        now: 1_060,
        last_attempt: None,
    });
    let operation_id = match commands.last().unwrap() {
        UpdaterCommand::FetchManifest { operation_id, .. } => *operation_id,
        unexpected => panic!("expected automatic fetch, got {unexpected:?}"),
    };

    assert!(updater
        .handle(UpdaterEvent::ManifestReceived {
            operation_id,
            bytes,
            model: ModelAvailability::Missing,
        })
        .is_empty());
    assert_eq!(updater.state(), &UpdaterState::Idle);
}

#[test]
fn automatic_failure_is_silent_manual_failure_is_visible_and_late_result_is_ignored() {
    let mut automatic = updater();
    let commands = automatic.handle(UpdaterEvent::AutomaticCheckDue {
        launch_at: 1_000,
        now: 1_060,
        last_attempt: None,
    });
    let automatic_id = match commands.last().unwrap() {
        UpdaterCommand::FetchManifest { operation_id, .. } => *operation_id,
        unexpected => panic!("expected fetch command, got {unexpected:?}"),
    };
    assert!(automatic
        .handle(UpdaterEvent::ManifestFailed {
            operation_id: automatic_id,
            failure: UpdateFailure::Network,
        })
        .is_empty());
    assert_eq!(automatic.state(), &UpdaterState::Idle);

    let mut manual = updater();
    let commands = manual.handle(UpdaterEvent::ManualCheckRequested { now: 2_000 });
    let manual_id = match commands.last().unwrap() {
        UpdaterCommand::FetchManifest { operation_id, .. } => *operation_id,
        unexpected => panic!("expected fetch command, got {unexpected:?}"),
    };
    assert!(manual
        .handle(UpdaterEvent::ManifestFailed {
            operation_id: OperationId(manual_id.0 + 100),
            failure: UpdateFailure::Network,
        })
        .is_empty());
    assert!(matches!(manual.state(), UpdaterState::Checking { .. }));
    assert!(manual
        .handle(UpdaterEvent::ManifestFailed {
            operation_id: manual_id,
            failure: UpdateFailure::Network,
        })
        .is_empty());
    assert_eq!(
        manual.state(),
        &UpdaterState::Failed {
            failure: UpdateFailure::Network,
            retry: RetryAction::ManualCheck,
        }
    );
}

#[test]
fn explicit_confirmation_rechecks_model_and_never_silently_switches_to_full() {
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );
    let mut updater = updater();
    manual_manifest_result(
        &mut updater,
        bytes,
        ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
    );

    let commands = updater.handle(UpdaterEvent::DownloadRequested);
    let recheck_id = match commands.as_slice() {
        [UpdaterCommand::RecheckModel {
            operation_id,
            required_model,
        }] if required_model.id == "gigaam-v3-rnnt-v1" => *operation_id,
        unexpected => panic!("expected model recheck, got {unexpected:?}"),
    };
    assert!(matches!(
        updater.state(),
        UpdaterState::RecheckingModel { .. }
    ));

    assert!(updater
        .handle(UpdaterEvent::ModelRechecked {
            operation_id: recheck_id,
            model: ModelAvailability::Missing,
        })
        .is_empty());
    let (artifact, _) = offered_artifact(updater.state());
    assert_eq!(artifact.kind, ArtifactKind::Full);

    let commands = updater.handle(UpdaterEvent::DownloadRequested);
    let second_recheck = match commands.as_slice() {
        [UpdaterCommand::RecheckModel { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected second model recheck, got {unexpected:?}"),
    };
    let commands = updater.handle(UpdaterEvent::ModelRechecked {
        operation_id: second_recheck,
        model: ModelAvailability::Missing,
    });
    assert!(matches!(
        commands.as_slice(),
        [UpdaterCommand::DownloadAndVerify {
            artifact: SelectedArtifact {
                kind: ArtifactKind::Full,
                ..
            },
            ..
        }]
    ));
    assert!(matches!(updater.state(), UpdaterState::Downloading { .. }));
}

fn begin_verified_download(updater: &mut Updater) -> OperationId {
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );
    manual_manifest_result(
        updater,
        bytes,
        ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
    );
    let recheck_id = match updater.handle(UpdaterEvent::DownloadRequested).as_slice() {
        [UpdaterCommand::RecheckModel { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected model recheck, got {unexpected:?}"),
    };
    match updater
        .handle(UpdaterEvent::ModelRechecked {
            operation_id: recheck_id,
            model: ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
        })
        .as_slice()
    {
        [UpdaterCommand::DownloadAndVerify { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected download command, got {unexpected:?}"),
    }
}

#[test]
fn digest_failure_is_rejected_and_stale_download_result_cannot_change_state() {
    let mut updater = updater();
    let download_id = begin_verified_download(&mut updater);
    let stale_id = OperationId(download_id.0 + 1_000);

    assert!(updater
        .handle(UpdaterEvent::DownloadVerified {
            operation_id: stale_id,
            download: VerifiedDownload::from_verified_path(PathBuf::from("/cache/stale.dmg")),
        })
        .is_empty());
    assert!(matches!(updater.state(), UpdaterState::Downloading { .. }));

    assert!(updater
        .handle(UpdaterEvent::DownloadFailed {
            operation_id: download_id,
            failure: UpdateFailure::DigestMismatch,
        })
        .is_empty());
    assert_eq!(
        updater.state(),
        &UpdaterState::Failed {
            failure: UpdateFailure::DigestMismatch,
            retry: RetryAction::Download,
        }
    );
}

#[test]
fn verified_download_waits_for_explicit_open_and_quits_only_after_open_success() {
    let mut updater = updater();
    let download_id = begin_verified_download(&mut updater);
    let dmg = PathBuf::from("/cache/PTT2me-1.0.6-update.dmg");

    assert!(updater
        .handle(UpdaterEvent::DownloadVerified {
            operation_id: download_id,
            download: VerifiedDownload::from_verified_path(dmg.clone()),
        })
        .is_empty());
    assert!(matches!(
        updater.state(),
        UpdaterState::ReadyToInstall { path, .. } if path == &dmg
    ));

    let open_id = match updater.handle(UpdaterEvent::OpenRequested).as_slice() {
        [UpdaterCommand::OpenDmg { operation_id, path }] if path == &dmg => *operation_id,
        unexpected => panic!("expected explicit open command, got {unexpected:?}"),
    };
    assert!(updater
        .handle(UpdaterEvent::OpenCompleted {
            operation_id: open_id,
            result: Err(UpdateFailure::OpenDmg),
        })
        .is_empty());
    assert!(matches!(
        updater.state(),
        UpdaterState::ReadyToInstall { path, .. } if path == &dmg
    ));
    assert_eq!(updater.last_open_failure(), Some(UpdateFailure::OpenDmg));

    let retry_id = match updater.handle(UpdaterEvent::OpenRequested).as_slice() {
        [UpdaterCommand::OpenDmg { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected open retry, got {unexpected:?}"),
    };
    assert_eq!(
        updater.handle(UpdaterEvent::OpenCompleted {
            operation_id: retry_id,
            result: Ok(()),
        }),
        vec![UpdaterCommand::RequestOrderlyQuit]
    );
}

#[test]
fn only_one_async_operation_can_be_active() {
    let mut updater = updater();
    let first = updater.handle(UpdaterEvent::ManualCheckRequested { now: 1_000 });
    assert!(matches!(
        first.last(),
        Some(UpdaterCommand::FetchManifest { .. })
    ));
    assert!(updater
        .handle(UpdaterEvent::ManualCheckRequested { now: 1_001 })
        .is_empty());
    assert!(updater.handle(UpdaterEvent::DownloadRequested).is_empty());
}

fn verified_release() -> VerifiedRelease {
    let (bytes, key) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );
    verify_envelope(&bytes, &key).unwrap()
}

#[derive(Clone)]
struct CountingReader {
    bytes: Cursor<Vec<u8>>,
    consumed: Rc<Cell<usize>>,
}

impl CountingReader {
    fn new(bytes: Vec<u8>) -> (Self, Rc<Cell<usize>>) {
        let consumed = Rc::new(Cell::new(0));
        (
            Self {
                bytes: Cursor::new(bytes),
                consumed: consumed.clone(),
            },
            consumed,
        )
    }
}

impl Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.bytes.read(buffer)?;
        self.consumed.set(self.consumed.get() + read);
        Ok(read)
    }
}

struct PanicReader;

impl Read for PanicReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        panic!("reader must not be consumed")
    }
}

#[test]
fn cache_key_contains_version_build_and_artifact_kind_and_promotes_atomically() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();

    let path = cache_verified_download(
        cache.path(),
        &release,
        ArtifactKind::Update,
        &release.application_update,
        Cursor::new(b"verified PTT2me update dmg fixture"),
        Some(34),
    )
    .unwrap();

    assert_eq!(
        path.file_name().unwrap(),
        "PTT2me-1.0.6-202608011200-update-macos-arm64.dmg"
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        b"verified PTT2me update dmg fixture"
    );
    assert!(!cache
        .path()
        .join("PTT2me-1.0.6-202608011200-update-macos-arm64.dmg.part")
        .exists());
}

#[test]
fn verified_cached_artifact_is_reused_without_consuming_response_body() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();
    let first = cache_verified_download(
        cache.path(),
        &release,
        ArtifactKind::Full,
        &release.fresh_install,
        Cursor::new(b"verified PTT2me full dmg fixture"),
        Some(32),
    )
    .unwrap();

    let reused = cache_verified_download(
        cache.path(),
        &release,
        ArtifactKind::Full,
        &release.fresh_install,
        PanicReader,
        Some(32),
    )
    .unwrap();

    assert_eq!(reused, first);
}

#[test]
fn wrong_digest_short_body_and_content_length_mismatch_leave_no_cache_files() {
    let release = verified_release();

    for (body, content_length, expected) in [
        (vec![b'x'; 34], Some(34), UpdateFailure::DigestMismatch),
        (b"short".to_vec(), Some(34), UpdateFailure::BodySizeMismatch),
        (
            b"verified PTT2me update dmg fixture".to_vec(),
            Some(33),
            UpdateFailure::ContentLengthMismatch,
        ),
    ] {
        let cache = tempfile::tempdir().unwrap();
        assert_eq!(
            cache_verified_download(
                cache.path(),
                &release,
                ArtifactKind::Update,
                &release.application_update,
                Cursor::new(body),
                content_length,
            ),
            Err(expected)
        );
        assert!(fs::read_dir(cache.path()).unwrap().next().is_none());
    }
}

#[test]
fn long_body_is_stopped_at_signed_size_plus_one_before_disk_exhaustion() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();
    let (reader, consumed) = CountingReader::new(vec![b'x'; 1_000_000]);

    assert_eq!(
        cache_verified_download(
            cache.path(),
            &release,
            ArtifactKind::Update,
            &release.application_update,
            reader,
            None,
        ),
        Err(UpdateFailure::BodySizeMismatch)
    );
    assert_eq!(consumed.get(), 35);
    assert!(fs::read_dir(cache.path()).unwrap().next().is_none());
}

#[test]
fn stale_partial_is_removed_even_when_new_response_metadata_is_rejected() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();
    let partial = cache
        .path()
        .join("PTT2me-1.0.6-202608011200-update-macos-arm64.dmg.part");
    fs::write(&partial, b"stale partial").unwrap();

    assert_eq!(
        cache_verified_download(
            cache.path(),
            &release,
            ArtifactKind::Update,
            &release.application_update,
            PanicReader,
            Some(33),
        ),
        Err(UpdateFailure::ContentLengthMismatch)
    );
    assert!(!partial.exists());
}

#[test]
fn worker_rejects_oversized_descriptor_before_reading_or_creating_files() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();
    let oversized = ArtifactDescriptor {
        size: MAX_UPDATE_ARTIFACT_BYTES + 1,
        ..release.application_update.clone()
    };

    assert_eq!(
        cache_verified_download(
            cache.path(),
            &release,
            ArtifactKind::Update,
            &oversized,
            PanicReader,
            None,
        ),
        Err(UpdateFailure::InvalidArtifactSize)
    );
    assert!(fs::read_dir(cache.path()).unwrap().next().is_none());
}

#[test]
fn final_path_is_reopened_and_failed_post_rename_verification_is_cleaned_up() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();

    assert_eq!(
        cache_verified_download_with_promoter(
            cache.path(),
            &release,
            ArtifactKind::Full,
            &release.fresh_install,
            Cursor::new(b"verified PTT2me full dmg fixture"),
            Some(32),
            |partial: &Path, final_path: &Path| {
                fs::rename(partial, final_path)?;
                fs::write(final_path, b"corrupted after rename")
            },
        ),
        Err(UpdateFailure::DigestMismatch)
    );
    assert!(fs::read_dir(cache.path()).unwrap().next().is_none());
}

#[test]
fn bounded_manifest_reader_rejects_the_sixty_four_kibibyte_plus_one_byte() {
    let (reader, consumed) = CountingReader::new(vec![b'x'; 70_000]);

    assert_eq!(
        read_bounded_manifest(reader),
        Err(UpdateFailure::ManifestTooLarge)
    );
    assert_eq!(consumed.get(), 65_537);
}

#[test]
fn http_metadata_rejects_non_success_insecure_redirect_and_bad_content_length() {
    assert_eq!(
        validate_http_response_metadata(200, "https://example.test/file", Some("34"), Some(34)),
        Ok(Some(34))
    );
    assert_eq!(
        validate_http_response_metadata(500, "https://example.test/file", None, None),
        Err(UpdateFailure::HttpStatus)
    );
    assert_eq!(
        validate_http_response_metadata(200, "http://example.test/file", None, None),
        Err(UpdateFailure::InsecureTransport)
    );
    assert_eq!(
        validate_http_response_metadata(200, "https://example.test/file", Some("33"), Some(34)),
        Err(UpdateFailure::ContentLengthMismatch)
    );
}

struct FixtureFetch {
    body: Vec<u8>,
    content_length: Option<u64>,
}

impl UpdateFetch for FixtureFetch {
    fn fetch_manifest(&self, _url: &str) -> Result<Vec<u8>, UpdateFailure> {
        Err(UpdateFailure::Network)
    }

    fn fetch_artifact(
        &self,
        _artifact: &ArtifactDescriptor,
    ) -> Result<DownloadResponse, UpdateFailure> {
        Ok(DownloadResponse {
            content_length: self.content_length,
            reader: Box::new(Cursor::new(self.body.clone())),
        })
    }
}

struct FixedQuarantine(bool);

impl QuarantineChecker for FixedQuarantine {
    fn has_quarantine(&self, _path: &Path) -> Result<bool, UpdateFailure> {
        Ok(self.0)
    }
}

#[test]
fn artifact_worker_requires_quarantine_before_reporting_a_ready_path() {
    let release = verified_release();

    let rejected_cache = tempfile::tempdir().unwrap();
    let rejected = ArtifactWorker::new(
        FixtureFetch {
            body: b"verified PTT2me update dmg fixture".to_vec(),
            content_length: Some(34),
        },
        FileUpdateStorage::new(rejected_cache.path().to_owned()),
        FixedQuarantine(false),
    );
    assert_eq!(
        rejected.download(&release, ArtifactKind::Update, &release.application_update,),
        Err(UpdateFailure::QuarantineMissing)
    );
    assert!(fs::read_dir(rejected_cache.path())
        .unwrap()
        .next()
        .is_none());

    let accepted_cache = tempfile::tempdir().unwrap();
    let accepted = ArtifactWorker::new(
        FixtureFetch {
            body: b"verified PTT2me update dmg fixture".to_vec(),
            content_length: Some(34),
        },
        FileUpdateStorage::new(accepted_cache.path().to_owned()),
        FixedQuarantine(true),
    );
    let download = accepted
        .download(&release, ArtifactKind::Update, &release.application_update)
        .unwrap();
    assert!(download.path().is_file());
}
