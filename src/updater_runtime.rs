use crate::event_wake::{EventNotifier, TerminalSender};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use objc2_foundation::NSProcessInfo;

use crate::model_store::{
    application_support_root_from_home, embedded_model_manifest, model_directory,
    verify_model_directory, MODEL_ID, PRODUCTION_MODEL_MANIFEST_SHA256,
};
use crate::permission_migration::{
    load_main_bundle_identity, BuildIdentityError, BuildIdentityLoad,
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

pub(crate) const PRODUCTION_MANIFEST_URL: &str =
    "https://torin2023.github.io/PTT2me/channels/stable.json";
const PRODUCTION_PUBLIC_KEY: &[u8] = include_bytes!("../updates/public-key.txt");

#[derive(Debug)]
pub(crate) enum ProductionUpdaterConfigError {
    Identity(BuildIdentityError),
    InvalidInstalledBuild,
    InvalidPublicKey,
    HomeUnavailable,
    InvalidHome,
    InvalidOperatingSystem,
}

impl std::fmt::Display for ProductionUpdaterConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Identity(error) => write!(formatter, "invalid application identity: {error:?}"),
            Self::InvalidInstalledBuild => formatter.write_str("invalid installed build identity"),
            Self::InvalidPublicKey => formatter.write_str("invalid embedded updater public key"),
            Self::HomeUnavailable => formatter.write_str("HOME is unavailable"),
            Self::InvalidHome => formatter.write_str("HOME must be an absolute path"),
            Self::InvalidOperatingSystem => formatter.write_str("invalid macOS version"),
        }
    }
}

pub(crate) fn load_production_updater_config(
) -> Result<Option<UpdaterLaunchConfig>, ProductionUpdaterConfigError> {
    let identity = load_main_bundle_identity().map_err(ProductionUpdaterConfigError::Identity)?;
    build_production_updater_config(
        identity,
        || {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(ProductionUpdaterConfigError::HomeUnavailable)
        },
        running_macos_version,
    )
}

fn build_production_updater_config(
    identity: BuildIdentityLoad,
    home: impl FnOnce() -> Result<PathBuf, ProductionUpdaterConfigError>,
    running_macos: impl FnOnce() -> Result<MacOsVersion, ProductionUpdaterConfigError>,
) -> Result<Option<UpdaterLaunchConfig>, ProductionUpdaterConfigError> {
    let BuildIdentityLoad::Release(identity) = identity else {
        return Ok(None);
    };

    let build = identity
        .build()
        .parse::<u64>()
        .map_err(|_| ProductionUpdaterConfigError::InvalidInstalledBuild)?;
    let installed = InstalledBuild::parse(identity.version(), build, identity.source_commit())
        .map_err(|_| ProductionUpdaterConfigError::InvalidInstalledBuild)?;
    let public_key = crate::release_manifest::parse_public_key(PRODUCTION_PUBLIC_KEY)
        .map_err(|_| ProductionUpdaterConfigError::InvalidPublicKey)?;
    let home = home()?;
    if !home.is_absolute() {
        return Err(ProductionUpdaterConfigError::InvalidHome);
    }
    let running_macos = running_macos()?;
    let application_support_root = application_support_root_from_home(&home)
        .map_err(|_| ProductionUpdaterConfigError::InvalidHome)?;
    let cache_root = home.join("Library/Caches/com.ptt2me.app");

    Ok(Some(UpdaterLaunchConfig {
        installed,
        public_key,
        running_macos,
        manifest_url: PRODUCTION_MANIFEST_URL.to_owned(),
        manifest_cache_path: cache_root.join("channels/stable.json"),
        artifact_cache_root: cache_root.join("artifacts"),
        application_support_root,
    }))
}

fn running_macos_version() -> Result<MacOsVersion, ProductionUpdaterConfigError> {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();
    let major = u64::try_from(version.majorVersion)
        .map_err(|_| ProductionUpdaterConfigError::InvalidOperatingSystem)?;
    let minor = u64::try_from(version.minorVersion)
        .map_err(|_| ProductionUpdaterConfigError::InvalidOperatingSystem)?;
    MacOsVersion::parse(&format!("{major}.{minor}"))
        .map_err(|_| ProductionUpdaterConfigError::InvalidOperatingSystem)
}

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
    fn operation_id(&self) -> OperationId {
        match self {
            Self::CachedManifestReceived { operation_id, .. }
            | Self::ManifestReceived { operation_id, .. }
            | Self::ManifestFailed { operation_id, .. }
            | Self::ModelRechecked { operation_id, .. }
            | Self::DownloadVerified { operation_id, .. }
            | Self::DownloadFailed { operation_id, .. }
            | Self::OpenCompleted { operation_id, .. } => *operation_id,
        }
    }

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
    pending: HashMap<OperationId, UpdaterEvent>,
}

impl UpdaterWorkerTask {
    #[cfg(test)]
    pub(crate) fn spawn(boundary: impl UpdaterWorkerBoundary) -> Self {
        Self::spawn_notified(boundary, EventNotifier::default())
    }
    fn spawn_notified(mut boundary: impl UpdaterWorkerBoundary, notifier: EventNotifier) -> Self {
        let (request_sender, request_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result_sender = TerminalSender::new(result_sender, notifier.clone());
            while let Ok(request) = request_receiver.recv() {
                if let Some(result) = boundary.execute(request) {
                    if result_sender.send(result).is_err() {
                        break;
                    }
                    notifier.notify();
                }
            }
        });
        Self {
            requests: request_sender,
            results: result_receiver,
            worker: Some(worker),
            pending: HashMap::new(),
        }
    }

    pub(crate) fn send(
        &mut self,
        request: UpdaterWorkerRequest,
    ) -> Result<(), UpdaterWorkerRequest> {
        let failure = worker_disconnect_event(&request);
        self.requests.send(request).map_err(|error| error.0)?;
        if let Some((operation_id, event)) = failure {
            self.pending.insert(operation_id, event);
        }
        Ok(())
    }

    #[cfg(test)]
    fn drain_results(&mut self) -> (Vec<UpdaterEvent>, bool) {
        self.drain_results_limit(usize::MAX)
    }
    fn drain_results_limit(&mut self, limit: usize) -> (Vec<UpdaterEvent>, bool) {
        let mut events = Vec::new();
        while events.len() < limit {
            match self.results.try_recv() {
                Ok(result) => {
                    self.pending.remove(&result.operation_id());
                    events.push(result.into_event());
                }
                Err(TryRecvError::Empty) => return (events, false),
                Err(TryRecvError::Disconnected) => {
                    // Buffered completions win over failures. Only requests without
                    // a result belong to the dead worker; new operation IDs remain safe.
                    if let Some(id) = self.pending.keys().next().copied() {
                        events.push(self.pending.remove(&id).unwrap());
                        if limit == 1 {
                            return (events, false);
                        }
                        events.extend(self.pending.drain().map(|(_, event)| event));
                    }
                    return (events, true);
                }
            }
        }
        (events, false)
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
    worker: Option<UpdaterWorkerTask>,
    worker_factory: Box<dyn FnMut() -> UpdaterWorkerTask>,
}

pub(crate) type SystemUpdaterLane = UpdaterLane<SystemUpdateScheduleStore, SystemClock>;

impl UpdaterLane<SystemUpdateScheduleStore, SystemClock> {
    pub(crate) fn production(config: UpdaterLaunchConfig, notifier: EventNotifier) -> Self {
        let worker_config = config.clone();
        Self::with_notifier(
            config,
            SystemUpdateScheduleStore::standard(),
            SystemClock,
            move || ProductionUpdaterWorker::new(&worker_config),
            notifier,
        )
    }
}

impl<R: RawUpdateScheduleStore, C: UpdateClock> UpdaterLane<R, C> {
    #[cfg(test)]
    pub(crate) fn with_boundaries<B: UpdaterWorkerBoundary>(
        config: UpdaterLaunchConfig,
        schedule: R,
        clock: C,
        worker_factory: impl FnMut() -> B + 'static,
    ) -> Self {
        Self::with_notifier(
            config,
            schedule,
            clock,
            worker_factory,
            EventNotifier::default(),
        )
    }
    fn with_notifier<B: UpdaterWorkerBoundary>(
        config: UpdaterLaunchConfig,
        schedule: R,
        clock: C,
        mut worker_factory: impl FnMut() -> B + 'static,
        notifier: EventNotifier,
    ) -> Self {
        let launch_at = clock.now();
        Self {
            updater: Updater::new(config.installed, config.public_key, config.running_macos),
            schedule: UpdateScheduleRepository::new(schedule),
            clock,
            launch_at,
            worker: Some(UpdaterWorkerTask::spawn_notified(
                worker_factory(),
                notifier.clone(),
            )),
            worker_factory: Box::new(move || {
                UpdaterWorkerTask::spawn_notified(worker_factory(), notifier.clone())
            }),
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

    #[cfg(test)]
    pub(crate) fn drain_worker_results(&mut self) -> (Vec<UpdaterRuntimeEffect>, bool) {
        self.drain_worker_results_limit(usize::MAX)
    }
    pub(crate) fn poll_worker_result(&mut self) -> (Vec<UpdaterRuntimeEffect>, bool) {
        self.drain_worker_results_limit(1)
    }
    fn drain_worker_results_limit(&mut self, limit: usize) -> (Vec<UpdaterRuntimeEffect>, bool) {
        let Some(worker) = &mut self.worker else {
            return (Vec::new(), false);
        };
        let (events, disconnected) = worker.drain_results_limit(limit);
        let handled = disconnected || !events.is_empty();
        if disconnected {
            tracing::warn!(error_category = "updater_worker_stopped");
            // Retire before dispatch: a buffered result can queue the next stage.
            // Recreate lazily for that stage or the next explicit/scheduled request.
            self.worker = None;
        }
        let mut effects = Vec::new();
        for event in events {
            effects.extend(self.handle_event(event));
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
        let worker = self.worker.get_or_insert_with(&mut self.worker_factory);
        match worker.send(request) {
            Ok(()) => Vec::new(),
            Err(request) => worker_disconnect_event(&request)
                .map_or_else(Vec::new, |(_, event)| self.handle_event(event)),
        }
    }
}

fn worker_disconnect_event(request: &UpdaterWorkerRequest) -> Option<(OperationId, UpdaterEvent)> {
    let (operation_id, event) = match request {
        UpdaterWorkerRequest::LoadCachedManifest { operation_id } => (
            *operation_id,
            UpdaterEvent::CachedManifestReceived {
                operation_id: *operation_id,
                bytes: Vec::new(),
                model: ModelAvailability::Invalid,
            },
        ),
        UpdaterWorkerRequest::StoreVerifiedManifest { .. } => return None,
        UpdaterWorkerRequest::FetchManifest { operation_id, .. } => (
            *operation_id,
            UpdaterEvent::ManifestFailed {
                operation_id: *operation_id,
                failure: UpdateFailure::WorkerStopped,
            },
        ),
        UpdaterWorkerRequest::RecheckModel { operation_id, .. } => (
            *operation_id,
            UpdaterEvent::ModelRecheckFailed {
                operation_id: *operation_id,
                failure: UpdateFailure::WorkerStopped,
            },
        ),
        UpdaterWorkerRequest::DownloadAndVerify { operation_id, .. } => (
            *operation_id,
            UpdaterEvent::DownloadFailed {
                operation_id: *operation_id,
                failure: UpdateFailure::WorkerStopped,
            },
        ),
        UpdaterWorkerRequest::VerifyAndOpenDmg { operation_id, .. } => (
            *operation_id,
            UpdaterEvent::OpenCompleted {
                operation_id: *operation_id,
                result: Err(UpdateFailure::WorkerStopped),
            },
        ),
    };
    Some((operation_id, event))
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
    fn development_bypass_does_not_read_home_or_operating_system() {
        let config = build_production_updater_config(
            crate::permission_migration::BuildIdentityLoad::DevelopmentBypass,
            || panic!("HOME must not be read for a development binary"),
            || panic!("macOS version must not be read for a development binary"),
        )
        .unwrap();

        assert!(config.is_none());
    }

    #[test]
    fn release_identity_maps_to_exact_production_updater_config() {
        let identity = crate::permission_migration::BuildIdentity::parse(
            "1.0.5",
            "202608011234",
            "0123456789abcdef0123456789abcdef01234567",
        )
        .unwrap();
        let home = PathBuf::from("/Users/release-user");

        let config = build_production_updater_config(
            crate::permission_migration::BuildIdentityLoad::Release(identity),
            || Ok(home.clone()),
            || Ok(MacOsVersion::parse("14.6").unwrap()),
        )
        .unwrap()
        .unwrap();

        assert_eq!(config.installed.version.to_string(), "1.0.5");
        assert_eq!(config.installed.build, 202608011234);
        assert_eq!(
            config.installed.source_commit,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(config.running_macos.to_string(), "14.6");
        assert_eq!(config.manifest_url, PRODUCTION_MANIFEST_URL);
        assert_eq!(
            config.manifest_cache_path,
            home.join("Library/Caches/com.ptt2me.app/channels/stable.json")
        );
        assert_eq!(
            config.artifact_cache_root,
            home.join("Library/Caches/com.ptt2me.app/artifacts")
        );
        assert_eq!(
            config.application_support_root,
            home.join("Library/Application Support/PTT2me")
        );
        assert_eq!(
            config.public_key,
            crate::release_manifest::parse_public_key(include_bytes!("../updates/public-key.txt"))
                .unwrap()
        );
    }

    #[test]
    fn release_config_rejects_relative_home() {
        let identity = crate::permission_migration::BuildIdentity::parse(
            "1.0.5",
            "202608011234",
            "0123456789abcdef0123456789abcdef01234567",
        )
        .unwrap();

        assert!(build_production_updater_config(
            crate::permission_migration::BuildIdentityLoad::Release(identity),
            || Ok(PathBuf::from("relative-home")),
            || Ok(MacOsVersion::parse("14.6").unwrap()),
        )
        .is_err());
    }

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

    #[derive(Clone)]
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
            let (_, handled) = lane.drain_worker_results();
            if handled && timeline.lock().unwrap().len() >= length {
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
        let mut lane = UpdaterLane::with_boundaries(
            launch_config(&temp),
            schedule,
            clock.clone(),
            move || worker.clone(),
        );

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
        let mut lane = UpdaterLane::with_boundaries(
            launch_config(&temp),
            schedule,
            clock.clone(),
            move || worker.clone(),
        );
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

    fn crash_lane() -> UpdaterLane<RecordingSchedule, FixedClock> {
        let temp = tempfile::tempdir().unwrap();
        UpdaterLane::with_boundaries(
            launch_config(&temp),
            RecordingSchedule {
                last_attempt: None,
                fail_writes: false,
                timeline: Arc::new(Mutex::new(Vec::new())),
            },
            FixedClock::new(1_000),
            || PanickingWorker,
        )
    }

    fn join_crashed_worker(lane: &mut UpdaterLane<RecordingSchedule, FixedClock>) {
        assert!(lane
            .worker
            .as_mut()
            .unwrap()
            .worker
            .take()
            .unwrap()
            .join()
            .is_err());
    }

    #[test]
    fn empty_live_channel_does_not_report_a_crash() {
        let mut lane = crash_lane();
        assert_eq!(lane.drain_worker_results(), (Vec::new(), false));
        assert_eq!(lane.state(), &UpdaterState::Idle);
        assert!(lane.worker.is_some());
    }

    #[test]
    fn cache_crash_is_silent_and_retired_once() {
        let mut lane = crash_lane();
        lane.launch();
        join_crashed_worker(&mut lane);
        assert_eq!(lane.drain_worker_results(), (Vec::new(), true));
        assert_eq!(lane.state(), &UpdaterState::Idle);
        assert_eq!(lane.drain_worker_results(), (Vec::new(), false));
        assert!(lane.worker.is_none());
    }

    #[test]
    fn send_failure_before_polling_does_not_get_overwritten_by_stale_cache_failure() {
        let mut lane = crash_lane();
        lane.launch();
        join_crashed_worker(&mut lane);
        // The UI can request a check before the timer observes the dead channel.
        lane.manual_check();
        lane.drain_worker_results();
        assert!(matches!(
            lane.state(),
            UpdaterState::Failed {
                failure: UpdateFailure::WorkerStopped,
                retry: crate::updater::RetryAction::ManualCheck,
                context: None,
            }
        ));
        assert!(lane.worker.is_none());
    }

    struct RecoveringCheckWorker {
        crash: bool,
    }

    impl UpdaterWorkerBoundary for RecoveringCheckWorker {
        fn execute(&mut self, request: UpdaterWorkerRequest) -> Option<UpdaterWorkerResult> {
            assert!(!self.crash, "injected crash on first worker generation");
            let UpdaterWorkerRequest::FetchManifest { operation_id, .. } = request else {
                panic!("unexpected recovery request");
            };
            Some(UpdaterWorkerResult::ManifestFailed {
                operation_id,
                failure: UpdateFailure::HttpStatus,
            })
        }
    }

    #[test]
    fn failed_check_can_complete_again_on_a_recreated_worker() {
        let temp = tempfile::tempdir().unwrap();
        let mut generation = 0;
        let mut lane = UpdaterLane::with_boundaries(
            launch_config(&temp),
            RecordingSchedule {
                last_attempt: None,
                fail_writes: false,
                timeline: Arc::new(Mutex::new(Vec::new())),
            },
            FixedClock::new(1_000),
            move || {
                generation += 1;
                RecoveringCheckWorker {
                    crash: generation == 1,
                }
            },
        );
        lane.manual_check();
        join_crashed_worker(&mut lane);
        lane.drain_worker_results();
        assert!(matches!(
            lane.state(),
            UpdaterState::Failed {
                failure: UpdateFailure::WorkerStopped,
                ..
            }
        ));

        lane.manual_check();
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(lane.state(), UpdaterState::Checking { .. }) {
            assert!(
                Instant::now() < deadline,
                "recreated worker never completed the check"
            );
            lane.drain_worker_results();
            thread::yield_now();
        }
        assert!(matches!(
            lane.state(),
            UpdaterState::Failed {
                failure: UpdateFailure::HttpStatus,
                ..
            }
        ));
        assert!(lane.worker.is_some());
    }

    #[test]
    fn automatic_check_crash_preserves_silent_failure_policy() {
        let mut lane = crash_lane();
        lane.clock.set(1_060);
        lane.automatic_check_due();
        join_crashed_worker(&mut lane);
        lane.drain_worker_results();
        assert_eq!(lane.state(), &UpdaterState::Idle);
        assert!(!lane.manual_check().is_empty());
        join_crashed_worker(&mut lane);
        lane.drain_worker_results();
        assert!(matches!(
            lane.state(),
            UpdaterState::Failed {
                failure: UpdateFailure::WorkerStopped,
                ..
            }
        ));
    }

    struct CompleteThenCrash;

    impl UpdaterWorkerBoundary for CompleteThenCrash {
        fn execute(&mut self, request: UpdaterWorkerRequest) -> Option<UpdaterWorkerResult> {
            match request {
                UpdaterWorkerRequest::FetchManifest { operation_id, .. } => {
                    Some(UpdaterWorkerResult::ManifestFailed {
                        operation_id,
                        failure: UpdateFailure::HttpStatus,
                    })
                }
                _ => panic!("injected crash after buffered completion"),
            }
        }
    }

    #[test]
    fn result_terminal_failure_and_recreated_worker_keep_main_wake() {
        use core_foundation::runloop::CFRunLoop;
        use std::cell::RefCell;
        use std::rc::Rc;
        let source = crate::event_wake::EventSource::new(CFRunLoop::get_current());
        let temp = tempfile::tempdir().unwrap();
        let starts = Arc::new(AtomicU64::new(0));
        let factory_starts = starts.clone();
        let lane = Rc::new(RefCell::new(UpdaterLane::with_notifier(
            launch_config(&temp),
            RecordingSchedule {
                last_attempt: None,
                fail_writes: false,
                timeline: Arc::new(Mutex::new(Vec::new())),
            },
            FixedClock::new(1_000),
            move || {
                factory_starts.fetch_add(1, Ordering::SeqCst);
                CompleteThenCrash
            },
            source.notifier(),
        )));
        let consumer = lane.clone();
        let notifier = source.notifier();
        source.set_handler(move || {
            // Production uses one event per lane visit and re-signals at quota.
            let (_, handled) = consumer.borrow_mut().poll_worker_result();
            if handled {
                notifier.notify();
            }
        });
        source.attach();
        CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::ZERO,
            true,
        );
        for expected_starts in 1..=2 {
            lane.borrow_mut().manual_check();
            lane.borrow_mut()
                .queue_worker(UpdaterWorkerRequest::StoreVerifiedManifest { bytes: vec![] });
            crate::event_wake::tests::pump_until(|| lane.borrow().worker.is_none());
            assert!(matches!(
                lane.borrow().state(),
                UpdaterState::Failed {
                    failure: UpdateFailure::HttpStatus,
                    ..
                }
            ));
            assert_eq!(starts.load(Ordering::SeqCst), expected_starts);
        }
    }

    #[test]
    fn buffered_completion_wins_and_next_check_uses_a_fresh_worker() {
        let temp = tempfile::tempdir().unwrap();
        let starts = Arc::new(AtomicU64::new(0));
        let factory_starts = starts.clone();
        let mut lane = UpdaterLane::with_boundaries(
            launch_config(&temp),
            RecordingSchedule {
                last_attempt: None,
                fail_writes: false,
                timeline: Arc::new(Mutex::new(Vec::new())),
            },
            FixedClock::new(1_000),
            move || {
                factory_starts.fetch_add(1, Ordering::SeqCst);
                CompleteThenCrash
            },
        );
        for expected_starts in 1..=2 {
            lane.manual_check();
            lane.queue_worker(UpdaterWorkerRequest::StoreVerifiedManifest { bytes: vec![] });
            join_crashed_worker(&mut lane);
            lane.drain_worker_results();
            assert!(matches!(
                lane.state(),
                UpdaterState::Failed {
                    failure: UpdateFailure::HttpStatus,
                    ..
                }
            ));
            assert_eq!(starts.load(Ordering::SeqCst), expected_starts);
            assert!(lane.worker.is_none());
        }
    }

    #[test]
    fn disconnected_worker_reports_every_unanswered_stage_with_its_operation_id() {
        let release = Box::new(VerifiedRelease {
            version: semver::Version::new(1, 0, 6),
            build: 1,
            source_commit: "0".repeat(40),
            minimum_macos: MacOsVersion::parse("13.0").unwrap(),
            required_model: RequiredModel {
                id: "test".into(),
                manifest_sha256: "0".repeat(64),
            },
            fresh_install: ArtifactDescriptor {
                url: "https://example.test/full.dmg".into(),
                sha256: "1".repeat(64),
                size: 10,
            },
            application_update: ArtifactDescriptor {
                url: "https://example.test/update.dmg".into(),
                sha256: "2".repeat(64),
                size: 5,
            },
            published_at: "2026-09-05T00:00:00Z".into(),
        });
        let artifact = SelectedArtifact {
            kind: crate::updater::ArtifactKind::Full,
            descriptor: release.fresh_install.clone(),
        };
        let requests = [
            UpdaterWorkerRequest::RecheckModel {
                operation_id: OperationId(11),
                required_model: release.required_model.clone(),
            },
            UpdaterWorkerRequest::DownloadAndVerify {
                operation_id: OperationId(12),
                release: release.clone(),
                artifact: artifact.clone(),
            },
            UpdaterWorkerRequest::VerifyAndOpenDmg {
                operation_id: OperationId(13),
                release,
                artifact,
                expected_path: PathBuf::from("/unused/test.dmg"),
            },
        ];
        let expected = [
            UpdaterEvent::ModelRecheckFailed {
                operation_id: OperationId(11),
                failure: UpdateFailure::WorkerStopped,
            },
            UpdaterEvent::DownloadFailed {
                operation_id: OperationId(12),
                failure: UpdateFailure::WorkerStopped,
            },
            UpdaterEvent::OpenCompleted {
                operation_id: OperationId(13),
                result: Err(UpdateFailure::WorkerStopped),
            },
        ];
        for (request, expected) in requests.into_iter().zip(expected) {
            let mut worker = UpdaterWorkerTask::spawn(PanickingWorker);
            worker.send(request).unwrap();
            assert!(worker.worker.take().unwrap().join().is_err());
            assert_eq!(worker.drain_results(), (vec![expected], true));
        }
    }

    struct BlockingWorker {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    struct PanickingWorker;

    impl UpdaterWorkerBoundary for PanickingWorker {
        fn execute(&mut self, _: UpdaterWorkerRequest) -> Option<UpdaterWorkerResult> {
            panic!("injected updater worker crash");
        }
    }

    #[test]
    fn worker_crash_finishes_the_active_manual_check() {
        let temp = tempfile::tempdir().unwrap();
        let schedule = RecordingSchedule {
            last_attempt: None,
            fail_writes: false,
            timeline: Arc::new(Mutex::new(Vec::new())),
        };
        let mut lane = UpdaterLane::with_boundaries(
            launch_config(&temp),
            schedule,
            FixedClock::new(1_000),
            || PanickingWorker,
        );
        lane.manual_check();
        assert!(lane
            .worker
            .as_mut()
            .unwrap()
            .worker
            .take()
            .unwrap()
            .join()
            .is_err());

        let (effects, handled) = lane.drain_worker_results();

        assert!(handled, "a disconnected worker must trigger a menu refresh");
        assert!(effects.is_empty());
        assert!(matches!(lane.state(), UpdaterState::Failed { .. }));
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
        let mut worker = UpdaterWorkerTask::spawn(BlockingWorker {
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
