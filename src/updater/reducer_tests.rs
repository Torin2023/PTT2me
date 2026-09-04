use std::cell::Cell;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File};
use std::io::{self, Cursor, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

use super::{
    cache_verified_download, cache_verified_download_with_promoter, descriptor_has_quarantine,
    file_url_for_path, next_automatic_check_at, read_bounded_manifest,
    validate_http_response_metadata, verify_and_open_dmg_with, ArtifactKind, ArtifactWorker,
    CheckReason, DownloadResponse, FileUpdateStorage, OperationId, QuarantineChecker, RetryAction,
    RetryContext, SelectedArtifact, UpdateFailure, UpdateFetch, Updater, UpdaterCommand,
    UpdaterEvent, UpdaterState, VerifiedDownload, AUTOMATIC_CHECK_INTERVAL_SECONDS,
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
        [UpdaterCommand::ScheduleAutomaticCheck(87_400), UpdaterCommand::PersistLastAttempt {
            operation_id,
            attempted_at: 1_000,
        }, UpdaterCommand::FetchManifest {
            operation_id: fetch_id,
            reason: CheckReason::Manual,
        }] if operation_id == fetch_id => *operation_id,
        unexpected => panic!("unexpected manual-check commands: {unexpected:?}"),
    };
    assert_eq!(
        updater.handle(UpdaterEvent::ManifestReceived {
            operation_id,
            bytes: bytes.clone(),
            model,
        }),
        vec![UpdaterCommand::StoreVerifiedManifest { bytes }]
    );
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
fn launch_cache_is_reverified_silently_without_recording_a_network_attempt() {
    let mut updater = updater();
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );

    let commands = updater.handle(UpdaterEvent::Launched {
        launch_at: 1_000,
        now: 1_000,
        last_attempt: None,
    });
    let cache_operation_id = match commands.as_slice() {
        [UpdaterCommand::ScheduleAutomaticCheck(1_060), UpdaterCommand::LoadCachedManifest { operation_id }] => {
            *operation_id
        }
        unexpected => panic!("unexpected launch commands: {unexpected:?}"),
    };

    assert!(updater
        .handle(UpdaterEvent::CachedManifestReceived {
            operation_id: cache_operation_id,
            bytes,
            model: ModelAvailability::Missing,
        })
        .is_empty());
    assert!(matches!(updater.state(), UpdaterState::Available { .. }));
}

#[test]
fn automatic_network_failure_restores_the_verified_cached_offer() {
    let mut updater = updater();
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );
    let cache_operation_id = match updater
        .handle(UpdaterEvent::Launched {
            launch_at: 1_000,
            now: 1_000,
            last_attempt: None,
        })
        .as_slice()
    {
        [_, UpdaterCommand::LoadCachedManifest { operation_id }] => *operation_id,
        unexpected => panic!("unexpected launch commands: {unexpected:?}"),
    };
    updater.handle(UpdaterEvent::CachedManifestReceived {
        operation_id: cache_operation_id,
        bytes,
        model: ModelAvailability::Missing,
    });
    let cached_offer = updater.state().clone();
    assert!(matches!(cached_offer, UpdaterState::Available { .. }));

    let commands = updater.handle(UpdaterEvent::AutomaticCheckDue {
        launch_at: 1_000,
        now: 1_060,
        last_attempt: None,
    });
    let operation_id = match commands.as_slice() {
        [UpdaterCommand::ScheduleAutomaticCheck(87_460), UpdaterCommand::PersistLastAttempt { operation_id, .. }, UpdaterCommand::FetchManifest {
            operation_id: fetch_id,
            reason: CheckReason::Automatic,
        }] if operation_id == fetch_id => *operation_id,
        unexpected => panic!("unexpected automatic commands: {unexpected:?}"),
    };

    updater.handle(UpdaterEvent::ManifestFailed {
        operation_id,
        failure: UpdateFailure::Network,
    });

    assert_eq!(updater.state(), &cached_offer);
}

#[test]
fn invalid_cached_envelope_stays_silent_and_late_cache_cannot_replace_manual_check() {
    let mut invalid = updater();
    let cache_id = match invalid
        .handle(UpdaterEvent::Launched {
            launch_at: 1_000,
            now: 1_000,
            last_attempt: None,
        })
        .as_slice()
    {
        [UpdaterCommand::ScheduleAutomaticCheck(1_060), UpdaterCommand::LoadCachedManifest { operation_id }] => {
            *operation_id
        }
        unexpected => panic!("unexpected launch commands: {unexpected:?}"),
    };
    assert!(invalid
        .handle(UpdaterEvent::CachedManifestReceived {
            operation_id: cache_id,
            bytes: b"not a signed envelope".to_vec(),
            model: ModelAvailability::Missing,
        })
        .is_empty());
    assert_eq!(invalid.state(), &UpdaterState::Idle);

    let mut stale = updater();
    let cache_id = match stale
        .handle(UpdaterEvent::Launched {
            launch_at: 1_000,
            now: 1_000,
            last_attempt: None,
        })
        .as_slice()
    {
        [_, UpdaterCommand::LoadCachedManifest { operation_id }] => *operation_id,
        unexpected => panic!("unexpected launch commands: {unexpected:?}"),
    };
    stale.handle(UpdaterEvent::ManualCheckRequested { now: 1_010 });
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );
    assert!(stale
        .handle(UpdaterEvent::CachedManifestReceived {
            operation_id: cache_id,
            bytes,
            model: ModelAvailability::Missing,
        })
        .is_empty());
    assert!(matches!(
        stale.state(),
        UpdaterState::Checking {
            reason: CheckReason::Manual,
            ..
        }
    ));
}

#[test]
fn verified_network_envelope_is_the_only_result_that_requests_cache_replacement() {
    let mut updater = updater();
    let commands = updater.handle(UpdaterEvent::ManualCheckRequested { now: 1_000 });
    let operation_id = match commands.as_slice() {
        [UpdaterCommand::ScheduleAutomaticCheck(87_400), UpdaterCommand::PersistLastAttempt {
            operation_id,
            attempted_at: 1_000,
        }, UpdaterCommand::FetchManifest {
            operation_id: fetch_id,
            reason: CheckReason::Manual,
        }] if operation_id == fetch_id => *operation_id,
        unexpected => panic!("unexpected check commands: {unexpected:?}"),
    };
    let (bytes, _) = manifest(
        "1.0.6",
        202608011200,
        "0123456789abcdef0123456789abcdef01234567",
        "13.0",
    );

    assert_eq!(
        updater.handle(UpdaterEvent::ManifestReceived {
            operation_id,
            bytes: bytes.clone(),
            model: ModelAvailability::Missing,
        }),
        vec![UpdaterCommand::StoreVerifiedManifest { bytes }]
    );
}

#[test]
fn persistence_failure_cancels_fetch_and_is_manual_visible_automatic_silent() {
    let mut manual = updater();
    let commands = manual.handle(UpdaterEvent::ManualCheckRequested { now: 1_000 });
    let manual_id = match commands.as_slice() {
        [UpdaterCommand::ScheduleAutomaticCheck(87_400), UpdaterCommand::PersistLastAttempt {
            operation_id,
            attempted_at: 1_000,
        }, UpdaterCommand::FetchManifest {
            operation_id: fetch_id,
            reason: CheckReason::Manual,
        }] if operation_id == fetch_id => *operation_id,
        unexpected => panic!("unexpected manual commands: {unexpected:?}"),
    };
    assert!(manual
        .handle(UpdaterEvent::LastAttemptPersistenceFailed {
            operation_id: manual_id,
        })
        .is_empty());
    assert!(matches!(
        manual.state(),
        UpdaterState::Failed {
            failure: UpdateFailure::Storage,
            retry: RetryAction::ManualCheck,
            ..
        }
    ));

    let mut automatic = updater();
    automatic.handle(UpdaterEvent::Launched {
        launch_at: 1_000,
        now: 1_000,
        last_attempt: None,
    });
    let commands = automatic.handle(UpdaterEvent::AutomaticCheckDue {
        launch_at: 1_000,
        now: 1_060,
        last_attempt: None,
    });
    let automatic_id = match commands.as_slice() {
        [UpdaterCommand::ScheduleAutomaticCheck(87_460), UpdaterCommand::PersistLastAttempt { operation_id, .. }, UpdaterCommand::FetchManifest {
            operation_id: fetch_id,
            reason: CheckReason::Automatic,
        }] if operation_id == fetch_id => *operation_id,
        unexpected => panic!("unexpected automatic commands: {unexpected:?}"),
    };
    automatic.handle(UpdaterEvent::LastAttemptPersistenceFailed {
        operation_id: automatic_id,
    });
    assert_eq!(automatic.state(), &UpdaterState::Idle);
}

#[test]
fn launch_and_suppressed_automatic_check_only_schedule_the_due_time() {
    let mut updater = updater();

    assert!(matches!(
        updater
            .handle(UpdaterEvent::Launched {
                launch_at: 1_000,
                now: 1_000,
                last_attempt: Some(950),
            })
            .as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(87_350),
            UpdaterCommand::LoadCachedManifest { .. }
        ]
    ));
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
fn early_automatic_callback_rearms_the_authoritative_launch_floor() {
    let mut updater = updater();
    assert!(matches!(
        updater
            .handle(UpdaterEvent::Launched {
                launch_at: 1_000,
                now: 1_000,
                last_attempt: None,
            })
            .as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(1_060),
            UpdaterCommand::LoadCachedManifest { .. }
        ]
    ));

    assert_eq!(
        updater.handle(UpdaterEvent::AutomaticCheckDue {
            launch_at: 1_000,
            now: 1_010,
            last_attempt: None,
        }),
        vec![UpdaterCommand::ScheduleAutomaticCheck(1_060)]
    );
    assert_eq!(updater.state(), &UpdaterState::Idle);

    assert!(matches!(
        updater
            .handle(UpdaterEvent::AutomaticCheckDue {
                launch_at: 1_000,
                now: 1_060,
                last_attempt: None,
            })
            .as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(87_460),
            UpdaterCommand::PersistLastAttempt {
                attempted_at: 1_060,
                ..
            },
            UpdaterCommand::FetchManifest {
                reason: CheckReason::Automatic,
                ..
            }
        ]
    ));
}

#[test]
fn busy_due_callback_rearms_and_the_retained_check_runs_after_completion() {
    let mut updater = updater();
    let manual_commands = updater.handle(UpdaterEvent::ManualCheckRequested { now: 1_000 });
    let manual_id = match manual_commands.last().unwrap() {
        UpdaterCommand::FetchManifest { operation_id, .. } => *operation_id,
        unexpected => panic!("expected manual fetch, got {unexpected:?}"),
    };

    assert_eq!(
        updater.handle(UpdaterEvent::AutomaticCheckDue {
            launch_at: 1_000,
            now: 87_400,
            last_attempt: Some(1_000),
        }),
        vec![UpdaterCommand::ScheduleAutomaticCheck(87_460)]
    );
    assert!(updater
        .handle(UpdaterEvent::ManifestFailed {
            operation_id: manual_id,
            failure: UpdateFailure::Network,
        })
        .is_empty());

    assert!(matches!(
        updater
            .handle(UpdaterEvent::AutomaticCheckDue {
                launch_at: 1_000,
                now: 87_460,
                last_attempt: Some(1_000),
            })
            .as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(173_860),
            UpdaterCommand::PersistLastAttempt {
                attempted_at: 87_460,
                ..
            },
            UpdaterCommand::FetchManifest {
                reason: CheckReason::Automatic,
                ..
            }
        ]
    ));
}

#[test]
fn backward_clock_normalization_does_not_postpone_the_same_deadline_twice() {
    let mut updater = updater();
    assert!(matches!(
        updater
            .handle(UpdaterEvent::Launched {
                launch_at: 1_000,
                now: 1_000,
                last_attempt: Some(2_000),
            })
            .as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(87_400),
            UpdaterCommand::LoadCachedManifest { .. }
        ]
    ));

    assert!(matches!(
        updater
            .handle(UpdaterEvent::AutomaticCheckDue {
                launch_at: 1_000,
                now: 87_400,
                last_attempt: Some(2_000),
            })
            .as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(173_800),
            UpdaterCommand::PersistLastAttempt {
                attempted_at: 87_400,
                ..
            },
            UpdaterCommand::FetchManifest {
                reason: CheckReason::Automatic,
                ..
            }
        ]
    ));
}

#[test]
fn accepted_manual_check_persists_immediately_and_replaces_the_automatic_deadline() {
    let mut updater = updater();
    assert!(matches!(
        updater
            .handle(UpdaterEvent::Launched {
                launch_at: 1_000,
                now: 1_000,
                last_attempt: None,
            })
            .as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(1_060),
            UpdaterCommand::LoadCachedManifest { .. }
        ]
    ));

    let commands = updater.handle(UpdaterEvent::ManualCheckRequested { now: 1_010 });
    assert!(matches!(
        commands.as_slice(),
        [
            UpdaterCommand::ScheduleAutomaticCheck(87_410),
            UpdaterCommand::PersistLastAttempt {
                attempted_at: 1_010,
                ..
            },
            UpdaterCommand::FetchManifest {
                reason: CheckReason::Manual,
                ..
            }
        ]
    ));
    assert_eq!(
        updater.handle(UpdaterEvent::AutomaticCheckDue {
            launch_at: 1_000,
            now: 1_060,
            last_attempt: None,
        }),
        vec![UpdaterCommand::ScheduleAutomaticCheck(87_410)]
    );
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
            UpdaterCommand::ScheduleAutomaticCheck(87_460),
            UpdaterCommand::PersistLastAttempt {
                attempted_at: 1_060,
                ..
            },
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
            UpdaterCommand::ScheduleAutomaticCheck(88_400),
            UpdaterCommand::PersistLastAttempt {
                attempted_at: 2_000,
                ..
            },
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

    assert!(matches!(
        updater
            .handle(UpdaterEvent::ManifestReceived {
                operation_id,
                bytes,
                model: ModelAvailability::Missing,
            })
            .as_slice(),
        [UpdaterCommand::StoreVerifiedManifest { .. }]
    ));
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
            context: None,
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
    assert!(matches!(
        updater.state(),
        UpdaterState::Failed {
            failure: UpdateFailure::DigestMismatch,
            retry: RetryAction::Download,
            context: Some(RetryContext::Download {
                artifact: SelectedArtifact {
                    kind: ArtifactKind::Update,
                    ..
                },
                ..
            }),
        }
    ));
}

#[test]
fn model_recheck_retry_preserves_the_offer_without_restarting_discovery() {
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
    let failed_id = match updater.handle(UpdaterEvent::DownloadRequested).as_slice() {
        [UpdaterCommand::RecheckModel { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected model recheck, got {unexpected:?}"),
    };
    assert!(updater
        .handle(UpdaterEvent::ModelRecheckFailed {
            operation_id: failed_id,
            failure: UpdateFailure::Storage,
        })
        .is_empty());
    assert!(matches!(
        updater.state(),
        UpdaterState::Failed {
            failure: UpdateFailure::Storage,
            retry: RetryAction::ModelRecheck,
            context: Some(RetryContext::ModelRecheck {
                disposition: super::OfferDisposition::Available,
                artifact: SelectedArtifact {
                    kind: ArtifactKind::Update,
                    ..
                },
                ..
            }),
        }
    ));

    let retry_id = match updater.handle(UpdaterEvent::RetryRequested).as_slice() {
        [UpdaterCommand::RecheckModel {
            operation_id,
            required_model,
        }] if required_model.id == "gigaam-v3-rnnt-v1" => *operation_id,
        unexpected => panic!("expected executable model retry, got {unexpected:?}"),
    };
    assert_ne!(retry_id, failed_id);
    assert!(updater
        .handle(UpdaterEvent::ModelRechecked {
            operation_id: failed_id,
            model: ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
        })
        .is_empty());
    assert!(matches!(
        updater.state(),
        UpdaterState::RecheckingModel {
            operation_id,
            ..
        } if operation_id == &retry_id
    ));
}

#[test]
fn download_retry_rechecks_the_required_model_before_fetching_again() {
    let mut updater = updater();
    let failed_id = begin_verified_download(&mut updater);
    assert!(updater
        .handle(UpdaterEvent::DownloadFailed {
            operation_id: failed_id,
            failure: UpdateFailure::Network,
        })
        .is_empty());

    assert!(matches!(
        updater.state(),
        UpdaterState::Failed {
            retry: RetryAction::Download,
            context: Some(RetryContext::Download {
                disposition: super::OfferDisposition::Available,
                artifact: SelectedArtifact {
                    kind: ArtifactKind::Update,
                    ..
                },
                ..
            }),
            ..
        }
    ));
    let retry_id = match updater.handle(UpdaterEvent::RetryRequested).as_slice() {
        [UpdaterCommand::RecheckModel {
            operation_id,
            required_model,
        }] if required_model.id == "gigaam-v3-rnnt-v1" => *operation_id,
        unexpected => panic!("expected model-safe download retry, got {unexpected:?}"),
    };
    assert_ne!(retry_id, failed_id);
    assert!(matches!(
        updater.state(),
        UpdaterState::RecheckingModel {
            operation_id,
            artifact: SelectedArtifact {
                kind: ArtifactKind::Update,
                ..
            },
            ..
        } if operation_id == &retry_id
    ));

    assert!(matches!(
        updater
            .handle(UpdaterEvent::ModelRechecked {
                operation_id: retry_id,
                model: ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned()),
            })
            .as_slice(),
        [UpdaterCommand::DownloadAndVerify {
            artifact: SelectedArtifact {
                kind: ArtifactKind::Update,
                ..
            },
            ..
        }]
    ));
}

#[test]
fn download_retry_model_drift_returns_to_full_offer_for_fresh_confirmation() {
    let mut updater = updater();
    let failed_id = begin_verified_download(&mut updater);
    assert!(updater
        .handle(UpdaterEvent::DownloadFailed {
            operation_id: failed_id,
            failure: UpdateFailure::Network,
        })
        .is_empty());
    let retry_id = match updater.handle(UpdaterEvent::RetryRequested).as_slice() {
        [UpdaterCommand::RecheckModel { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected retry model recheck, got {unexpected:?}"),
    };

    assert!(updater
        .handle(UpdaterEvent::ModelRechecked {
            operation_id: retry_id,
            model: ModelAvailability::Missing,
        })
        .is_empty());
    let (artifact, disposition) = offered_artifact(updater.state());
    assert_eq!(disposition, "available");
    assert_eq!(artifact.kind, ArtifactKind::Full);

    let confirmation_recheck = match updater.handle(UpdaterEvent::DownloadRequested).as_slice() {
        [UpdaterCommand::RecheckModel { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected fresh Full confirmation, got {unexpected:?}"),
    };
    assert!(matches!(
        updater
            .handle(UpdaterEvent::ModelRechecked {
                operation_id: confirmation_recheck,
                model: ModelAvailability::Missing,
            })
            .as_slice(),
        [UpdaterCommand::DownloadAndVerify {
            artifact: SelectedArtifact {
                kind: ArtifactKind::Full,
                ..
            },
            ..
        }]
    ));
}

#[test]
fn explicit_open_is_one_verified_boundary_with_stale_suppression_and_quit_after_success() {
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
        [UpdaterCommand::VerifyAndOpenDmg {
            operation_id,
            release,
            artifact,
            expected_path,
        }] if release.version.to_string() == "1.0.6"
            && artifact.kind == ArtifactKind::Update
            && expected_path == &dmg =>
        {
            *operation_id
        }
        unexpected => panic!("expected one verified-open command, got {unexpected:?}"),
    };
    assert!(matches!(
        updater.state(),
        UpdaterState::Opening { operation_id, .. } if operation_id == &open_id
    ));

    assert!(updater
        .handle(UpdaterEvent::OpenCompleted {
            operation_id: OperationId(open_id.0 + 1_000),
            result: Ok(()),
        })
        .is_empty());
    assert!(matches!(
        updater.state(),
        UpdaterState::Opening { operation_id, .. } if operation_id == &open_id
    ));

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
        [UpdaterCommand::VerifyAndOpenDmg { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected verified-open retry, got {unexpected:?}"),
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
fn failed_open_time_verification_never_opens_and_exposes_a_download_retry() {
    let mut updater = updater();
    let download_id = begin_verified_download(&mut updater);
    assert!(updater
        .handle(UpdaterEvent::DownloadVerified {
            operation_id: download_id,
            download: VerifiedDownload::from_verified_path(PathBuf::from("/cache/ready.dmg")),
        })
        .is_empty());
    let open_id = match updater.handle(UpdaterEvent::OpenRequested).as_slice() {
        [UpdaterCommand::VerifyAndOpenDmg { operation_id, .. }] => *operation_id,
        unexpected => panic!("expected verified-open command, got {unexpected:?}"),
    };

    assert!(updater
        .handle(UpdaterEvent::OpenCompleted {
            operation_id: open_id,
            result: Err(UpdateFailure::DigestMismatch),
        })
        .is_empty());
    assert!(matches!(
        updater.state(),
        UpdaterState::Failed {
            failure: UpdateFailure::DigestMismatch,
            retry: RetryAction::Download,
            context: Some(RetryContext::Download {
                disposition: super::OfferDisposition::Available,
                ..
            }),
        }
    ));
    assert!(matches!(
        updater.handle(UpdaterEvent::RetryRequested).as_slice(),
        [UpdaterCommand::RecheckModel { .. }]
    ));
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
fn verified_download_receives_quarantine_before_promotion() {
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
    let file = File::open(path).unwrap();

    assert_eq!(descriptor_has_quarantine(file.as_raw_fd()), Ok(true));
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

struct OfflineFetch;

impl UpdateFetch for OfflineFetch {
    fn fetch_manifest(&self, _url: &str) -> Result<Vec<u8>, UpdateFailure> {
        Err(UpdateFailure::Network)
    }

    fn fetch_artifact(
        &self,
        _artifact: &ArtifactDescriptor,
    ) -> Result<DownloadResponse, UpdateFailure> {
        Err(UpdateFailure::Network)
    }
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

#[test]
fn artifact_worker_uses_quarantined_cache_before_network_and_cleans_stale_partial() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();
    let final_path = cache_verified_download(
        cache.path(),
        &release,
        ArtifactKind::Update,
        &release.application_update,
        Cursor::new(b"verified PTT2me update dmg fixture"),
        Some(34),
    )
    .unwrap();
    let partial_path = final_path.with_extension("dmg.part");
    fs::write(&partial_path, b"stale interrupted body").unwrap();

    let worker = ArtifactWorker::new(
        OfflineFetch,
        FileUpdateStorage::new(cache.path().to_owned()),
        FixedQuarantine(true),
    );
    let reused = worker
        .download(&release, ArtifactKind::Update, &release.application_update)
        .unwrap();

    assert_eq!(reused.path(), final_path);
    assert!(!partial_path.exists());
}

#[test]
fn offline_cache_hit_still_fails_closed_when_quarantine_is_missing() {
    let cache = tempfile::tempdir().unwrap();
    let release = verified_release();
    let final_path = cache_verified_download(
        cache.path(),
        &release,
        ArtifactKind::Full,
        &release.fresh_install,
        Cursor::new(b"verified PTT2me full dmg fixture"),
        Some(32),
    )
    .unwrap();
    let worker = ArtifactWorker::new(
        OfflineFetch,
        FileUpdateStorage::new(cache.path().to_owned()),
        FixedQuarantine(false),
    );

    assert_eq!(
        worker.download(&release, ArtifactKind::Full, &release.fresh_install),
        Err(UpdateFailure::QuarantineMissing)
    );
    assert!(!final_path.exists());
}

fn cached_update(
    release: &VerifiedRelease,
    quarantine: bool,
) -> (tempfile::TempDir, PathBuf, SelectedArtifact) {
    let cache = tempfile::tempdir().unwrap();
    let path = cache_verified_download(
        cache.path(),
        release,
        ArtifactKind::Update,
        &release.application_update,
        Cursor::new(b"verified PTT2me update dmg fixture"),
        Some(34),
    )
    .unwrap();
    if quarantine {
        set_quarantine(&path);
    } else {
        clear_quarantine(&path);
    }
    (
        cache,
        path,
        SelectedArtifact {
            kind: ArtifactKind::Update,
            descriptor: release.application_update.clone(),
        },
    )
}

fn set_quarantine(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let attribute = b"com.apple.quarantine\0";
    let value = b"0081;00000000;PTT2meTests;";
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            attribute.as_ptr().cast(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    assert_eq!(result, 0, "setxattr failed: {}", io::Error::last_os_error());
}

fn clear_quarantine(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let attribute = b"com.apple.quarantine\0";
    let result = unsafe { libc::removexattr(path.as_ptr(), attribute.as_ptr().cast(), 0) };
    assert_eq!(
        result,
        0,
        "removexattr failed: {}",
        io::Error::last_os_error()
    );
}

#[test]
fn verified_open_boundary_hashes_quarantine_checks_and_opens_one_held_descriptor() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || Ok(()),
            |opened_path| {
                assert_eq!(opened_path, path);
                true
            },
        ),
        Ok(())
    );
}

#[test]
fn verified_open_boundary_reports_workspace_false_without_lying_about_success() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || Ok(()),
            |_| false,
        ),
        Err(UpdateFailure::OpenDmg)
    );
}

#[test]
fn workspace_file_url_preserves_non_utf8_native_path_bytes() {
    let path = PathBuf::from(OsStr::from_bytes(b"/tmp/PTT2me-cache-\xff.dmg"));

    let url = file_url_for_path(&path).unwrap();
    let round_trip = unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) };

    assert_eq!(round_trip.to_bytes(), path.as_os_str().as_bytes());
}

#[test]
fn verified_open_boundary_rejects_corruption_before_workspace_open() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);
    fs::write(&path, vec![b'x'; 34]).unwrap();

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || Ok(()),
            |_| panic!("workspace must not receive corrupt bytes"),
        ),
        Err(UpdateFailure::DigestMismatch)
    );
}

#[test]
fn verified_open_boundary_rejects_valid_bytes_at_a_noncanonical_cache_path() {
    let release = verified_release();
    let (cache, _path, artifact) = cached_update(&release, true);
    let other_path = cache.path().join("other.dmg");
    fs::write(&other_path, b"verified PTT2me update dmg fixture").unwrap();
    set_quarantine(&other_path);

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &other_path,
            || Ok(()),
            |_| panic!("workspace must not receive a noncanonical cache path"),
        ),
        Err(UpdateFailure::DigestMismatch)
    );
}

#[test]
fn verified_open_boundary_rejects_regular_replacement_at_post_hash_barrier() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || {
                fs::remove_file(&path)?;
                fs::write(&path, b"verified PTT2me update dmg fixture")?;
                set_quarantine(&path);
                Ok(())
            },
            |_| panic!("workspace must not receive a replacement inode"),
        ),
        Err(UpdateFailure::DigestMismatch)
    );
}

#[test]
fn verified_open_boundary_rejects_symlink_replacement_at_post_hash_barrier() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);
    let target = cache.path().join("replacement-target.dmg");
    fs::write(&target, b"verified PTT2me update dmg fixture").unwrap();
    set_quarantine(&target);

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || {
                fs::remove_file(&path)?;
                std::os::unix::fs::symlink(&target, &path)?;
                Ok(())
            },
            |_| panic!("workspace must not receive a symlink replacement"),
        ),
        Err(UpdateFailure::DigestMismatch)
    );
}

#[test]
fn quarantine_is_read_from_held_descriptor_not_quarantined_replacement_path() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, false);
    let barrier_ran = Cell::new(false);

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || {
                barrier_ran.set(true);
                fs::remove_file(&path)?;
                fs::write(&path, b"verified PTT2me update dmg fixture")?;
                set_quarantine(&path);
                Ok(())
            },
            |_| panic!("workspace must not receive an unquarantined descriptor"),
        ),
        Err(UpdateFailure::QuarantineMissing)
    );
    assert!(barrier_ran.get());
}

#[test]
fn verified_open_boundary_rejects_group_or_world_writable_cache_root() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);
    fs::set_permissions(cache.path(), fs::Permissions::from_mode(0o777)).unwrap();

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || Ok(()),
            |_| panic!("workspace must not receive a path from an uncontrolled cache"),
        ),
        Err(UpdateFailure::Storage)
    );
}

#[test]
fn verified_open_boundary_rejects_group_or_world_writable_artifact() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || Ok(()),
            |_| panic!("workspace must not receive an uncontrolled artifact"),
        ),
        Err(UpdateFailure::Storage)
    );
}

#[test]
fn path_based_workspace_api_cannot_bind_a_same_uid_swap_after_final_identity_check() {
    let release = verified_release();
    let (cache, path, artifact) = cached_update(&release, true);

    // This closure is the path-based NSWorkspace call site, after the final
    // descriptor/path identity check. A hostile concurrent process running as
    // the same UID can still swap the path in this final window. Holding the
    // descriptor narrows the race but cannot cryptographically bind openURL.
    assert_eq!(
        verify_and_open_dmg_with(
            cache.path(),
            &release,
            &artifact,
            &path,
            || Ok(()),
            |workspace_path| {
                fs::remove_file(workspace_path).unwrap();
                fs::write(workspace_path, b"same-uid post-check replacement").unwrap();
                true
            },
        ),
        Ok(())
    );
    assert_eq!(fs::read(path).unwrap(), b"same-uid post-check replacement");
}
