use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::model_store::{
    embedded_model_manifest, model_directory, verify_model_directory, MODEL_ID,
    PRODUCTION_MODEL_MANIFEST_SHA256,
};
use crate::preferences::{
    RawUpdateScheduleStore, SystemUpdateScheduleStore, UpdateScheduleRepository,
};
use crate::state::AppStatus;
use crate::update_manifest::{
    verify_envelope, InstalledBuild, MacOsVersion, ModelAvailability, RequiredModel,
    VerifiedRelease,
};
use crate::updater::{
    read_bounded_manifest, ArtifactWorker, CheckReason, DmgOpener, FileUpdateStorage,
    HttpsUpdateFetch, MacOsQuarantineChecker, MacOsWorkspaceOpener, OperationId, SelectedArtifact,
    SystemClock, UpdateClock, UpdateFailure, UpdateFetch, Updater, UpdaterCommand, UpdaterEvent,
    UpdaterState, VerifiedDownload,
};

pub(crate) fn required_model_availability(
    required: &RequiredModel,
    application_support_root: &Path,
) -> ModelAvailability {
    if required.id != MODEL_ID || required.manifest_sha256 != PRODUCTION_MODEL_MANIFEST_SHA256 {
        return ModelAvailability::Invalid;
    }
    let Ok(manifest) = embedded_model_manifest() else {
        return ModelAvailability::Invalid;
    };
    let directory = model_directory(application_support_root);
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => ModelAvailability::Missing,
        Err(_) => ModelAvailability::Invalid,
        Ok(_) => verify_model_directory(&directory, &manifest)
            .map(|model| model.availability())
            .unwrap_or(ModelAvailability::Invalid),
    }
}

pub(crate) const fn updater_open_allowed(status: &AppStatus, paste_pending: bool) -> bool {
    matches!(status, AppStatus::Ready) && !paste_pending
}

#[derive(Debug, Default)]
pub(crate) struct OrderlyQuitGate {
    pending: bool,
}

impl OrderlyQuitGate {
    pub(crate) fn request(&mut self) {
        self.pending = true;
    }

    pub(crate) fn take_if_ready(&mut self, status: &AppStatus, paste_pending: bool) -> bool {
        if self.pending && updater_open_allowed(status, paste_pending) {
            self.pending = false;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ManifestCache {
    path: PathBuf,
}

impl ManifestCache {
    pub(crate) const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn load(&self) -> Result<Vec<u8>, UpdateFailure> {
        let file = File::open(&self.path).map_err(|_| UpdateFailure::Storage)?;
        read_bounded_manifest(file)
    }

    pub(crate) fn store_verified(&self, bytes: &[u8]) -> Result<(), UpdateFailure> {
        read_bounded_manifest(bytes).map_err(|failure| match failure {
            UpdateFailure::ManifestTooLarge => failure,
            _ => UpdateFailure::Storage,
        })?;
        let parent = self.path.parent().ok_or(UpdateFailure::Storage)?;
        fs::create_dir_all(parent).map_err(|_| UpdateFailure::Storage)?;
        let partial_path = append_suffix(&self.path, ".part");
        remove_stale_partial(&partial_path)?;

        let result = (|| {
            let mut partial = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&partial_path)
                .map_err(|_| UpdateFailure::Storage)?;
            partial
                .write_all(bytes)
                .and_then(|()| partial.flush())
                .and_then(|()| partial.sync_all())
                .map_err(|_| UpdateFailure::Storage)?;
            drop(partial);
            fs::rename(&partial_path, &self.path).map_err(|_| UpdateFailure::Storage)?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| UpdateFailure::Storage)
        })();

        if result.is_err() {
            let _ = fs::remove_file(&partial_path);
        }
        result
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn remove_stale_partial(path: &Path) -> Result<(), UpdateFailure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|_| UpdateFailure::Storage)
        }
        Ok(_) => Err(UpdateFailure::Storage),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(UpdateFailure::Storage),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UpdaterLaunchConfig {
    pub(crate) installed: InstalledBuild,
    pub(crate) public_key: [u8; 32],
    pub(crate) running_macos: MacOsVersion,
    pub(crate) manifest_url: String,
    pub(crate) manifest_cache_path: PathBuf,
    pub(crate) artifact_cache_root: PathBuf,
    pub(crate) application_support_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdaterWorkerRequest {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdaterWorkerResult {
    CachedManifestReceived {
        operation_id: OperationId,
        bytes: Vec<u8>,
        model: ModelAvailability,
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
    ModelRechecked {
        operation_id: OperationId,
        model: ModelAvailability,
    },
    DownloadVerified {
        operation_id: OperationId,
        download: VerifiedDownload,
    },
    DownloadFailed {
        operation_id: OperationId,
        failure: UpdateFailure,
    },
    OpenCompleted {
        operation_id: OperationId,
        result: Result<(), UpdateFailure>,
    },
}

impl UpdaterWorkerResult {
    fn into_event(self) -> UpdaterEvent {
        match self {
            Self::CachedManifestReceived {
                operation_id,
                bytes,
                model,
            } => UpdaterEvent::CachedManifestReceived {
                operation_id,
                bytes,
                model,
            },
            Self::ManifestReceived {
                operation_id,
                bytes,
                model,
            } => UpdaterEvent::ManifestReceived {
                operation_id,
                bytes,
                model,
            },
            Self::ManifestFailed {
                operation_id,
                failure,
            } => UpdaterEvent::ManifestFailed {
                operation_id,
                failure,
            },
            Self::ModelRechecked {
                operation_id,
                model,
            } => UpdaterEvent::ModelRechecked {
                operation_id,
                model,
            },
            Self::DownloadVerified {
                operation_id,
                download,
            } => UpdaterEvent::DownloadVerified {
                operation_id,
                download,
            },
            Self::DownloadFailed {
                operation_id,
                failure,
            } => UpdaterEvent::DownloadFailed {
                operation_id,
                failure,
            },
            Self::OpenCompleted {
                operation_id,
                result,
            } => UpdaterEvent::OpenCompleted {
                operation_id,
                result,
            },
        }
    }
}

pub(crate) trait UpdaterWorkerBoundary: Send + 'static {
    fn execute(&mut self, request: UpdaterWorkerRequest) -> Option<UpdaterWorkerResult>;
}

pub(crate) struct UpdaterWorkerTask {
    requests: Sender<UpdaterWorkerRequest>,
    results: Receiver<UpdaterWorkerResult>,
    worker: Option<JoinHandle<()>>,
}

impl UpdaterWorkerTask {
    pub(crate) fn spawn(mut boundary: impl UpdaterWorkerBoundary) -> Self {
        let (request_sender, request_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                if let Some(result) = boundary.execute(request) {
                    if result_sender.send(result).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            requests: request_sender,
            results: result_receiver,
            worker: Some(worker),
        }
    }

    pub(crate) fn send(&self, request: UpdaterWorkerRequest) -> Result<(), UpdaterWorkerRequest> {
        self.requests.send(request).map_err(|error| error.0)
    }

    fn drain_results(&self) -> Vec<UpdaterWorkerResult> {
        self.results.try_iter().collect()
    }
}

impl Drop for UpdaterWorkerTask {
    fn drop(&mut self) {
        // Dropping a live JoinHandle detaches it. Updater I/O must never hold
        // the AppKit main thread during shutdown.
        self.worker.take();
    }
}

struct ProductionUpdaterWorker {
    manifest_url: String,
    public_key: [u8; 32],
    application_support_root: PathBuf,
    manifest_cache: ManifestCache,
    fetch: HttpsUpdateFetch,
    artifacts: ArtifactWorker<HttpsUpdateFetch, FileUpdateStorage, MacOsQuarantineChecker>,
    opener: MacOsWorkspaceOpener,
}

impl ProductionUpdaterWorker {
    fn new(config: &UpdaterLaunchConfig) -> Self {
        let fetch = HttpsUpdateFetch::new();
        Self {
            manifest_url: config.manifest_url.clone(),
            public_key: config.public_key,
            application_support_root: config.application_support_root.clone(),
            manifest_cache: ManifestCache::new(config.manifest_cache_path.clone()),
            artifacts: ArtifactWorker::new(
                fetch.clone(),
                FileUpdateStorage::new(config.artifact_cache_root.clone()),
                MacOsQuarantineChecker,
            ),
            opener: MacOsWorkspaceOpener::new(config.artifact_cache_root.clone()),
            fetch,
        }
    }

    fn model_for_envelope(&self, bytes: &[u8]) -> ModelAvailability {
        verify_envelope(bytes, &self.public_key).map_or(ModelAvailability::Invalid, |release| {
            required_model_availability(&release.required_model, &self.application_support_root)
        })
    }
}

impl UpdaterWorkerBoundary for ProductionUpdaterWorker {
    fn execute(&mut self, request: UpdaterWorkerRequest) -> Option<UpdaterWorkerResult> {
        match request {
            UpdaterWorkerRequest::LoadCachedManifest { operation_id } => {
                let bytes = self.manifest_cache.load().unwrap_or_default();
                let model = self.model_for_envelope(&bytes);
                Some(UpdaterWorkerResult::CachedManifestReceived {
                    operation_id,
                    bytes,
                    model,
                })
            }
            UpdaterWorkerRequest::StoreVerifiedManifest { bytes } => {
                if self.manifest_cache.store_verified(&bytes).is_err() {
                    tracing::warn!(error_category = "update_manifest_cache_store");
                }
                None
            }
            UpdaterWorkerRequest::FetchManifest {
                operation_id,
                reason: _,
            } => match self.fetch.fetch_manifest(&self.manifest_url) {
                Ok(bytes) => {
                    let model = self.model_for_envelope(&bytes);
                    Some(UpdaterWorkerResult::ManifestReceived {
                        operation_id,
                        bytes,
                        model,
                    })
                }
                Err(failure) => Some(UpdaterWorkerResult::ManifestFailed {
                    operation_id,
                    failure,
                }),
            },
            UpdaterWorkerRequest::RecheckModel {
                operation_id,
                required_model,
            } => Some(UpdaterWorkerResult::ModelRechecked {
                operation_id,
                model: required_model_availability(&required_model, &self.application_support_root),
            }),
            UpdaterWorkerRequest::DownloadAndVerify {
                operation_id,
                release,
                artifact,
            } => match self
                .artifacts
                .download(&release, artifact.kind, &artifact.descriptor)
            {
                Ok(download) => Some(UpdaterWorkerResult::DownloadVerified {
                    operation_id,
                    download,
                }),
                Err(failure) => Some(UpdaterWorkerResult::DownloadFailed {
                    operation_id,
                    failure,
                }),
            },
            UpdaterWorkerRequest::VerifyAndOpenDmg {
                operation_id,
                release,
                artifact,
                expected_path,
            } => Some(UpdaterWorkerResult::OpenCompleted {
                operation_id,
                result: self
                    .opener
                    .verify_and_open_dmg(&release, &artifact, &expected_path),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdaterRuntimeEffect {
    ScheduleAt(u64),
    RequestOrderlyQuit,
}

pub(crate) struct UpdaterLane<R: RawUpdateScheduleStore, C: UpdateClock> {
    updater: Updater,
    schedule: UpdateScheduleRepository<R>,
    clock: C,
    launch_at: u64,
    worker: UpdaterWorkerTask,
}

pub(crate) type SystemUpdaterLane = UpdaterLane<SystemUpdateScheduleStore, SystemClock>;

impl UpdaterLane<SystemUpdateScheduleStore, SystemClock> {
    pub(crate) fn production(config: UpdaterLaunchConfig) -> Self {
        let worker = ProductionUpdaterWorker::new(&config);
        Self::with_boundaries(
            config,
            SystemUpdateScheduleStore::standard(),
            SystemClock,
            worker,
        )
    }
}

impl<R: RawUpdateScheduleStore, C: UpdateClock> UpdaterLane<R, C> {
    pub(crate) fn with_boundaries(
        config: UpdaterLaunchConfig,
        schedule: R,
        clock: C,
        worker: impl UpdaterWorkerBoundary,
    ) -> Self {
        let launch_at = clock.now();
        Self {
            updater: Updater::new(config.installed, config.public_key, config.running_macos),
            schedule: UpdateScheduleRepository::new(schedule),
            clock,
            launch_at,
            worker: UpdaterWorkerTask::spawn(worker),
        }
    }

    pub(crate) fn state(&self) -> &UpdaterState {
        self.updater.state()
    }

    pub(crate) fn launch(&mut self) -> Vec<UpdaterRuntimeEffect> {
        let now = self.clock.now();
        self.handle_event(UpdaterEvent::Launched {
            launch_at: self.launch_at,
            now,
            last_attempt: self.schedule.load_last_attempt(),
        })
    }

    pub(crate) fn automatic_check_due(&mut self) -> Vec<UpdaterRuntimeEffect> {
        let now = self.clock.now();
        self.handle_event(UpdaterEvent::AutomaticCheckDue {
            launch_at: self.launch_at,
            now,
            last_attempt: self.schedule.load_last_attempt(),
        })
    }

    pub(crate) fn manual_check(&mut self) -> Vec<UpdaterRuntimeEffect> {
        self.handle_event(UpdaterEvent::ManualCheckRequested {
            now: self.clock.now(),
        })
    }

    pub(crate) fn request_download(&mut self) -> Vec<UpdaterRuntimeEffect> {
        self.handle_event(UpdaterEvent::DownloadRequested)
    }

    pub(crate) fn retry(&mut self) -> Vec<UpdaterRuntimeEffect> {
        self.handle_event(UpdaterEvent::RetryRequested)
    }

    pub(crate) fn request_open(&mut self) -> Vec<UpdaterRuntimeEffect> {
        self.handle_event(UpdaterEvent::OpenRequested)
    }

    pub(crate) fn drain_worker_results(&mut self) -> (Vec<UpdaterRuntimeEffect>, bool) {
        let results = self.worker.drain_results();
        let handled = !results.is_empty();
        let mut effects = Vec::new();
        for result in results {
            effects.extend(self.handle_event(result.into_event()));
        }
        (effects, handled)
    }

    fn handle_event(&mut self, event: UpdaterEvent) -> Vec<UpdaterRuntimeEffect> {
        let commands = self.updater.handle(event);
        self.execute_commands(commands)
    }

    fn execute_commands(&mut self, commands: Vec<UpdaterCommand>) -> Vec<UpdaterRuntimeEffect> {
        let mut effects = Vec::new();
        let mut blocked_fetch = None;
        for command in commands {
            match command {
                UpdaterCommand::ScheduleAutomaticCheck(deadline) => {
                    effects.push(UpdaterRuntimeEffect::ScheduleAt(deadline));
                }
                UpdaterCommand::PersistLastAttempt {
                    operation_id,
                    attempted_at,
                } => {
                    if self.schedule.persist_last_attempt(attempted_at).is_err() {
                        blocked_fetch = Some(operation_id);
                        effects.extend(self.handle_event(
                            UpdaterEvent::LastAttemptPersistenceFailed { operation_id },
                        ));
                    }
                }
                UpdaterCommand::LoadCachedManifest { operation_id } => {
                    effects.extend(
                        self.queue_worker(UpdaterWorkerRequest::LoadCachedManifest {
                            operation_id,
                        }),
                    );
                }
                UpdaterCommand::StoreVerifiedManifest { bytes } => {
                    effects.extend(
                        self.queue_worker(UpdaterWorkerRequest::StoreVerifiedManifest { bytes }),
                    );
                }
                UpdaterCommand::FetchManifest {
                    operation_id,
                    reason,
                } if blocked_fetch != Some(operation_id) => {
                    effects.extend(self.queue_worker(UpdaterWorkerRequest::FetchManifest {
                        operation_id,
                        reason,
                    }));
                }
                UpdaterCommand::FetchManifest { .. } => {}
                UpdaterCommand::RecheckModel {
                    operation_id,
                    required_model,
                } => {
                    effects.extend(self.queue_worker(UpdaterWorkerRequest::RecheckModel {
                        operation_id,
                        required_model,
                    }));
                }
                UpdaterCommand::DownloadAndVerify {
                    operation_id,
                    release,
                    artifact,
                } => {
                    effects.extend(self.queue_worker(UpdaterWorkerRequest::DownloadAndVerify {
                        operation_id,
                        release,
                        artifact,
                    }));
                }
                UpdaterCommand::VerifyAndOpenDmg {
                    operation_id,
                    release,
                    artifact,
                    expected_path,
                } => {
                    effects.extend(self.queue_worker(UpdaterWorkerRequest::VerifyAndOpenDmg {
                        operation_id,
                        release,
                        artifact,
                        expected_path,
                    }));
                }
                UpdaterCommand::RequestOrderlyQuit => {
                    effects.push(UpdaterRuntimeEffect::RequestOrderlyQuit);
                }
            }
        }
        effects
    }

    fn queue_worker(&mut self, request: UpdaterWorkerRequest) -> Vec<UpdaterRuntimeEffect> {
        match self.worker.send(request) {
            Ok(()) => Vec::new(),
            Err(request) => worker_disconnect_event(request)
                .map_or_else(Vec::new, |event| self.handle_event(event)),
        }
    }
}

fn worker_disconnect_event(request: UpdaterWorkerRequest) -> Option<UpdaterEvent> {
    match request {
        UpdaterWorkerRequest::LoadCachedManifest { operation_id } => {
            Some(UpdaterEvent::CachedManifestReceived {
                operation_id,
                bytes: Vec::new(),
                model: ModelAvailability::Invalid,
            })
        }
        UpdaterWorkerRequest::StoreVerifiedManifest { .. } => None,
        UpdaterWorkerRequest::FetchManifest { operation_id, .. } => {
            Some(UpdaterEvent::ManifestFailed {
                operation_id,
                failure: UpdateFailure::Network,
            })
        }
        UpdaterWorkerRequest::RecheckModel { operation_id, .. } => {
            Some(UpdaterEvent::ModelRecheckFailed {
                operation_id,
                failure: UpdateFailure::Network,
            })
        }
        UpdaterWorkerRequest::DownloadAndVerify { operation_id, .. } => {
            Some(UpdaterEvent::DownloadFailed {
                operation_id,
                failure: UpdateFailure::Network,
            })
        }
        UpdaterWorkerRequest::VerifyAndOpenDmg { operation_id, .. } => {
            Some(UpdaterEvent::OpenCompleted {
                operation_id,
                result: Err(UpdateFailure::Network),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::preferences::{RawUpdateScheduleStore, UpdateScheduleError};
    use crate::state::AppStatus;
    use crate::update_manifest::{
        select_artifact, ArtifactDescriptor, InstalledBuild, MacOsVersion, RequiredModel,
        VerifiedRelease,
    };
    use crate::updater::{OperationId, UpdateClock, UpdaterState};

    use super::*;

    #[test]
    fn model_manifest_hash_mismatch_selects_the_full_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let required = RequiredModel {
            id: crate::model_store::MODEL_ID.to_owned(),
            manifest_sha256: "0".repeat(64),
        };
        let availability = required_model_availability(&required, temp.path());
        let full = ArtifactDescriptor {
            url: "https://example.test/full.dmg".to_owned(),
            sha256: "1".repeat(64),
            size: 10,
        };
        let update = ArtifactDescriptor {
            url: "https://example.test/update.dmg".to_owned(),
            sha256: "2".repeat(64),
            size: 5,
        };
        let release = VerifiedRelease {
            version: semver::Version::parse("1.0.6").unwrap(),
            build: 202608011200,
            source_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            minimum_macos: crate::update_manifest::MacOsVersion::parse("13.0").unwrap(),
            required_model: required,
            fresh_install: full.clone(),
            application_update: update,
            published_at: "2026-08-01T12:00:00Z".to_owned(),
        };

        assert_eq!(
            availability,
            crate::update_manifest::ModelAvailability::Invalid
        );
        assert_eq!(select_artifact(&release, &availability), &full);
    }

    #[test]
    fn open_and_orderly_quit_wait_for_idle_voice_and_paste() {
        assert!(updater_open_allowed(&AppStatus::Ready, false));
        assert!(!updater_open_allowed(&AppStatus::Recording, false));
        assert!(!updater_open_allowed(&AppStatus::Ready, true));

        let mut gate = OrderlyQuitGate::default();
        gate.request();
        assert!(!gate.take_if_ready(&AppStatus::Recording, false));
        assert!(!gate.take_if_ready(&AppStatus::Ready, true));
        assert!(gate.take_if_ready(&AppStatus::Ready, false));
        assert!(!gate.take_if_ready(&AppStatus::Ready, false));
    }

    #[test]
    fn manifest_cache_promotes_verified_bytes_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("updates/stable-envelope.json");
        let cache = ManifestCache::new(path.clone());

        cache.store_verified(b"signed envelope").unwrap();

        assert_eq!(cache.load().unwrap(), b"signed envelope");
        assert!(!PathBuf::from(format!("{}.part", path.display())).exists());
    }

    #[derive(Clone)]
    struct FixedClock(Arc<AtomicU64>);

    impl FixedClock {
        fn new(now: u64) -> Self {
            Self(Arc::new(AtomicU64::new(now)))
        }

        fn set(&self, now: u64) {
            self.0.store(now, Ordering::SeqCst);
        }
    }

    impl UpdateClock for FixedClock {
        fn now(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct RecordingSchedule {
        last_attempt: Option<u64>,
        fail_writes: bool,
        timeline: Arc<Mutex<Vec<String>>>,
    }

    impl RawUpdateScheduleStore for RecordingSchedule {
        fn last_network_check_attempt(&self) -> Option<u64> {
            self.last_attempt
        }

        fn set_last_network_check_attempt(
            &mut self,
            value: u64,
        ) -> Result<(), UpdateScheduleError> {
            self.timeline
                .lock()
                .unwrap()
                .push(format!("persist:{value}"));
            if self.fail_writes {
                Err(UpdateScheduleError::WriteFailed)
            } else {
                self.last_attempt = Some(value);
                Ok(())
            }
        }
    }

    struct RecordingWorker {
        timeline: Arc<Mutex<Vec<String>>>,
    }

    impl UpdaterWorkerBoundary for RecordingWorker {
        fn execute(&mut self, request: UpdaterWorkerRequest) -> Option<UpdaterWorkerResult> {
            match request {
                UpdaterWorkerRequest::LoadCachedManifest { operation_id } => {
                    self.timeline
                        .lock()
                        .unwrap()
                        .push(format!("worker:load:{}", operation_id.0));
                    Some(UpdaterWorkerResult::CachedManifestReceived {
                        operation_id,
                        bytes: Vec::new(),
                        model: crate::update_manifest::ModelAvailability::Missing,
                    })
                }
                UpdaterWorkerRequest::FetchManifest { operation_id, .. } => {
                    self.timeline
                        .lock()
                        .unwrap()
                        .push(format!("worker:fetch:{}", operation_id.0));
                    Some(UpdaterWorkerResult::ManifestFailed {
                        operation_id,
                        failure: crate::updater::UpdateFailure::Network,
                    })
                }
                unexpected => panic!("unexpected worker request: {unexpected:?}"),
            }
        }
    }

    fn launch_config(temp: &tempfile::TempDir) -> UpdaterLaunchConfig {
        UpdaterLaunchConfig {
            installed: InstalledBuild::parse(
                "1.0.5",
                202607311200,
                "0123456789abcdef0123456789abcdef01234567",
            )
            .unwrap(),
            public_key: [7; 32],
            running_macos: MacOsVersion::parse("13.0").unwrap(),
            manifest_url: "https://example.test/stable.json".to_owned(),
            manifest_cache_path: temp.path().join("stable-envelope.json"),
            artifact_cache_root: temp.path().join("artifacts"),
            application_support_root: temp.path().join("Application Support/PTT2me"),
        }
    }

    fn wait_for_timeline<R: RawUpdateScheduleStore, C: UpdateClock>(
        lane: &mut UpdaterLane<R, C>,
        timeline: &Arc<Mutex<Vec<String>>>,
        length: usize,
    ) {
        for _ in 0..100 {
            let _ = lane.drain_worker_results();
            if timeline.lock().unwrap().len() >= length {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("worker timeline did not reach {length} entries");
    }

    #[test]
    fn lane_schedules_sixty_seconds_then_twenty_four_hours_and_manual_bypasses_history() {
        let temp = tempfile::tempdir().unwrap();
        let timeline = Arc::new(Mutex::new(Vec::new()));
        let clock = FixedClock::new(1_000);
        let schedule = RecordingSchedule {
            last_attempt: None,
            fail_writes: false,
            timeline: Arc::clone(&timeline),
        };
        let worker = RecordingWorker {
            timeline: Arc::clone(&timeline),
        };
        let mut lane =
            UpdaterLane::with_boundaries(launch_config(&temp), schedule, clock.clone(), worker);

        assert_eq!(lane.launch(), vec![UpdaterRuntimeEffect::ScheduleAt(1_060)]);
        wait_for_timeline(&mut lane, &timeline, 1);

        clock.set(1_060);
        assert_eq!(
            lane.automatic_check_due(),
            vec![UpdaterRuntimeEffect::ScheduleAt(87_460)]
        );
        wait_for_timeline(&mut lane, &timeline, 3);
        assert_eq!(lane.state(), &UpdaterState::Idle);

        clock.set(1_070);
        assert_eq!(
            lane.manual_check(),
            vec![UpdaterRuntimeEffect::ScheduleAt(87_470)]
        );
        wait_for_timeline(&mut lane, &timeline, 5);
        assert!(matches!(
            lane.state(),
            UpdaterState::Failed {
                failure: crate::updater::UpdateFailure::Network,
                retry: crate::updater::RetryAction::ManualCheck,
                context: None,
            }
        ));
        assert_eq!(
            timeline.lock().unwrap().as_slice(),
            [
                "worker:load:1",
                "persist:1060",
                "worker:fetch:2",
                "persist:1070",
                "worker:fetch:3",
            ]
        );
    }

    #[test]
    fn lane_never_queues_fetch_after_persistence_failure() {
        let temp = tempfile::tempdir().unwrap();
        let timeline = Arc::new(Mutex::new(Vec::new()));
        let clock = FixedClock::new(1_000);
        let schedule = RecordingSchedule {
            last_attempt: None,
            fail_writes: true,
            timeline: Arc::clone(&timeline),
        };
        let worker = RecordingWorker {
            timeline: Arc::clone(&timeline),
        };
        let mut lane =
            UpdaterLane::with_boundaries(launch_config(&temp), schedule, clock.clone(), worker);
        let _ = lane.launch();
        wait_for_timeline(&mut lane, &timeline, 1);

        clock.set(1_060);
        assert_eq!(
            lane.automatic_check_due(),
            vec![UpdaterRuntimeEffect::ScheduleAt(87_460)]
        );
        std::thread::sleep(Duration::from_millis(10));
        let _ = lane.drain_worker_results();

        assert_eq!(lane.state(), &UpdaterState::Idle);
        assert_eq!(
            timeline.lock().unwrap().as_slice(),
            ["worker:load:1", "persist:1060"]
        );
    }

    struct BlockingWorker {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl UpdaterWorkerBoundary for BlockingWorker {
        fn execute(&mut self, request: UpdaterWorkerRequest) -> Option<UpdaterWorkerResult> {
            assert!(matches!(
                request,
                UpdaterWorkerRequest::LoadCachedManifest {
                    operation_id: OperationId(41)
                }
            ));
            self.started.send(()).unwrap();
            self.release.recv().unwrap();
            None
        }
    }

    #[test]
    fn dropping_an_active_updater_worker_detaches_without_joining() {
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let worker = UpdaterWorkerTask::spawn(BlockingWorker {
            started: started_sender,
            release: release_receiver,
        });
        worker
            .send(UpdaterWorkerRequest::LoadCachedManifest {
                operation_id: OperationId(41),
            })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let started_at = Instant::now();
        drop(worker);
        assert!(started_at.elapsed() < Duration::from_millis(100));
        release_sender.send(()).unwrap();
    }
}
