use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSThread, NSURL};

use crate::constants::{
    MAX_UPDATE_ARTIFACT_BYTES, MAX_UPDATE_MANIFEST_BYTES, UPDATE_CONNECT_TIMEOUT_SECONDS,
    UPDATE_MAX_REDIRECTS, UPDATE_OVERALL_TIMEOUT_SECONDS, UPDATE_READ_TIMEOUT_SECONDS,
};
use crate::update_manifest::{
    classify_release, select_artifact, verify_artifact, verify_envelope, ArtifactDescriptor,
    InstalledBuild, MacOsVersion, ManifestError, ModelAvailability, ReleaseDisposition,
    RequiredModel, VerifiedRelease,
};

pub const AUTOMATIC_CHECK_INTERVAL_SECONDS: u64 = 24 * 60 * 60;
pub const FIRST_AUTOMATIC_CHECK_DELAY_SECONDS: u64 = 60;

// Pure reducer contract. Commands describe side effects; operation results
// re-enter through events and stale operation IDs are ignored.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationId(pub u64);

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
    InvalidArtifactSize,
    ContentLengthMismatch,
    BodySizeMismatch,
    ManifestTooLarge,
    HttpStatus,
    InsecureTransport,
    QuarantineMissing,
    WrongThread,
    OpenDmg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Full,
    Update,
}

impl ArtifactKind {
    const fn cache_label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Update => "update",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedArtifact {
    pub kind: ArtifactKind,
    pub descriptor: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDownload {
    path: PathBuf,
}

impl VerifiedDownload {
    fn from_verified_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    ManualCheck,
    Download,
    ModelRecheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferDisposition {
    Available,
    DivergedLocal,
    RepairRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryContext {
    ModelRecheck {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
        disposition: OfferDisposition,
    },
    Download {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
        disposition: OfferDisposition,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterState {
    Idle,
    Checking {
        reason: CheckReason,
        operation_id: OperationId,
    },
    Current,
    Available {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
    },
    DivergedLocal {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
    },
    RepairRequired {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
    },
    Incompatible {
        release: Box<VerifiedRelease>,
        required_macos: MacOsVersion,
    },
    UnpublishedLocal,
    RecheckingModel {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
        disposition: OfferDisposition,
        operation_id: OperationId,
    },
    Downloading {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
        disposition: OfferDisposition,
        operation_id: OperationId,
    },
    ReadyToInstall {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
        disposition: OfferDisposition,
        path: PathBuf,
    },
    Opening {
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
        disposition: OfferDisposition,
        path: PathBuf,
        operation_id: OperationId,
    },
    Failed {
        failure: UpdateFailure,
        retry: RetryAction,
        context: Option<RetryContext>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterCommand {
    PersistLastAttempt {
        operation_id: OperationId,
        attempted_at: u64,
    },
    ScheduleAutomaticCheck(u64),
    LoadCachedManifest {
        operation_id: OperationId,
    },
    StoreVerifiedManifest {
        bytes: Vec<u8>,
    },
    FetchManifest {
        operation_id: OperationId,
        reason: CheckReason,
    },
    RecheckModel {
        operation_id: OperationId,
        required_model: RequiredModel,
    },
    DownloadAndVerify {
        operation_id: OperationId,
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
    },
    VerifyAndOpenDmg {
        operation_id: OperationId,
        release: Box<VerifiedRelease>,
        artifact: SelectedArtifact,
        expected_path: PathBuf,
    },
    RequestOrderlyQuit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterEvent {
    Launched {
        launch_at: u64,
        now: u64,
        last_attempt: Option<u64>,
    },
    AutomaticCheckDue {
        launch_at: u64,
        now: u64,
        last_attempt: Option<u64>,
    },
    ManualCheckRequested {
        now: u64,
    },
    CachedManifestReceived {
        operation_id: OperationId,
        bytes: Vec<u8>,
        model: ModelAvailability,
    },
    LastAttemptPersistenceFailed {
        operation_id: OperationId,
    },
    ManifestReceived {
        operation_id: OperationId,
        bytes: Vec<u8>,
        model: ModelAvailability,
    },
    ManifestFailed {
        operation_id: OperationId,
        failure: UpdateFailure,
    },
    DownloadRequested,
    ModelRechecked {
        operation_id: OperationId,
        model: ModelAvailability,
    },
    ModelRecheckFailed {
        operation_id: OperationId,
        failure: UpdateFailure,
    },
    DownloadVerified {
        operation_id: OperationId,
        download: VerifiedDownload,
    },
    DownloadFailed {
        operation_id: OperationId,
        failure: UpdateFailure,
    },
    RetryRequested,
    OpenRequested,
    OpenCompleted {
        operation_id: OperationId,
        result: Result<(), UpdateFailure>,
    },
}

pub struct Updater {
    installed: InstalledBuild,
    public_key: [u8; 32],
    running_macos: MacOsVersion,
    state: UpdaterState,
    next_operation_id: u64,
    active_operation: Option<OperationId>,
    automatic_check_at: Option<u64>,
    pending_cache_operation: Option<OperationId>,
    last_open_failure: Option<UpdateFailure>,
}

impl Updater {
    pub const fn new(
        installed: InstalledBuild,
        public_key: [u8; 32],
        running_macos: MacOsVersion,
    ) -> Self {
        Self {
            installed,
            public_key,
            running_macos,
            state: UpdaterState::Idle,
            next_operation_id: 1,
            active_operation: None,
            automatic_check_at: None,
            pending_cache_operation: None,
            last_open_failure: None,
        }
    }

    pub const fn state(&self) -> &UpdaterState {
        &self.state
    }

    pub const fn last_open_failure(&self) -> Option<UpdateFailure> {
        self.last_open_failure
    }

    pub fn handle(&mut self, event: UpdaterEvent) -> Vec<UpdaterCommand> {
        match event {
            UpdaterEvent::Launched {
                launch_at,
                now,
                last_attempt,
            } => {
                let deadline = next_automatic_check_at(launch_at, now, last_attempt);
                self.automatic_check_at = Some(deadline);
                let operation_id = self.allocate_detached_operation();
                self.pending_cache_operation = Some(operation_id);
                vec![
                    UpdaterCommand::ScheduleAutomaticCheck(deadline),
                    UpdaterCommand::LoadCachedManifest { operation_id },
                ]
            }
            UpdaterEvent::AutomaticCheckDue {
                launch_at,
                now,
                last_attempt,
            } => {
                let deadline = *self
                    .automatic_check_at
                    .get_or_insert_with(|| next_automatic_check_at(launch_at, now, last_attempt));
                if now < deadline {
                    return vec![UpdaterCommand::ScheduleAutomaticCheck(deadline)];
                }
                if !self.can_start_operation() {
                    let retry_at = now.saturating_add(FIRST_AUTOMATIC_CHECK_DELAY_SECONDS);
                    self.automatic_check_at = Some(retry_at);
                    return vec![UpdaterCommand::ScheduleAutomaticCheck(retry_at)];
                }
                self.start_check(CheckReason::Automatic, now)
            }
            UpdaterEvent::ManualCheckRequested { now } => {
                if !self.can_start_operation() {
                    return Vec::new();
                }
                self.start_check(CheckReason::Manual, now)
            }
            UpdaterEvent::CachedManifestReceived {
                operation_id,
                bytes,
                model,
            } => self.receive_cached_manifest(operation_id, &bytes, model),
            UpdaterEvent::LastAttemptPersistenceFailed { operation_id } => {
                self.last_attempt_persistence_failed(operation_id)
            }
            UpdaterEvent::ManifestReceived {
                operation_id,
                bytes,
                model,
            } => self.receive_manifest(operation_id, &bytes, model),
            UpdaterEvent::ManifestFailed {
                operation_id,
                failure,
            } => self.manifest_failed(operation_id, failure),
            UpdaterEvent::DownloadRequested => self.request_download(),
            UpdaterEvent::ModelRechecked {
                operation_id,
                model,
            } => self.model_rechecked(operation_id, model),
            UpdaterEvent::ModelRecheckFailed {
                operation_id,
                failure,
            } => self.model_recheck_failed(operation_id, failure),
            UpdaterEvent::DownloadVerified {
                operation_id,
                download,
            } => self.download_verified(operation_id, download),
            UpdaterEvent::DownloadFailed {
                operation_id,
                failure,
            } => self.download_failed(operation_id, failure),
            UpdaterEvent::RetryRequested => self.retry_failed_operation(),
            UpdaterEvent::OpenRequested => self.request_open(),
            UpdaterEvent::OpenCompleted {
                operation_id,
                result,
            } => self.open_completed(operation_id, result),
        }
    }

    fn can_start_operation(&self) -> bool {
        self.active_operation.is_none()
    }

    fn allocate_operation(&mut self) -> OperationId {
        let operation_id = self.allocate_detached_operation();
        self.active_operation = Some(operation_id);
        operation_id
    }

    fn allocate_detached_operation(&mut self) -> OperationId {
        let operation_id = OperationId(self.next_operation_id);
        self.next_operation_id = self.next_operation_id.wrapping_add(1).max(1);
        operation_id
    }

    fn finish_operation(&mut self, operation_id: OperationId) -> bool {
        if self.active_operation != Some(operation_id) {
            return false;
        }
        self.active_operation = None;
        true
    }

    fn start_check(&mut self, reason: CheckReason, now: u64) -> Vec<UpdaterCommand> {
        self.pending_cache_operation = None;
        let operation_id = self.allocate_operation();
        let next_attempt = now.saturating_add(AUTOMATIC_CHECK_INTERVAL_SECONDS);
        self.automatic_check_at = Some(next_attempt);
        self.state = UpdaterState::Checking {
            reason,
            operation_id,
        };
        vec![
            UpdaterCommand::ScheduleAutomaticCheck(next_attempt),
            UpdaterCommand::PersistLastAttempt {
                operation_id,
                attempted_at: now,
            },
            UpdaterCommand::FetchManifest {
                operation_id,
                reason,
            },
        ]
    }

    fn receive_cached_manifest(
        &mut self,
        operation_id: OperationId,
        bytes: &[u8],
        model: ModelAvailability,
    ) -> Vec<UpdaterCommand> {
        if self.pending_cache_operation != Some(operation_id)
            || self.active_operation.is_some()
            || self.state != UpdaterState::Idle
        {
            return Vec::new();
        }
        self.pending_cache_operation = None;
        if let Ok(release) = verify_envelope(bytes, &self.public_key) {
            self.project_release(release, model, CheckReason::Automatic);
        }
        Vec::new()
    }

    fn last_attempt_persistence_failed(
        &mut self,
        operation_id: OperationId,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::Checking {
            reason,
            operation_id: expected,
        } = self.state
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        self.state = if reason == CheckReason::Manual {
            UpdaterState::Failed {
                failure: UpdateFailure::Storage,
                retry: RetryAction::ManualCheck,
                context: None,
            }
        } else {
            UpdaterState::Idle
        };
        Vec::new()
    }

    fn receive_manifest(
        &mut self,
        operation_id: OperationId,
        bytes: &[u8],
        model: ModelAvailability,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::Checking {
            reason,
            operation_id: expected,
        } = self.state
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        match verify_envelope(bytes, &self.public_key) {
            Ok(release) => {
                self.project_release(release, model, reason);
                return vec![UpdaterCommand::StoreVerifiedManifest {
                    bytes: bytes.to_vec(),
                }];
            }
            Err(_) => {
                self.state = if reason == CheckReason::Manual {
                    UpdaterState::Failed {
                        failure: UpdateFailure::UntrustedManifest,
                        retry: RetryAction::ManualCheck,
                        context: None,
                    }
                } else {
                    UpdaterState::Idle
                };
            }
        }
        Vec::new()
    }

    fn manifest_failed(
        &mut self,
        operation_id: OperationId,
        failure: UpdateFailure,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::Checking {
            reason,
            operation_id: expected,
        } = self.state
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        self.state = if reason == CheckReason::Automatic {
            UpdaterState::Idle
        } else {
            UpdaterState::Failed {
                failure,
                retry: RetryAction::ManualCheck,
                context: None,
            }
        };
        Vec::new()
    }

    fn project_release(
        &mut self,
        release: VerifiedRelease,
        model: ModelAvailability,
        reason: CheckReason,
    ) {
        let disposition = classify_release(&self.installed, &release);
        if disposition == ReleaseDisposition::UnpublishedLocal {
            self.state = UpdaterState::UnpublishedLocal;
            return;
        }
        if self.running_macos < release.minimum_macos {
            if reason == CheckReason::Automatic {
                self.state = UpdaterState::Idle;
                return;
            }
            let required_macos = release.minimum_macos;
            self.state = UpdaterState::Incompatible {
                release: Box::new(release),
                required_macos,
            };
            return;
        }

        let artifact = selected_artifact(&release, &model);
        self.state = match disposition {
            ReleaseDisposition::Available => UpdaterState::Available {
                release: Box::new(release),
                artifact,
            },
            ReleaseDisposition::DivergedLocal => UpdaterState::DivergedLocal {
                release: Box::new(release),
                artifact,
            },
            ReleaseDisposition::Current if artifact.kind == ArtifactKind::Full => {
                UpdaterState::RepairRequired {
                    release: Box::new(release),
                    artifact,
                }
            }
            ReleaseDisposition::Current => UpdaterState::Current,
            ReleaseDisposition::UnpublishedLocal => UpdaterState::UnpublishedLocal,
        };
    }

    fn request_download(&mut self) -> Vec<UpdaterCommand> {
        if !self.can_start_operation() {
            return Vec::new();
        }
        let Some((release, artifact, disposition)) = download_offer(&self.state) else {
            return Vec::new();
        };
        let operation_id = self.allocate_operation();
        let required_model = release.required_model.clone();
        self.state = UpdaterState::RecheckingModel {
            release,
            artifact,
            disposition,
            operation_id,
        };
        vec![UpdaterCommand::RecheckModel {
            operation_id,
            required_model,
        }]
    }

    fn model_rechecked(
        &mut self,
        operation_id: OperationId,
        model: ModelAvailability,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::RecheckingModel {
            release,
            artifact: previous,
            disposition,
            operation_id: expected,
        } = self.state.clone()
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        let artifact = selected_artifact(&release, &model);
        if artifact.kind != previous.kind {
            self.state = project_offer_after_recheck(release, artifact, disposition);
            return Vec::new();
        }

        let download_id = self.allocate_operation();
        self.state = UpdaterState::Downloading {
            release: release.clone(),
            artifact: artifact.clone(),
            disposition,
            operation_id: download_id,
        };
        vec![UpdaterCommand::DownloadAndVerify {
            operation_id: download_id,
            release,
            artifact,
        }]
    }

    fn model_recheck_failed(
        &mut self,
        operation_id: OperationId,
        failure: UpdateFailure,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::RecheckingModel {
            release,
            artifact,
            disposition,
            operation_id: expected,
        } = self.state.clone()
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        self.state = UpdaterState::Failed {
            failure,
            retry: RetryAction::ModelRecheck,
            context: Some(RetryContext::ModelRecheck {
                release,
                artifact,
                disposition,
            }),
        };
        Vec::new()
    }

    fn download_verified(
        &mut self,
        operation_id: OperationId,
        download: VerifiedDownload,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::Downloading {
            release,
            artifact,
            disposition,
            operation_id: expected,
        } = self.state.clone()
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        self.state = UpdaterState::ReadyToInstall {
            release,
            artifact,
            disposition,
            path: download.path,
        };
        Vec::new()
    }

    fn download_failed(
        &mut self,
        operation_id: OperationId,
        failure: UpdateFailure,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::Downloading {
            release,
            artifact,
            disposition,
            operation_id: expected,
        } = self.state.clone()
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        self.state = UpdaterState::Failed {
            failure,
            retry: RetryAction::Download,
            context: Some(RetryContext::Download {
                release,
                artifact,
                disposition,
            }),
        };
        Vec::new()
    }

    fn retry_failed_operation(&mut self) -> Vec<UpdaterCommand> {
        if !self.can_start_operation() {
            return Vec::new();
        }
        let UpdaterState::Failed {
            retry,
            context: Some(context),
            ..
        } = self.state.clone()
        else {
            return Vec::new();
        };
        match (retry, context) {
            (
                RetryAction::ModelRecheck,
                RetryContext::ModelRecheck {
                    release,
                    artifact,
                    disposition,
                },
            ) => {
                let operation_id = self.allocate_operation();
                let required_model = release.required_model.clone();
                self.state = UpdaterState::RecheckingModel {
                    release,
                    artifact,
                    disposition,
                    operation_id,
                };
                vec![UpdaterCommand::RecheckModel {
                    operation_id,
                    required_model,
                }]
            }
            (
                RetryAction::Download,
                RetryContext::Download {
                    release,
                    artifact,
                    disposition,
                },
            ) => {
                let operation_id = self.allocate_operation();
                let required_model = release.required_model.clone();
                self.state = UpdaterState::RecheckingModel {
                    release,
                    artifact,
                    disposition,
                    operation_id,
                };
                vec![UpdaterCommand::RecheckModel {
                    operation_id,
                    required_model,
                }]
            }
            _ => Vec::new(),
        }
    }

    fn request_open(&mut self) -> Vec<UpdaterCommand> {
        if !self.can_start_operation() {
            return Vec::new();
        }
        let UpdaterState::ReadyToInstall {
            release,
            artifact,
            disposition,
            path,
        } = self.state.clone()
        else {
            return Vec::new();
        };
        let operation_id = self.allocate_operation();
        self.last_open_failure = None;
        self.state = UpdaterState::Opening {
            release: release.clone(),
            artifact: artifact.clone(),
            disposition,
            path: path.clone(),
            operation_id,
        };
        vec![UpdaterCommand::VerifyAndOpenDmg {
            operation_id,
            release,
            artifact,
            expected_path: path,
        }]
    }

    fn open_completed(
        &mut self,
        operation_id: OperationId,
        result: Result<(), UpdateFailure>,
    ) -> Vec<UpdaterCommand> {
        let UpdaterState::Opening {
            release,
            artifact,
            disposition,
            path,
            operation_id: expected,
        } = self.state.clone()
        else {
            return Vec::new();
        };
        if expected != operation_id || !self.finish_operation(operation_id) {
            return Vec::new();
        }
        match result {
            Ok(()) => vec![UpdaterCommand::RequestOrderlyQuit],
            Err(UpdateFailure::OpenDmg) => {
                self.last_open_failure = Some(UpdateFailure::OpenDmg);
                self.state = UpdaterState::ReadyToInstall {
                    release,
                    artifact,
                    disposition,
                    path,
                };
                Vec::new()
            }
            Err(failure) => {
                self.last_open_failure = Some(failure);
                self.state = failed_download_context(failure, release, artifact, disposition);
                Vec::new()
            }
        }
    }
}

pub const fn automatic_check_due(now: u64, last_attempt: Option<u64>) -> bool {
    match last_attempt {
        None => true,
        Some(previous) if previous > now => false,
        Some(previous) => now - previous >= AUTOMATIC_CHECK_INTERVAL_SECONDS,
    }
}

pub const fn next_automatic_check_at(launch_at: u64, now: u64, last_attempt: Option<u64>) -> u64 {
    let launch_floor = launch_at.saturating_add(FIRST_AUTOMATIC_CHECK_DELAY_SECONDS);
    let Some(previous) = last_attempt else {
        return launch_floor;
    };
    let clamped_previous = if previous > now { now } else { previous };
    let due = clamped_previous.saturating_add(AUTOMATIC_CHECK_INTERVAL_SECONDS);
    if due > launch_floor {
        due
    } else {
        launch_floor
    }
}

fn selected_artifact(release: &VerifiedRelease, model: &ModelAvailability) -> SelectedArtifact {
    let descriptor = select_artifact(release, model);
    let kind = if descriptor == &release.application_update {
        ArtifactKind::Update
    } else {
        ArtifactKind::Full
    };
    SelectedArtifact {
        kind,
        descriptor: descriptor.clone(),
    }
}

fn download_offer(
    state: &UpdaterState,
) -> Option<(Box<VerifiedRelease>, SelectedArtifact, OfferDisposition)> {
    match state {
        UpdaterState::Available { release, artifact } => Some((
            release.clone(),
            artifact.clone(),
            OfferDisposition::Available,
        )),
        UpdaterState::DivergedLocal { release, artifact } => Some((
            release.clone(),
            artifact.clone(),
            OfferDisposition::DivergedLocal,
        )),
        UpdaterState::RepairRequired { release, artifact } => Some((
            release.clone(),
            artifact.clone(),
            OfferDisposition::RepairRequired,
        )),
        _ => None,
    }
}

fn project_offer_after_recheck(
    release: Box<VerifiedRelease>,
    artifact: SelectedArtifact,
    disposition: OfferDisposition,
) -> UpdaterState {
    match disposition {
        OfferDisposition::Available => UpdaterState::Available { release, artifact },
        OfferDisposition::DivergedLocal => UpdaterState::DivergedLocal { release, artifact },
        OfferDisposition::RepairRequired if artifact.kind == ArtifactKind::Update => {
            UpdaterState::Current
        }
        OfferDisposition::RepairRequired => UpdaterState::RepairRequired { release, artifact },
    }
}

fn failed_download_context(
    failure: UpdateFailure,
    release: Box<VerifiedRelease>,
    artifact: SelectedArtifact,
    disposition: OfferDisposition,
) -> UpdaterState {
    UpdaterState::Failed {
        failure,
        retry: RetryAction::Download,
        context: Some(RetryContext::Download {
            release,
            artifact,
            disposition,
        }),
    }
}

// Artifact cache and worker boundaries. All production implementations reject
// AppKit's main thread; Task 5 owns dispatching these synchronous boundaries.

fn artifact_cache_paths(
    cache_root: &Path,
    release: &VerifiedRelease,
    kind: ArtifactKind,
) -> (PathBuf, PathBuf) {
    let file_name = format!(
        "PTT2me-{}-{}-{}-macos-arm64.dmg",
        release.version,
        release.build,
        kind.cache_label()
    );
    let final_path = cache_root.join(&file_name);
    let partial_path = cache_root.join(format!("{file_name}.part"));
    (final_path, partial_path)
}

fn lookup_verified_download(
    cache_root: &Path,
    release: &VerifiedRelease,
    kind: ArtifactKind,
    artifact: &ArtifactDescriptor,
) -> Result<Option<PathBuf>, UpdateFailure> {
    if artifact.size == 0 || artifact.size > MAX_UPDATE_ARTIFACT_BYTES {
        return Err(UpdateFailure::InvalidArtifactSize);
    }
    fs::create_dir_all(cache_root).map_err(|_| UpdateFailure::Storage)?;
    let (final_path, partial_path) = artifact_cache_paths(cache_root, release, kind);
    remove_stale_partial(&partial_path)?;

    match fs::symlink_metadata(&final_path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let cached = File::open(&final_path).map_err(|_| UpdateFailure::Storage)?;
            if verify_artifact(cached, artifact).is_ok() {
                Ok(Some(final_path))
            } else {
                fs::remove_file(&final_path).map_err(|_| UpdateFailure::Storage)?;
                Ok(None)
            }
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(&final_path).map_err(|_| UpdateFailure::Storage)?;
            Ok(None)
        }
        Ok(_) => Err(UpdateFailure::Storage),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(UpdateFailure::Storage),
    }
}

pub fn cache_verified_download(
    cache_root: &Path,
    release: &VerifiedRelease,
    kind: ArtifactKind,
    artifact: &ArtifactDescriptor,
    reader: impl Read,
    content_length: Option<u64>,
) -> Result<PathBuf, UpdateFailure> {
    cache_verified_download_with_promoter(
        cache_root,
        release,
        kind,
        artifact,
        reader,
        content_length,
        |partial, final_path| fs::rename(partial, final_path),
    )
}

fn cache_verified_download_with_promoter(
    cache_root: &Path,
    release: &VerifiedRelease,
    kind: ArtifactKind,
    artifact: &ArtifactDescriptor,
    mut reader: impl Read,
    content_length: Option<u64>,
    promote: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<PathBuf, UpdateFailure> {
    if let Some(cached) = lookup_verified_download(cache_root, release, kind, artifact)? {
        return Ok(cached);
    }
    let (final_path, partial_path) = artifact_cache_paths(cache_root, release, kind);

    if let Some(length) = content_length {
        if length != artifact.size {
            return Err(UpdateFailure::ContentLengthMismatch);
        }
    }

    let mut promoted = false;
    let write_result = (|| {
        let file = File::create(&partial_path).map_err(|_| UpdateFailure::Storage)?;
        let mut writer = BufWriter::new(file);
        let limit = artifact
            .size
            .checked_add(1)
            .ok_or(UpdateFailure::InvalidArtifactSize)?;
        let copied = io::copy(&mut reader.by_ref().take(limit), &mut writer)
            .map_err(|_| UpdateFailure::Network)?;
        writer.flush().map_err(|_| UpdateFailure::Storage)?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|_| UpdateFailure::Storage)?;
        drop(writer);

        if copied != artifact.size {
            return Err(UpdateFailure::BodySizeMismatch);
        }

        let downloaded = File::open(&partial_path).map_err(|_| UpdateFailure::Storage)?;
        verify_artifact(downloaded, artifact).map_err(map_artifact_error)?;
        promote(&partial_path, &final_path).map_err(|_| UpdateFailure::Storage)?;
        promoted = true;

        let promoted_file = File::open(&final_path).map_err(|_| UpdateFailure::Storage)?;
        verify_artifact(promoted_file, artifact).map_err(map_artifact_error)?;
        File::open(cache_root)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| UpdateFailure::Storage)?;
        Ok(final_path.clone())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&partial_path);
        if promoted {
            let _ = fs::remove_file(&final_path);
        }
    }
    write_result
}

fn remove_stale_partial(partial_path: &Path) -> Result<(), UpdateFailure> {
    match fs::symlink_metadata(partial_path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            fs::remove_file(partial_path).map_err(|_| UpdateFailure::Storage)
        }
        Ok(_) => Err(UpdateFailure::Storage),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(UpdateFailure::Storage),
    }
}

fn map_artifact_error(error: ManifestError) -> UpdateFailure {
    match error {
        ManifestError::ArtifactSizeMismatch | ManifestError::ArtifactDigestMismatch => {
            UpdateFailure::DigestMismatch
        }
        _ => UpdateFailure::Storage,
    }
}

pub struct DownloadResponse {
    pub content_length: Option<u64>,
    pub reader: Box<dyn Read + Send>,
}

pub trait UpdateFetch: Send + Sync {
    fn fetch_manifest(&self, url: &str) -> Result<Vec<u8>, UpdateFailure>;
    fn fetch_artifact(
        &self,
        artifact: &ArtifactDescriptor,
    ) -> Result<DownloadResponse, UpdateFailure>;
}

pub trait UpdateStorage: Send + Sync {
    fn lookup_verified(
        &self,
        release: &VerifiedRelease,
        kind: ArtifactKind,
        artifact: &ArtifactDescriptor,
    ) -> Result<Option<PathBuf>, UpdateFailure>;

    fn store_verified(
        &self,
        release: &VerifiedRelease,
        kind: ArtifactKind,
        artifact: &ArtifactDescriptor,
        reader: &mut (dyn Read + Send),
        content_length: Option<u64>,
    ) -> Result<PathBuf, UpdateFailure>;

    fn discard(&self, path: &Path) -> Result<(), UpdateFailure>;
}

pub trait UpdateClock: Send + Sync {
    fn now(&self) -> u64;
}

pub trait DmgOpener: Send + Sync {
    fn verify_and_open_dmg(
        &self,
        release: &VerifiedRelease,
        artifact: &SelectedArtifact,
        expected_path: &Path,
    ) -> Result<(), UpdateFailure>;
}

pub trait QuarantineChecker: Send + Sync {
    fn has_quarantine(&self, path: &Path) -> Result<bool, UpdateFailure>;
}

pub struct ArtifactWorker<F, S, Q> {
    fetch: F,
    storage: S,
    quarantine: Q,
}

impl<F, S, Q> ArtifactWorker<F, S, Q>
where
    F: UpdateFetch,
    S: UpdateStorage,
    Q: QuarantineChecker,
{
    pub const fn new(fetch: F, storage: S, quarantine: Q) -> Self {
        Self {
            fetch,
            storage,
            quarantine,
        }
    }

    pub fn download(
        &self,
        release: &VerifiedRelease,
        kind: ArtifactKind,
        artifact: &ArtifactDescriptor,
    ) -> Result<VerifiedDownload, UpdateFailure> {
        if let Some(path) = self.storage.lookup_verified(release, kind, artifact)? {
            return self.finish_download(path);
        }
        let mut response = self.fetch.fetch_artifact(artifact)?;
        let path = self.storage.store_verified(
            release,
            kind,
            artifact,
            response.reader.as_mut(),
            response.content_length,
        )?;
        self.finish_download(path)
    }

    fn finish_download(&self, path: PathBuf) -> Result<VerifiedDownload, UpdateFailure> {
        match self.quarantine.has_quarantine(&path) {
            Ok(true) => Ok(VerifiedDownload::from_verified_path(path)),
            Ok(false) => {
                self.storage.discard(&path)?;
                Err(UpdateFailure::QuarantineMissing)
            }
            Err(failure) => {
                let _ = self.storage.discard(&path);
                Err(failure)
            }
        }
    }
}

pub struct FileUpdateStorage {
    cache_root: PathBuf,
}

impl FileUpdateStorage {
    pub const fn new(cache_root: PathBuf) -> Self {
        Self { cache_root }
    }
}

impl UpdateStorage for FileUpdateStorage {
    fn lookup_verified(
        &self,
        release: &VerifiedRelease,
        kind: ArtifactKind,
        artifact: &ArtifactDescriptor,
    ) -> Result<Option<PathBuf>, UpdateFailure> {
        ensure_background_thread()?;
        lookup_verified_download(&self.cache_root, release, kind, artifact)
    }

    fn store_verified(
        &self,
        release: &VerifiedRelease,
        kind: ArtifactKind,
        artifact: &ArtifactDescriptor,
        reader: &mut (dyn Read + Send),
        content_length: Option<u64>,
    ) -> Result<PathBuf, UpdateFailure> {
        ensure_background_thread()?;
        cache_verified_download(
            &self.cache_root,
            release,
            kind,
            artifact,
            reader,
            content_length,
        )
    }

    fn discard(&self, path: &Path) -> Result<(), UpdateFailure> {
        ensure_background_thread()?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(UpdateFailure::Storage),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl UpdateClock for SystemClock {
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    }
}

#[derive(Debug, Clone)]
pub struct HttpsUpdateFetch {
    agent: ureq::Agent,
}

impl Default for HttpsUpdateFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpsUpdateFetch {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .https_only(true)
            .timeout_connect(Duration::from_secs(UPDATE_CONNECT_TIMEOUT_SECONDS))
            .timeout_read(Duration::from_secs(UPDATE_READ_TIMEOUT_SECONDS))
            .redirects(UPDATE_MAX_REDIRECTS)
            .build();
        Self { agent }
    }

    fn response(&self, url: &str) -> Result<(ureq::Response, Instant), UpdateFailure> {
        ensure_background_thread()?;
        let started_at = Instant::now();
        let response = self.agent.get(url).call().map_err(map_ureq_error)?;
        Ok((response, started_at))
    }
}

impl UpdateFetch for HttpsUpdateFetch {
    fn fetch_manifest(&self, url: &str) -> Result<Vec<u8>, UpdateFailure> {
        let (response, started_at) = self.response(url)?;
        let content_length = validate_http_response_metadata(
            response.status(),
            response.get_url(),
            response.header("Content-Length"),
            None,
        )?;
        if content_length.is_some_and(|length| length > MAX_UPDATE_MANIFEST_BYTES) {
            return Err(UpdateFailure::ManifestTooLarge);
        }
        read_bounded_manifest(OverallDeadlineReader::new(
            response.into_reader(),
            started_at,
        ))
    }

    fn fetch_artifact(
        &self,
        artifact: &ArtifactDescriptor,
    ) -> Result<DownloadResponse, UpdateFailure> {
        if artifact.size == 0 || artifact.size > MAX_UPDATE_ARTIFACT_BYTES {
            return Err(UpdateFailure::InvalidArtifactSize);
        }
        let (response, started_at) = self.response(&artifact.url)?;
        let content_length = validate_http_response_metadata(
            response.status(),
            response.get_url(),
            response.header("Content-Length"),
            Some(artifact.size),
        )?;
        Ok(DownloadResponse {
            content_length,
            reader: Box::new(OverallDeadlineReader::new(
                response.into_reader(),
                started_at,
            )),
        })
    }
}

struct OverallDeadlineReader<R> {
    inner: R,
    started_at: Instant,
}

impl<R> OverallDeadlineReader<R> {
    const fn new(inner: R, started_at: Instant) -> Self {
        Self { inner, started_at }
    }
}

impl<R: Read> Read for OverallDeadlineReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.started_at.elapsed() >= Duration::from_secs(UPDATE_OVERALL_TIMEOUT_SECONDS) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "update request exceeded overall timeout",
            ));
        }
        let read = self.inner.read(buffer)?;
        if self.started_at.elapsed() >= Duration::from_secs(UPDATE_OVERALL_TIMEOUT_SECONDS) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "update request exceeded overall timeout",
            ));
        }
        Ok(read)
    }
}

pub fn read_bounded_manifest(mut reader: impl Read) -> Result<Vec<u8>, UpdateFailure> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_UPDATE_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UpdateFailure::Network)?;
    if bytes.len() as u64 > MAX_UPDATE_MANIFEST_BYTES {
        return Err(UpdateFailure::ManifestTooLarge);
    }
    Ok(bytes)
}

pub fn validate_http_response_metadata(
    status: u16,
    final_url: &str,
    content_length: Option<&str>,
    expected_length: Option<u64>,
) -> Result<Option<u64>, UpdateFailure> {
    if !(200..300).contains(&status) {
        return Err(UpdateFailure::HttpStatus);
    }
    if !final_url.starts_with("https://") {
        return Err(UpdateFailure::InsecureTransport);
    }
    let parsed_length = match content_length {
        Some(value) => Some(
            value
                .parse::<u64>()
                .map_err(|_| UpdateFailure::ContentLengthMismatch)?,
        ),
        None => None,
    };
    if let (Some(actual), Some(expected)) = (parsed_length, expected_length) {
        if actual != expected {
            return Err(UpdateFailure::ContentLengthMismatch);
        }
    }
    Ok(parsed_length)
}

fn map_ureq_error(error: ureq::Error) -> UpdateFailure {
    match error {
        ureq::Error::Status(_, _) => UpdateFailure::HttpStatus,
        ureq::Error::Transport(_) => UpdateFailure::Network,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MacOsQuarantineChecker;

impl QuarantineChecker for MacOsQuarantineChecker {
    fn has_quarantine(&self, path: &Path) -> Result<bool, UpdateFailure> {
        ensure_background_thread()?;
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| UpdateFailure::Storage)?;
        let attribute = b"com.apple.quarantine\0";
        let result = unsafe {
            libc::getxattr(
                path.as_ptr(),
                attribute.as_ptr().cast(),
                std::ptr::null_mut(),
                0,
                0,
                0,
            )
        };
        if result >= 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOATTR) {
            Ok(false)
        } else {
            Err(UpdateFailure::Storage)
        }
    }
}

fn read_fd_stat(fd: RawFd) -> Result<libc::stat, UpdateFailure> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(UpdateFailure::Storage);
    }
    Ok(unsafe { stat.assume_init() })
}

fn read_relative_stat(directory_fd: RawFd, name: &CString) -> Result<libc::stat, UpdateFailure> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(UpdateFailure::DigestMismatch);
    }
    Ok(unsafe { stat.assume_init() })
}

fn validate_controlled_directory(stat: &libc::stat) -> Result<(), UpdateFailure> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_mode & (libc::S_IWGRP | libc::S_IWOTH) != 0
    {
        return Err(UpdateFailure::Storage);
    }
    Ok(())
}

fn validate_controlled_artifact(
    stat: &libc::stat,
    expected_size: u64,
) -> Result<(), UpdateFailure> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_size < 0
        || stat.st_size as u64 != expected_size
    {
        return Err(UpdateFailure::DigestMismatch);
    }
    if stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_mode & (libc::S_IWGRP | libc::S_IWOTH) != 0
    {
        return Err(UpdateFailure::Storage);
    }
    Ok(())
}

fn same_file_identity(left: &libc::stat, right: &libc::stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino && left.st_size == right.st_size
}

fn same_verified_snapshot(left: &libc::stat, right: &libc::stat) -> bool {
    same_file_identity(left, right)
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn open_controlled_directory(path: &Path) -> Result<File, UpdateFailure> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| UpdateFailure::Storage)?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(UpdateFailure::Storage);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_controlled_artifact(directory_fd: RawFd, name: &CString) -> Result<File, UpdateFailure> {
    let fd = unsafe {
        libc::openat(
            directory_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOENT)
        {
            Err(UpdateFailure::DigestMismatch)
        } else {
            Err(UpdateFailure::Storage)
        };
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn descriptor_has_quarantine(fd: RawFd) -> Result<bool, UpdateFailure> {
    let attribute = b"com.apple.quarantine\0";
    let result =
        unsafe { libc::fgetxattr(fd, attribute.as_ptr().cast(), std::ptr::null_mut(), 0, 0, 0) };
    if result >= 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOATTR) {
        Ok(false)
    } else {
        Err(UpdateFailure::Storage)
    }
}

fn verify_and_open_dmg_with<AfterHash, OpenPath>(
    cache_root: &Path,
    release: &VerifiedRelease,
    artifact: &SelectedArtifact,
    expected_path: &Path,
    after_hash: AfterHash,
    open_path: OpenPath,
) -> Result<(), UpdateFailure>
where
    AfterHash: FnOnce() -> io::Result<()>,
    OpenPath: FnOnce(&Path) -> bool,
{
    if !cache_root.is_absolute() {
        return Err(UpdateFailure::Storage);
    }
    let signed_descriptor = match artifact.kind {
        ArtifactKind::Full => &release.fresh_install,
        ArtifactKind::Update => &release.application_update,
    };
    if &artifact.descriptor != signed_descriptor {
        return Err(UpdateFailure::DigestMismatch);
    }
    let (canonical_path, _) = artifact_cache_paths(cache_root, release, artifact.kind);
    if expected_path != canonical_path {
        return Err(UpdateFailure::DigestMismatch);
    }
    let name = expected_path
        .file_name()
        .ok_or(UpdateFailure::DigestMismatch)?;
    let name = CString::new(name.as_bytes()).map_err(|_| UpdateFailure::Storage)?;

    let directory = open_controlled_directory(cache_root)?;
    let directory_stat = read_fd_stat(directory.as_raw_fd())?;
    validate_controlled_directory(&directory_stat)?;

    let file = open_controlled_artifact(directory.as_raw_fd(), &name)?;
    let verified_stat = read_fd_stat(file.as_raw_fd())?;
    validate_controlled_artifact(&verified_stat, artifact.descriptor.size)?;
    let limit = artifact
        .descriptor
        .size
        .checked_add(1)
        .ok_or(UpdateFailure::InvalidArtifactSize)?;
    verify_artifact((&file).take(limit), &artifact.descriptor).map_err(map_artifact_error)?;

    after_hash().map_err(|_| UpdateFailure::Storage)?;
    if !descriptor_has_quarantine(file.as_raw_fd())? {
        return Err(UpdateFailure::QuarantineMissing);
    }

    let final_file_stat = read_fd_stat(file.as_raw_fd())?;
    validate_controlled_artifact(&final_file_stat, artifact.descriptor.size)?;
    if !same_verified_snapshot(&verified_stat, &final_file_stat) {
        return Err(UpdateFailure::DigestMismatch);
    }
    let path_directory_stat = read_relative_stat(
        libc::AT_FDCWD,
        &CString::new(cache_root.as_os_str().as_bytes()).map_err(|_| UpdateFailure::Storage)?,
    )?;
    validate_controlled_directory(&path_directory_stat)?;
    if !same_file_identity(&directory_stat, &path_directory_stat) {
        return Err(UpdateFailure::DigestMismatch);
    }
    let path_file_stat = read_relative_stat(directory.as_raw_fd(), &name)?;
    validate_controlled_artifact(&path_file_stat, artifact.descriptor.size)?;
    if !same_file_identity(&final_file_stat, &path_file_stat) {
        return Err(UpdateFailure::DigestMismatch);
    }

    // NSWorkspace accepts only a path URL. Holding both descriptors through
    // openURL and checking identity immediately beforehand narrows replacement
    // races, but cannot cryptographically bind LaunchServices to this inode
    // against hostile code already running as the same UID.
    let opened = open_path(expected_path);
    drop(file);
    drop(directory);
    if opened {
        Ok(())
    } else {
        Err(UpdateFailure::OpenDmg)
    }
}

fn file_url_for_path(path: &Path) -> Result<Retained<NSURL>, UpdateFailure> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| UpdateFailure::OpenDmg)?;
    let path = NonNull::new(path.as_ptr().cast_mut()).ok_or(UpdateFailure::OpenDmg)?;
    Ok(unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(path, false, None)
    })
}

#[derive(Debug, Clone)]
pub struct MacOsWorkspaceOpener {
    cache_root: PathBuf,
}

impl MacOsWorkspaceOpener {
    pub const fn new(cache_root: PathBuf) -> Self {
        Self { cache_root }
    }
}

impl DmgOpener for MacOsWorkspaceOpener {
    fn verify_and_open_dmg(
        &self,
        release: &VerifiedRelease,
        artifact: &SelectedArtifact,
        expected_path: &Path,
    ) -> Result<(), UpdateFailure> {
        ensure_background_thread()?;
        verify_and_open_dmg_with(
            &self.cache_root,
            release,
            artifact,
            expected_path,
            || Ok(()),
            |path| {
                let Ok(url) = file_url_for_path(path) else {
                    return false;
                };
                unsafe { NSWorkspace::sharedWorkspace().openURL(&url) }
            },
        )
    }
}

fn ensure_background_thread() -> Result<(), UpdateFailure> {
    if NSThread::isMainThread_class() {
        Err(UpdateFailure::WrongThread)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "updater/reducer_tests.rs"]
mod tests;
