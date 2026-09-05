use std::ffi::c_void;
use std::marker::PhantomPinned;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use core_foundation::date::CFDate;
use core_foundation::runloop::{
    kCFRunLoopCommonModes, CFRunLoop, CFRunLoopTimer, CFRunLoopTimerContext, CFRunLoopTimerRef,
};
use objc2_app_kit::NSApplication;
use objc2_foundation::MainThreadMarker;

use crate::asr::{spawn_asr_worker, AsrCommand, AsrEvent};
use crate::audio::{AudioError, AudioRecorder};
use crate::constants::{MAX_CAPTURE_MS, RELEASE_GRACE_MS};
use crate::hotkey::{AssignmentEpoch, HotkeyControl, HotkeyListener, HotkeySignal};
use crate::inserter::{InsertError, PASTEBOARD_RESTORE_DELAY_MS, PASTEBOARD_SETTLE_DELAY_MS};
use crate::menu::{MenuAction, UpdaterMenuAction};
use crate::menu::{MenuBar, MenuCommand, MenuReadiness};
use crate::model::{resources_dir_from_executable, ModelPaths};
use crate::model_store::{
    application_support_root, bundled_model_directory, embedded_model_manifest, resolve_model,
    verify_model_directory, ModelStoreError, VerifiedModel,
};
use crate::output_preferences::{
    OutputPreferenceController, OutputPreferenceError, OutputPreferenceRepository,
    RawOutputPreferenceStore, SystemOutputPreferenceStore,
};
use crate::permission_migration::{
    persist_setup_completion_if_granted, run_system_permission_migration, BuildIdentity,
    PermissionMigrationRunError, PermissionMigrationSuccess, SystemPermissionMigrationStore,
};
use crate::permissions::{
    self, MicrophoneAuthorization, MicrophonePermissionBoundary, MicrophonePermissionFlow,
    SystemPermissionProbe,
};
use crate::preferences::{
    PreferenceError, PreferenceRepository, Preferences, RawPreferenceStore, SystemPreferenceStore,
    TriggerKey,
};
use crate::state::{
    AppController, AppEvent, AppStatus, Effect, ModelPreparationFailure, PermissionSnapshot,
};
use crate::text_inserter::{self, PendingTextInsertion};
use crate::updater::{RetryAction, SystemClock, UpdateClock, UpdaterState};
use crate::updater_runtime::{
    load_production_updater_config, updater_open_allowed, OrderlyQuitGate, SystemUpdaterLane,
    UpdaterLaunchConfig, UpdaterRuntimeEffect,
};

const EVENT_DRAIN_MS: u64 = 50;
const PERMISSION_POLL_MS: u64 = 1_000;
const SMOKE_MODEL_TIMEOUT: Duration = Duration::from_secs(180);
const SMOKE_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SMOKE_TIMEOUT_EXIT_CODE: i32 = 124;

#[derive(Debug)]
enum ModelPreparationPlan {
    BeginPermissionMigration(ModelPaths),
    Failed(ModelPreparationFailure),
}

impl ModelPreparationPlan {
    const fn starts_permission_migration(&self) -> bool {
        matches!(self, Self::BeginPermissionMigration(_))
    }
}

fn model_preparation_plan(result: Result<VerifiedModel, ModelStoreError>) -> ModelPreparationPlan {
    match result {
        Ok(verified) => ModelPreparationPlan::BeginPermissionMigration(verified.into_paths()),
        Err(ModelStoreError::RepairRequired) => {
            ModelPreparationPlan::Failed(ModelPreparationFailure::RepairRequired)
        }
        Err(_) => ModelPreparationPlan::Failed(ModelPreparationFailure::Storage),
    }
}

fn spawn_model_preparation_worker_with<F>(
    prepare: F,
) -> (
    Receiver<Result<VerifiedModel, ModelStoreError>>,
    JoinHandle<()>,
)
where
    F: FnOnce() -> Result<VerifiedModel, ModelStoreError> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = sender.send(prepare());
    });
    (receiver, worker)
}

fn spawn_model_preparation_worker() -> (
    Receiver<Result<VerifiedModel, ModelStoreError>>,
    JoinHandle<()>,
) {
    spawn_model_preparation_worker_with(prepare_runtime_model)
}

/// Owns one preparation attempt. Dropping an unfinished task deliberately
/// detaches its `JoinHandle`; the transactional model store is restart-safe,
/// and AppKit teardown must never wait for model I/O.
struct ModelPreparationTask {
    events: Receiver<Result<VerifiedModel, ModelStoreError>>,
    worker: Option<JoinHandle<()>>,
}

impl ModelPreparationTask {
    fn new(
        events: Receiver<Result<VerifiedModel, ModelStoreError>>,
        worker: JoinHandle<()>,
    ) -> Self {
        Self {
            events,
            worker: Some(worker),
        }
    }

    fn try_recv(&self) -> Result<Result<VerifiedModel, ModelStoreError>, TryRecvError> {
        self.events.try_recv()
    }

    fn join_completed(mut self) -> thread::Result<()> {
        self.worker
            .take()
            .expect("model preparation task must own its worker")
            .join()
    }
}

struct PermissionMigrationWorkerResult {
    paths: ModelPaths,
    migration: Result<PermissionMigrationSuccess, PermissionMigrationRunError>,
}

fn spawn_permission_migration_worker_with<F>(
    paths: ModelPaths,
    migrate: F,
) -> (Receiver<PermissionMigrationWorkerResult>, JoinHandle<()>)
where
    F: FnOnce() -> Result<PermissionMigrationSuccess, PermissionMigrationRunError> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let migration = migrate();
        let _ = sender.send(PermissionMigrationWorkerResult { paths, migration });
    });
    (receiver, worker)
}

fn spawn_permission_migration_worker(
    paths: ModelPaths,
) -> (Receiver<PermissionMigrationWorkerResult>, JoinHandle<()>) {
    spawn_permission_migration_worker_with(paths, run_system_permission_migration)
}

/// Owns one permission migration attempt. Dropping an unfinished task
/// deliberately detaches its worker so AppKit teardown never waits for TCC.
struct PermissionMigrationTask {
    events: Receiver<PermissionMigrationWorkerResult>,
    worker: Option<JoinHandle<()>>,
}

impl PermissionMigrationTask {
    fn new(events: Receiver<PermissionMigrationWorkerResult>, worker: JoinHandle<()>) -> Self {
        Self {
            events,
            worker: Some(worker),
        }
    }

    fn try_recv(&self) -> Result<PermissionMigrationWorkerResult, TryRecvError> {
        self.events.try_recv()
    }

    fn join_completed(mut self) -> thread::Result<()> {
        self.worker
            .take()
            .expect("permission migration task must own its worker")
            .join()
    }
}

fn prepare_runtime_model() -> Result<VerifiedModel, ModelStoreError> {
    let application_support = application_support_root()?;
    let executable =
        std::env::current_exe().map_err(|error| ModelStoreError::Environment(error.to_string()))?;
    let resources =
        resources_dir_from_executable(&executable).map_err(ModelStoreError::Environment)?;
    let bundled = bundled_model_directory(&resources);
    resolve_model(&application_support, Some(&bundled))
}

struct RuntimePreferences<R: RawPreferenceStore> {
    current: Preferences,
    repository: PreferenceRepository<R>,
}

impl<R: RawPreferenceStore> RuntimePreferences<R> {
    fn new(current: Preferences, repository: PreferenceRepository<R>) -> Self {
        Self {
            current,
            repository,
        }
    }

    fn current(&self) -> Preferences {
        self.current
    }

    // The live UI splits mutation from persistence so menu and gate state are
    // updated before synchronous user-defaults I/O; this combined form is the
    // repository-level command contract exercised by the focused unit test.
    #[allow(dead_code)]
    fn apply(&mut self, command: MenuCommand) -> Result<(), ()> {
        self.apply_in_memory(command)?;
        self.persist().map_err(|_| ())
    }

    fn apply_in_memory(&mut self, command: MenuCommand) -> Result<(), ()> {
        match command {
            MenuCommand::ResetTrigger => self.current.trigger = TriggerKey::FnGlobe,
            MenuCommand::SetThreshold(threshold) => self.current.threshold = threshold,
            MenuCommand::BeginTriggerAssignment { .. } | MenuCommand::SetAppendSpace(_) => {
                return Err(())
            }
        }
        Ok(())
    }

    fn select_trigger(&mut self, trigger: TriggerKey) {
        self.current.trigger = trigger;
    }

    fn persist(&mut self) -> Result<(), PreferenceError> {
        self.repository.save(self.current)
    }

    #[cfg(test)]
    fn saved(&self) -> Preferences {
        self.repository.load()
    }
}

fn apply_output_menu_command<R: RawOutputPreferenceStore>(
    command: MenuCommand,
    preferences: &mut OutputPreferenceController<R>,
) -> Result<(), OutputPreferenceError> {
    match command {
        MenuCommand::SetAppendSpace(value) => preferences.set_append_space(value),
        _ => Ok(()),
    }
}

struct MicrophonePermissionRuntime {
    flow: MicrophonePermissionFlow,
    completion_sender: Sender<()>,
    completions: Receiver<()>,
}

impl Default for MicrophonePermissionRuntime {
    fn default() -> Self {
        let (completion_sender, completions) = mpsc::channel();
        Self {
            flow: MicrophonePermissionFlow::default(),
            completion_sender,
            completions,
        }
    }
}

impl MicrophonePermissionRuntime {
    fn completion_sender(&self) -> Sender<()> {
        self.completion_sender.clone()
    }

    fn permission_needed(
        &mut self,
        authorization: MicrophoneAuthorization,
        boundary: &mut impl MicrophonePermissionBoundary,
    ) {
        self.flow.permission_needed(authorization, boundary);
    }

    fn drain_completions(
        &mut self,
        mut authorization: impl FnMut() -> MicrophoneAuthorization,
        boundary: &mut impl MicrophonePermissionBoundary,
    ) -> bool {
        if self.completions.try_recv().is_err() {
            return false;
        }
        while self.completions.try_recv().is_ok() {}
        self.flow.request_completed(authorization(), boundary);
        true
    }
}

struct SystemMicrophonePermissionBoundary {
    completion_sender: Sender<()>,
}

impl MicrophonePermissionBoundary for SystemMicrophonePermissionBoundary {
    fn request_access(&mut self) -> bool {
        let completion_sender = self.completion_sender.clone();
        permissions::request_microphone_access(move || {
            let _ = completion_sender.send(());
        })
    }

    fn open_settings(&mut self) -> bool {
        permissions::open_settings(crate::state::PermissionKind::Microphone)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapState {
    Lost,
    Restored,
}

impl TapState {
    const fn event(self) -> AppEvent {
        match self {
            Self::Lost => AppEvent::EventTapLost,
            Self::Restored => AppEvent::EventTapRestored,
        }
    }
}

#[derive(Default)]
struct DeferredTapState {
    pending: Option<TapState>,
}

impl DeferredTapState {
    fn observe(&mut self, status: &AppStatus, state: TapState) -> Option<AppEvent> {
        if is_dictation_in_flight(status) {
            self.pending = Some(state);
            None
        } else {
            Some(state.event())
        }
    }

    fn take_when_idle(&mut self, status: &AppStatus) -> Option<AppEvent> {
        if is_dictation_in_flight(status) {
            None
        } else {
            self.pending.take().map(TapState::event)
        }
    }
}

#[derive(Default)]
struct AssignmentTracker {
    active_epoch: Option<AssignmentEpoch>,
}

impl AssignmentTracker {
    fn begin(&mut self, epoch: AssignmentEpoch) {
        self.active_epoch = Some(epoch);
    }

    fn cancel_unless_ready(&mut self, status: &AppStatus, control: &HotkeyControl) {
        if status == &AppStatus::Ready {
            return;
        }
        if let Some(epoch) = self.active_epoch.take() {
            control.cancel_assignment(epoch);
        }
        control.cancel_current_assignment();
    }

    fn accept_selection(
        &mut self,
        trigger: TriggerKey,
        epoch: AssignmentEpoch,
        status: &AppStatus,
    ) -> Option<TriggerKey> {
        if status == &AppStatus::Ready && self.active_epoch == Some(epoch) {
            self.active_epoch = None;
            Some(trigger)
        } else {
            None
        }
    }

    fn accept_cancellation(&mut self, epoch: AssignmentEpoch) -> bool {
        if self.active_epoch == Some(epoch) {
            self.active_epoch = None;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.active_epoch.is_some()
    }
}

const fn is_dictation_in_flight(status: &AppStatus) -> bool {
    matches!(status, AppStatus::Recording | AppStatus::Recognizing)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerKind {
    DrainEvents,
    AutomaticUpdateCheck,
    PollPermissions,
    FinishCapture,
    CaptureLimit,
    PasteCommand,
    RestorePasteboard,
    ResetError,
}

trait PasteInsertion {
    fn paste(&mut self) -> Result<(), InsertError>;
    fn restore(&mut self) -> Result<(), InsertError>;
    fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError;
}

impl PasteInsertion for PendingTextInsertion {
    fn paste(&mut self) -> Result<(), InsertError> {
        PendingTextInsertion::paste(self)
    }

    fn restore(&mut self) -> Result<(), InsertError> {
        PendingTextInsertion::restore(self)
    }

    fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
        PendingTextInsertion::restore_after_paste_failure(self, primary)
    }
}

trait PasteFlowBoundary {
    fn schedule(&mut self, kind: TimerKind, delay_ms: u64);
    fn finish_and_drain_hotkeys(&mut self, result: Result<(), InsertError>);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteFlowState {
    AwaitingPaste,
    AwaitingRestore,
    Finished,
}

struct PasteFlow<I> {
    insertion: I,
    state: PasteFlowState,
}

impl<I: PasteInsertion> PasteFlow<I> {
    fn begin(insertion: I, boundary: &mut impl PasteFlowBoundary) -> Self {
        boundary.schedule(TimerKind::PasteCommand, PASTEBOARD_SETTLE_DELAY_MS);
        Self {
            insertion,
            state: PasteFlowState::AwaitingPaste,
        }
    }

    fn handle_timer(&mut self, kind: TimerKind, boundary: &mut impl PasteFlowBoundary) {
        match (self.state, kind) {
            (PasteFlowState::AwaitingPaste, TimerKind::PasteCommand) => {
                match self.insertion.paste() {
                    Ok(()) => {
                        self.state = PasteFlowState::AwaitingRestore;
                        boundary
                            .schedule(TimerKind::RestorePasteboard, PASTEBOARD_RESTORE_DELAY_MS);
                    }
                    Err(primary) => {
                        let error = self.insertion.restore_after_paste_failure(primary);
                        self.state = PasteFlowState::Finished;
                        boundary.finish_and_drain_hotkeys(Err(error));
                    }
                }
            }
            (PasteFlowState::AwaitingRestore, TimerKind::RestorePasteboard) => {
                let result = self.insertion.restore();
                self.state = PasteFlowState::Finished;
                boundary.finish_and_drain_hotkeys(result);
            }
            _ => {}
        }
    }

    fn is_finished(&self) -> bool {
        self.state == PasteFlowState::Finished
    }

    fn restore_on_shutdown(&mut self) -> Result<(), InsertError> {
        if self.is_finished() {
            return Ok(());
        }
        let result = self.insertion.restore();
        self.state = PasteFlowState::Finished;
        result
    }
}

struct TimerContext {
    // Points into the pinned `Runtime` that owns this context. Every timer is
    // removed on that same main run loop before the runtime can be dropped.
    runtime: *mut Runtime,
    kind: TimerKind,
}

struct ScheduledTimer {
    timer: CFRunLoopTimer,
    _context: Box<TimerContext>,
}

impl ScheduledTimer {
    fn new(
        run_loop: &CFRunLoop,
        runtime: *mut Runtime,
        kind: TimerKind,
        delay_ms: u64,
        repeat_ms: Option<u64>,
    ) -> Self {
        let mut context = Box::new(TimerContext { runtime, kind });
        let mut cf_context = CFRunLoopTimerContext {
            version: 0,
            info: (&mut *context as *mut TimerContext).cast::<c_void>(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer = CFRunLoopTimer::new(
            CFDate::now().abs_time() + milliseconds_to_seconds(delay_ms),
            repeat_ms.map(milliseconds_to_seconds).unwrap_or(0.0),
            0,
            0,
            timer_fired,
            &mut cf_context,
        );
        run_loop.add_timer(&timer, unsafe { kCFRunLoopCommonModes });
        Self {
            timer,
            _context: context,
        }
    }

    fn remove(self, run_loop: &CFRunLoop) {
        run_loop.remove_timer(&self.timer, unsafe { kCFRunLoopCommonModes });
    }
}

const fn milliseconds_to_seconds(milliseconds: u64) -> f64 {
    milliseconds as f64 / 1_000.0
}

extern "C" fn timer_fired(_timer: CFRunLoopTimerRef, raw_context: *mut c_void) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let context = raw_context.cast::<TimerContext>();
        if context.is_null() {
            return;
        }
        let runtime = (*context).runtime;
        if runtime.is_null() {
            return;
        }
        (*runtime).handle_timer((*context).kind);
    }));

    if result.is_err() {
        tracing::error!(error_category = "timer_callback_panic");
    }
}

/// Main-thread owner of the reducer and every macOS UI/input component.
pub struct Runtime {
    controller: AppController,
    menu: MenuBar,
    preferences: RuntimePreferences<SystemPreferenceStore>,
    output_preferences: OutputPreferenceController<SystemOutputPreferenceStore>,
    menu_commands: Receiver<MenuCommand>,
    hotkey_control: HotkeyControl,
    assignment: AssignmentTracker,
    menu_readiness: MenuReadiness,
    recorder: AudioRecorder,
    hotkey: Option<HotkeyListener>,
    hotkey_sender: Sender<HotkeySignal>,
    hotkey_events: Receiver<HotkeySignal>,
    asr_commands: Sender<AsrCommand>,
    asr_events: Receiver<AsrEvent>,
    asr_worker: Option<JoinHandle<()>>,
    asr_connected: bool,
    model_preparation: Option<ModelPreparationTask>,
    permission_migration: Option<PermissionMigrationTask>,
    prepared_model_paths: Option<ModelPaths>,
    permission_build_identity: Option<BuildIdentity>,
    updater: Option<SystemUpdaterLane>,
    orderly_quit: OrderlyQuitGate,
    run_loop: CFRunLoop,
    drain_timer: Option<ScheduledTimer>,
    updater_timer: Option<ScheduledTimer>,
    permission_timer: Option<ScheduledTimer>,
    finish_timer: Option<ScheduledTimer>,
    capture_limit_timer: Option<ScheduledTimer>,
    insertion_timer: Option<ScheduledTimer>,
    error_timer: Option<ScheduledTimer>,
    pending_insertion: Option<PasteFlow<PendingTextInsertion>>,
    applied_permissions: PermissionSnapshot,
    microphone_permissions: MicrophonePermissionRuntime,
    tap_needs_retry: bool,
    deferred_tap_state: DeferredTapState,
    _pin: PhantomPinned,
}

impl Runtime {
    /// Creates and starts the complete runtime. The returned box must remain
    /// alive until `NSApplication::run` returns.
    pub fn start(mtm: MainThreadMarker) -> Pin<Box<Self>> {
        let updater_config = match load_production_updater_config() {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(
                    error_category = "updater_production_config",
                    error = %error
                );
                None
            }
        };
        Self::start_with_updater_config(mtm, updater_config)
    }

    pub(crate) fn start_with_updater_config(
        _mtm: MainThreadMarker,
        updater_config: Option<UpdaterLaunchConfig>,
    ) -> Pin<Box<Self>> {
        let (hotkey_sender, hotkey_events) = mpsc::channel();
        let (menu_sender, menu_commands) = mpsc::channel();
        let (asr_commands, asr_command_receiver) = mpsc::channel();
        let (asr_event_sender, asr_events) = mpsc::channel();
        let asr_worker = spawn_asr_worker(asr_command_receiver, asr_event_sender);
        let (model_preparation_events, model_preparation_worker) = spawn_model_preparation_worker();
        let model_preparation =
            ModelPreparationTask::new(model_preparation_events, model_preparation_worker);
        let preference_repository = PreferenceRepository::new(SystemPreferenceStore::new());
        let preferences = preference_repository.load();
        let output_preferences = OutputPreferenceController::load(OutputPreferenceRepository::new(
            SystemOutputPreferenceStore::standard(),
        ));
        let append_space = output_preferences.current().append_space;
        let hotkey_control = HotkeyControl::new(preferences);
        let menu_readiness = MenuReadiness::new(false);
        let updater = updater_config.map(SystemUpdaterLane::production);

        let mut runtime = Box::pin(Self {
            controller: AppController::new(),
            menu: MenuBar::new(
                preferences,
                append_space,
                menu_sender,
                hotkey_control.clone(),
                menu_readiness.clone(),
            ),
            preferences: RuntimePreferences::new(preferences, preference_repository),
            output_preferences,
            menu_commands,
            hotkey_control,
            assignment: AssignmentTracker::default(),
            menu_readiness,
            recorder: AudioRecorder::new(),
            hotkey: None,
            hotkey_sender,
            hotkey_events,
            asr_commands,
            asr_events,
            asr_worker: Some(asr_worker),
            asr_connected: true,
            model_preparation: Some(model_preparation),
            permission_migration: None,
            prepared_model_paths: None,
            permission_build_identity: None,
            updater,
            orderly_quit: OrderlyQuitGate::default(),
            run_loop: CFRunLoop::get_main(),
            drain_timer: None,
            updater_timer: None,
            permission_timer: None,
            finish_timer: None,
            capture_limit_timer: None,
            insertion_timer: None,
            error_timer: None,
            pending_insertion: None,
            applied_permissions: PermissionSnapshot::default(),
            microphone_permissions: MicrophonePermissionRuntime::default(),
            tap_needs_retry: false,
            deferred_tap_state: DeferredTapState::default(),
            _pin: PhantomPinned,
        });

        // SAFETY: The runtime has just been pinned, contains `PhantomPinned`,
        // and is never exposed without its `Pin`. Its timers may therefore
        // retain this address until `Drop` removes them.
        let runtime_ref = unsafe { Pin::as_mut(&mut runtime).get_unchecked_mut() };
        runtime_ref.install_repeating_timers();
        runtime_ref.initialize_updater();
        tracing::info!(
            lifecycle = "started",
            state = status_name(runtime_ref.controller.status())
        );
        runtime
    }

    fn install_repeating_timers(&mut self) {
        let runtime = self as *mut Self;
        self.drain_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            TimerKind::DrainEvents,
            EVENT_DRAIN_MS,
            Some(EVENT_DRAIN_MS),
        ));
    }

    fn initialize_updater(&mut self) {
        let effects = self
            .updater
            .as_mut()
            .map(SystemUpdaterLane::launch)
            .unwrap_or_default();
        self.apply_updater_effects(effects);
        self.render_updater_menu();
    }

    fn start_permission_setup(&mut self) {
        if self.permission_timer.is_some() {
            return;
        }
        let runtime = self as *mut Self;
        self.permission_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            TimerKind::PollPermissions,
            PERMISSION_POLL_MS,
            Some(PERMISSION_POLL_MS),
        ));
        self.poll_permissions();
    }

    fn handle_timer(&mut self, kind: TimerKind) {
        match kind {
            TimerKind::DrainEvents => self.drain_events(),
            TimerKind::AutomaticUpdateCheck => self.handle_automatic_update_timer(),
            TimerKind::PollPermissions => self.poll_permissions(),
            TimerKind::FinishCapture => self.finish_capture(),
            TimerKind::CaptureLimit => {
                self.dispatch(AppEvent::CaptureLimitReached);
            }
            TimerKind::PasteCommand | TimerKind::RestorePasteboard => {
                self.advance_pending_paste(kind);
            }
            TimerKind::ResetError => self.dispatch(AppEvent::ErrorTimerFired),
        }
    }

    fn handle_automatic_update_timer(&mut self) {
        cancel_timer(&self.run_loop, &mut self.updater_timer);
        let effects = self
            .updater
            .as_mut()
            .map(SystemUpdaterLane::automatic_check_due)
            .unwrap_or_default();
        self.apply_updater_effects(effects);
        self.render_updater_menu();
    }

    fn apply_updater_effects(&mut self, effects: Vec<UpdaterRuntimeEffect>) {
        for effect in effects {
            match effect {
                UpdaterRuntimeEffect::ScheduleAt(deadline) => {
                    self.replace_updater_timer(deadline);
                }
                UpdaterRuntimeEffect::RequestOrderlyQuit => self.orderly_quit.request(),
            }
        }
    }

    fn replace_updater_timer(&mut self, deadline: u64) {
        cancel_timer(&self.run_loop, &mut self.updater_timer);
        let delay_ms = deadline
            .saturating_sub(SystemClock.now())
            .saturating_mul(1_000);
        let runtime = self as *mut Self;
        self.updater_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            TimerKind::AutomaticUpdateCheck,
            delay_ms,
            None,
        ));
    }

    fn drain_events(&mut self) {
        self.drain_menu_actions();
        self.drain_updater_worker_results();
        self.drain_updater_actions();
        self.drain_model_preparation();
        self.drain_permission_migration();
        self.drain_microphone_permission_completions();

        self.drain_menu_commands();

        if self.pending_insertion.is_none() {
            self.drain_hotkey_events();
        }

        loop {
            match self.asr_events.try_recv() {
                Ok(AsrEvent::Loaded(result)) => self.dispatch(AppEvent::ModelLoaded(result)),
                Ok(AsrEvent::Recognized(result)) => {
                    self.dispatch(AppEvent::RecognitionFinished(result));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.asr_connected {
                        self.asr_connected = false;
                        tracing::error!(error_category = "asr_worker_disconnected");
                        self.dispatch(AppEvent::ModelLoaded(Err(
                            "ASR worker unavailable".to_owned()
                        )));
                    }
                    break;
                }
            }
        }
        self.try_orderly_quit();
    }

    fn drain_model_preparation(&mut self) {
        let received = self
            .model_preparation
            .as_ref()
            .map(ModelPreparationTask::try_recv);
        let result = match received {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => Some(Err(ModelStoreError::Environment(
                "model preparation worker disconnected".to_owned(),
            ))),
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        let Some(result) = result else {
            return;
        };

        if let Some(task) = self.model_preparation.take() {
            if task.join_completed().is_err() {
                tracing::error!(error_category = "model_preparation_worker_panic");
            }
        }

        let plan = model_preparation_plan(result);
        debug_assert_eq!(
            plan.starts_permission_migration(),
            matches!(&plan, ModelPreparationPlan::BeginPermissionMigration(_))
        );
        match plan {
            ModelPreparationPlan::BeginPermissionMigration(paths) => {
                self.begin_permission_migration(paths);
            }
            ModelPreparationPlan::Failed(failure) => {
                self.dispatch(AppEvent::ModelPreparationFailed(failure));
            }
        }
    }

    fn begin_permission_migration(&mut self, paths: ModelPaths) {
        if self.permission_migration.is_some() {
            return;
        }
        self.dispatch(AppEvent::PermissionMigrationStarted);
        self.prepared_model_paths = Some(paths.clone());
        let (events, worker) = spawn_permission_migration_worker(paths);
        self.permission_migration = Some(PermissionMigrationTask::new(events, worker));
    }

    fn retry_permission_migration(&mut self) {
        if self.permission_migration.is_some() {
            return;
        }
        let Some(paths) = self.prepared_model_paths.take() else {
            return;
        };
        self.begin_permission_migration(paths);
    }

    fn drain_permission_migration(&mut self) {
        let received = self
            .permission_migration
            .as_ref()
            .map(PermissionMigrationTask::try_recv);
        let completed = match received {
            Some(Ok(result)) => Some(Some(result)),
            Some(Err(TryRecvError::Disconnected)) => Some(None),
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        let Some(completed) = completed else {
            return;
        };

        if let Some(task) = self.permission_migration.take() {
            if task.join_completed().is_err() {
                tracing::error!(error_category = "permission_migration_worker_panic");
            }
        }

        let Some(result) = completed else {
            tracing::error!(error_category = "permission_migration_worker_disconnected");
            self.dispatch(AppEvent::PermissionMigrationFailed);
            return;
        };
        match result.migration {
            Ok(success) => {
                self.prepared_model_paths = None;
                self.permission_build_identity = match success {
                    PermissionMigrationSuccess::Release(identity) => Some(identity),
                    PermissionMigrationSuccess::DevelopmentBypass => None,
                };
                self.dispatch(AppEvent::PermissionMigrationCompleted);
                self.start_permission_setup();
                if self
                    .asr_commands
                    .send(AsrCommand::Load(result.paths))
                    .is_err()
                {
                    self.dispatch(AppEvent::ModelLoaded(Err(
                        "ASR worker unavailable".to_owned()
                    )));
                }
            }
            Err(error) => {
                tracing::error!(error = ?error, error_category = "permission_migration");
                self.prepared_model_paths = Some(result.paths);
                self.dispatch(AppEvent::PermissionMigrationFailed);
            }
        }
    }

    fn begin_model_preparation(&mut self) {
        if self.model_preparation.is_some() {
            return;
        }
        self.dispatch(AppEvent::ModelPreparationStarted);
        let (events, worker) = spawn_model_preparation_worker();
        self.model_preparation = Some(ModelPreparationTask::new(events, worker));
    }

    fn drain_hotkey_events(&mut self) {
        let hotkey_events: Vec<_> = self.hotkey_events.try_iter().collect();
        for signal in hotkey_events {
            self.handle_hotkey(signal);
        }
    }

    fn drain_menu_commands(&mut self) {
        let menu_commands: Vec<_> = self.menu_commands.try_iter().collect();
        for command in menu_commands {
            self.handle_menu_command(command);
        }
    }

    fn drain_menu_actions(&mut self) {
        while let Some(action) = self.menu.take_action() {
            match action {
                MenuAction::OpenPermission(permission) => {
                    if !permissions::open_settings(permission) {
                        tracing::warn!(error_category = "open_permission_settings");
                    }
                }
                MenuAction::RetryModelPreparation => self.begin_model_preparation(),
                MenuAction::RetryPermissionMigration => self.retry_permission_migration(),
            }
        }
    }

    fn drain_updater_worker_results(&mut self) {
        let Some(updater) = self.updater.as_mut() else {
            return;
        };
        let (effects, handled) = updater.drain_worker_results();
        self.apply_updater_effects(effects);
        if handled {
            self.render_updater_menu();
        }
    }

    fn drain_updater_actions(&mut self) {
        while let Some(action) = self.menu.take_updater_action() {
            if self.updater.is_none() {
                continue;
            }
            let effects = match action {
                UpdaterMenuAction::CheckForUpdates => self
                    .updater
                    .as_mut()
                    .map(SystemUpdaterLane::manual_check)
                    .unwrap_or_default(),
                UpdaterMenuAction::DownloadUpdate => self
                    .updater
                    .as_mut()
                    .map(SystemUpdaterLane::request_download)
                    .unwrap_or_default(),
                UpdaterMenuAction::RetryUpdate => {
                    let retry_is_manual_check = matches!(
                        self.updater.as_ref().map(SystemUpdaterLane::state),
                        Some(UpdaterState::Failed {
                            retry: RetryAction::ManualCheck,
                            ..
                        })
                    );
                    if retry_is_manual_check {
                        self.updater
                            .as_mut()
                            .map(SystemUpdaterLane::manual_check)
                            .unwrap_or_default()
                    } else {
                        self.updater
                            .as_mut()
                            .map(SystemUpdaterLane::retry)
                            .unwrap_or_default()
                    }
                }
                UpdaterMenuAction::OpenDownloadedUpdate => {
                    if self.pending_insertion.is_none() {
                        self.drain_hotkey_events();
                    }
                    if updater_open_allowed(
                        self.controller.status(),
                        self.pending_insertion.is_some(),
                    ) {
                        self.updater
                            .as_mut()
                            .map(SystemUpdaterLane::request_open)
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                }
            };
            self.apply_updater_effects(effects);
            self.render_updater_menu();
        }
    }

    fn render_updater_menu(&self) {
        let open_enabled =
            updater_open_allowed(self.controller.status(), self.pending_insertion.is_some());
        self.menu.render_updater(
            self.updater.as_ref().map(SystemUpdaterLane::state),
            open_enabled,
        );
    }

    fn try_orderly_quit(&mut self) {
        if !self
            .orderly_quit
            .take_if_ready(self.controller.status(), self.pending_insertion.is_some())
        {
            return;
        }
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let application = NSApplication::sharedApplication(mtm);
        unsafe { application.terminate(None) };
    }

    fn drain_microphone_permission_completions(&mut self) {
        let mut boundary = SystemMicrophonePermissionBoundary {
            completion_sender: self.microphone_permissions.completion_sender(),
        };
        let should_repoll = self.microphone_permissions.drain_completions(
            SystemPermissionProbe::microphone_authorization,
            &mut boundary,
        );
        if should_repoll {
            self.poll_permissions();
        }
    }

    fn handle_hotkey(&mut self, signal: HotkeySignal) {
        match signal {
            HotkeySignal::Pressed => {
                self.dispatch(AppEvent::TriggerPressed);
            }
            HotkeySignal::Released { short } => {
                self.dispatch(AppEvent::TriggerReleased { short });
            }
            HotkeySignal::Cancelled => {
                self.dispatch(AppEvent::TriggerCancelled);
            }
            HotkeySignal::TapLost => {
                self.tap_needs_retry = true;
                self.observe_tap_state(TapState::Lost);
            }
            HotkeySignal::TapRestored => {
                self.tap_needs_retry = false;
                self.observe_tap_state(TapState::Restored);
            }
            HotkeySignal::AssignmentSelected { trigger, epoch } => {
                if let Some(trigger) =
                    self.assignment
                        .accept_selection(trigger, epoch, self.controller.status())
                {
                    self.preferences.select_trigger(trigger);
                    self.publish_preferences();
                    self.menu.render(self.controller.status());
                }
            }
            HotkeySignal::AssignmentCancelled { epoch }
                if self.assignment.accept_cancellation(epoch) =>
            {
                self.menu.render(self.controller.status());
            }
            HotkeySignal::AssignmentCancelled { .. } => {}
        }
    }

    fn handle_menu_command(&mut self, command: MenuCommand) {
        if let MenuCommand::BeginTriggerAssignment { epoch } = command {
            if self.controller.status() == &AppStatus::Ready {
                self.assignment.begin(epoch);
                self.menu.render_assignment();
            } else {
                self.hotkey_control.cancel_assignment(epoch);
            }
            return;
        }

        if let MenuCommand::SetAppendSpace(value) = command {
            if apply_output_menu_command(
                MenuCommand::SetAppendSpace(value),
                &mut self.output_preferences,
            )
            .is_err()
            {
                tracing::warn!(error_category = "output_preference_persistence");
            }
            self.menu
                .render_append_space(self.output_preferences.current().append_space);
            return;
        }

        if self.preferences.apply_in_memory(command).is_ok() {
            self.publish_preferences();
        }
    }

    fn publish_preferences(&mut self) {
        let preferences = self.preferences.current();
        self.menu.render_preferences(preferences);
        self.hotkey_control.set_preferences(preferences);
        if self.preferences.persist().is_err() {
            tracing::warn!(error_category = "preference_persistence");
        }
    }

    fn poll_permissions(&mut self) {
        let permissions = SystemPermissionProbe::check();
        if let Some(identity) = self.permission_build_identity.as_ref() {
            let mut marker_store = SystemPermissionMigrationStore::standard();
            if persist_setup_completion_if_granted(identity, permissions, &mut marker_store)
                .is_err()
            {
                tracing::warn!(error_category = "permission_setup_marker_persistence");
            }
        }
        if permissions.microphone {
            let mut boundary = SystemMicrophonePermissionBoundary {
                completion_sender: self.microphone_permissions.completion_sender(),
            };
            self.microphone_permissions
                .permission_needed(MicrophoneAuthorization::Authorized, &mut boundary);
        }
        let may_change_idle_state = !matches!(
            self.controller.status(),
            AppStatus::Recording | AppStatus::Recognizing
        );
        if permissions != self.applied_permissions && may_change_idle_state {
            self.applied_permissions = permissions;
            self.dispatch(AppEvent::PermissionsChanged(permissions));
        }

        if !permissions.input_monitoring {
            self.tap_needs_retry = false;
            reset_hotkey_before_drop(&self.hotkey_control, &self.hotkey_sender);
            self.hotkey.take();
            return;
        }

        if self.tap_needs_retry {
            reset_hotkey_before_drop(&self.hotkey_control, &self.hotkey_sender);
            self.hotkey.take();
        }
        if self.hotkey.is_none() {
            match HotkeyListener::install(self.hotkey_sender.clone(), self.hotkey_control.clone()) {
                Ok(listener) => {
                    self.hotkey = Some(listener);
                    self.tap_needs_retry = false;
                    self.observe_tap_state(TapState::Restored);
                }
                Err(_) => {
                    self.tap_needs_retry = true;
                    self.observe_tap_state(TapState::Lost);
                }
            }
        }
    }

    fn dispatch(&mut self, event: AppEvent) {
        let effects = self.controller.handle(event);
        self.menu_readiness
            .set_ready(self.controller.status() == &AppStatus::Ready);
        self.assignment
            .cancel_unless_ready(self.controller.status(), &self.hotkey_control);
        self.menu.render(self.controller.status());
        self.render_updater_menu();
        tracing::debug!(state = status_name(self.controller.status()));
        for effect in effects {
            self.execute(effect);
        }
        self.flush_deferred_tap_state();
    }

    fn observe_tap_state(&mut self, state: TapState) {
        if let Some(event) = self
            .deferred_tap_state
            .observe(self.controller.status(), state)
        {
            self.dispatch(event);
        }
    }

    fn flush_deferred_tap_state(&mut self) {
        if let Some(event) = self
            .deferred_tap_state
            .take_when_idle(self.controller.status())
        {
            self.dispatch(event);
        }
    }

    fn execute(&mut self, effect: Effect) {
        match effect {
            Effect::OpenPermission(permission) => {
                if permission == crate::state::PermissionKind::Microphone {
                    let authorization = SystemPermissionProbe::microphone_authorization();
                    let mut boundary = SystemMicrophonePermissionBoundary {
                        completion_sender: self.microphone_permissions.completion_sender(),
                    };
                    self.microphone_permissions
                        .permission_needed(authorization, &mut boundary);
                } else if !permissions::open_settings(permission) {
                    tracing::warn!(error_category = "open_permission_settings");
                }
            }
            Effect::StartCapture => {
                match capture_start_result_event(self.recorder.start(), &self.hotkey_control) {
                    Ok(()) => {
                        self.replace_capture_limit_timer(MAX_CAPTURE_MS);
                        tracing::debug!(lifecycle = "capture_started");
                    }
                    Err(event) => {
                        tracing::warn!(error_category = "microphone_start");
                        self.dispatch(event);
                    }
                }
            }
            Effect::AbortCapture => {
                self.cancel_finish_timer();
                self.cancel_capture_limit_timer();
                self.recorder.abort();
                tracing::debug!(lifecycle = "capture_aborted");
            }
            Effect::FinishCaptureAfter { delay_ms } => {
                self.cancel_capture_limit_timer();
                if delay_ms == 0 {
                    self.finish_capture();
                } else {
                    debug_assert_eq!(delay_ms, RELEASE_GRACE_MS);
                    self.replace_finish_timer(delay_ms);
                }
            }
            Effect::Recognize(samples) => {
                tracing::debug!(
                    sample_count = samples.len(),
                    lifecycle = "recognition_started"
                );
                if self
                    .asr_commands
                    .send(AsrCommand::Transcribe(samples))
                    .is_err()
                {
                    tracing::warn!(error_category = "asr_channel");
                    self.dispatch(AppEvent::RecognitionFinished(Err(
                        "ASR worker unavailable".to_owned()
                    )));
                }
            }
            Effect::InsertText(text) => {
                match text_inserter::begin(&text, self.output_preferences.current().append_space) {
                    Ok(insertion) => {
                        let flow = PasteFlow::begin(insertion, self);
                        self.pending_insertion = Some(flow);
                    }
                    Err(error) => {
                        log_text_insertion_error(error);
                        self.dispatch(AppEvent::PasteFinished(Err("insert failed".to_owned())));
                    }
                }
            }
            Effect::ScheduleErrorReset { delay_ms } => {
                self.replace_error_timer(delay_ms);
            }
        }
    }

    fn finish_capture(&mut self) {
        let stop_result = self.recorder.stop();
        match &stop_result {
            Ok(samples) => {
                let sample_count = samples.as_ref().map_or(0, Vec::len);
                tracing::debug!(sample_count, lifecycle = "capture_finished");
            }
            Err(_) => {
                tracing::warn!(error_category = "microphone_stop");
            }
        }
        self.dispatch(capture_result_event(stop_result));
    }

    fn replace_finish_timer(&mut self, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.finish_timer);
        let runtime = self as *mut Self;
        self.finish_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            TimerKind::FinishCapture,
            delay_ms,
            None,
        ));
    }

    fn replace_capture_limit_timer(&mut self, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.capture_limit_timer);
        let runtime = self as *mut Self;
        self.capture_limit_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            TimerKind::CaptureLimit,
            delay_ms,
            None,
        ));
    }

    fn replace_error_timer(&mut self, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.error_timer);
        let runtime = self as *mut Self;
        self.error_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            TimerKind::ResetError,
            delay_ms,
            None,
        ));
    }

    fn replace_insertion_timer(&mut self, kind: TimerKind, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        let runtime = self as *mut Self;
        self.insertion_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            kind,
            delay_ms,
            None,
        ));
    }

    fn advance_pending_paste(&mut self, kind: TimerKind) {
        let Some(mut flow) = self.pending_insertion.take() else {
            return;
        };
        flow.handle_timer(kind, self);
        if !flow.is_finished() {
            self.pending_insertion = Some(flow);
        }
    }

    fn cancel_finish_timer(&mut self) {
        cancel_timer(&self.run_loop, &mut self.finish_timer);
    }

    fn cancel_capture_limit_timer(&mut self) {
        cancel_timer(&self.run_loop, &mut self.capture_limit_timer);
    }
}

fn reset_hotkey_before_drop(control: &HotkeyControl, sender: &Sender<HotkeySignal>) {
    if control.reset_for_listener_removal() {
        let _ = sender.send(HotkeySignal::Cancelled);
    }
}

fn capture_start_result_event(
    result: Result<(), AudioError>,
    control: &HotkeyControl,
) -> Result<(), AppEvent> {
    result.map_err(|_| {
        control.guard_pending_release_after_capture_failure();
        AppEvent::CaptureFailed
    })
}

impl PasteFlowBoundary for Runtime {
    fn schedule(&mut self, kind: TimerKind, delay_ms: u64) {
        self.replace_insertion_timer(kind, delay_ms);
    }

    fn finish_and_drain_hotkeys(&mut self, result: Result<(), InsertError>) {
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        if result.is_ok() {
            tracing::debug!(method = "clipboard", lifecycle = "text_inserted");
        } else if let Err(error) = result {
            log_text_insertion_error(error);
        }
        self.dispatch(AppEvent::PasteFinished(
            result.map_err(|_| "insert failed".to_owned()),
        ));
        self.drain_menu_commands();
        self.drain_hotkey_events();
        self.try_orderly_quit();
    }
}

fn log_text_insertion_error(error: InsertError) {
    tracing::warn!(
        error_category = "text_insertion",
        error_kind = error.kind(),
        diagnostic_stage = ?error.diagnostic_stage(),
        ax_attribute = ?error.ax_attribute(),
        ax_error_code = ?error.ax_error_code(),
        error = %error,
    );
}

pub(crate) fn capture_result_event(result: Result<Option<Vec<f32>>, AudioError>) -> AppEvent {
    match result {
        Ok(samples) => AppEvent::AudioReady(samples),
        Err(_) => AppEvent::CaptureFailed,
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        cancel_timer(&self.run_loop, &mut self.drain_timer);
        cancel_timer(&self.run_loop, &mut self.updater_timer);
        cancel_timer(&self.run_loop, &mut self.permission_timer);
        cancel_timer(&self.run_loop, &mut self.finish_timer);
        cancel_timer(&self.run_loop, &mut self.capture_limit_timer);
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        cancel_timer(&self.run_loop, &mut self.error_timer);
        if let Some(mut flow) = self.pending_insertion.take() {
            if flow.restore_on_shutdown().is_err() {
                tracing::warn!(error_category = "pasteboard_restore_on_shutdown");
            }
        }
        self.recorder.abort();
        self.hotkey.take();
        let _ = self.asr_commands.send(AsrCommand::Shutdown);
        if let Some(worker) = self.asr_worker.take() {
            let _ = worker.join();
        }
        self.model_preparation.take();
        self.permission_migration.take();
        self.updater.take();
        tracing::info!(lifecycle = "terminated");
    }
}

fn cancel_timer(run_loop: &CFRunLoop, slot: &mut Option<ScheduledTimer>) {
    if let Some(timer) = slot.take() {
        timer.remove(run_loop);
    }
}

const fn status_name(status: &AppStatus) -> &'static str {
    match status {
        AppStatus::PreparingModel => "preparing_model",
        AppStatus::ModelRepairRequired => "model_repair_required",
        AppStatus::ModelPreparationFailed => "model_preparation_failed",
        AppStatus::ResettingPermissions => "resetting_permissions",
        AppStatus::PermissionResetFailed => "permission_reset_failed",
        AppStatus::PermissionBlocked(_) => "permission_blocked",
        AppStatus::Ready => "ready",
        AppStatus::Recording => "recording",
        AppStatus::Recognizing => "recognizing",
        AppStatus::Error { .. } => "error",
    }
}

pub fn bundled_model_paths() -> Result<ModelPaths, String> {
    let executable =
        std::env::current_exe().map_err(|_| "current executable unavailable".to_owned())?;
    bundled_model_paths_from_executable(executable)
}

fn bundled_model_paths_from_executable(executable: PathBuf) -> Result<ModelPaths, String> {
    let resources = resources_dir_from_executable(&executable)?;
    let bundled = bundled_model_directory(&resources);
    let manifest = embedded_model_manifest().map_err(|error| error.to_string())?;
    verify_model_directory(&bundled, &manifest)
        .map(VerifiedModel::into_paths)
        .map_err(|error| error.to_string())
}

/// Starts a bounded child process that initializes the embedded model.
pub fn smoke_bundled_model() -> i32 {
    let Ok(executable) = std::env::current_exe() else {
        tracing::error!(error_category = "current_executable");
        return 1;
    };
    let Ok(mut child) = Command::new(executable).arg("--smoke-model-child").spawn() else {
        tracing::error!(error_category = "model_smoke_spawn");
        return 1;
    };
    wait_for_smoke_child(&mut child, SMOKE_MODEL_TIMEOUT)
}

/// Initializes the embedded model inside the watchdog child process.
pub fn smoke_bundled_model_child() -> i32 {
    let Ok(paths) = bundled_model_paths() else {
        tracing::error!(error_category = "model_resources");
        return 1;
    };

    let (commands, command_receiver) = mpsc::channel();
    let (event_sender, events) = mpsc::channel();
    let worker = spawn_asr_worker(command_receiver, event_sender);
    let load_sent = commands.send(AsrCommand::Load(paths)).is_ok();
    let loaded = load_sent && matches!(events.recv(), Ok(AsrEvent::Loaded(Ok(()))));
    let _ = commands.send(AsrCommand::Shutdown);
    let joined = worker.join().is_ok();

    if loaded && joined {
        0
    } else {
        tracing::error!(error_category = "model_load");
        1
    }
}

fn wait_for_smoke_child(child: &mut Child, timeout: Duration) -> i32 {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(1),
            Ok(None) => {}
            Err(_) => {
                tracing::error!(error_category = "model_smoke_wait");
                let _ = child.kill();
                let _ = child.wait();
                return 1;
            }
        }

        if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
            tracing::error!(error_category = "model_load_timeout");
            let _ = child.kill();
            let _ = child.wait();
            return SMOKE_TIMEOUT_EXIT_CODE;
        }
        thread::sleep(SMOKE_CHILD_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;
    use std::thread;
    use std::time::Duration;

    use super::{
        apply_output_menu_command, capture_start_result_event, milliseconds_to_seconds,
        model_preparation_plan, reset_hotkey_before_drop, spawn_model_preparation_worker_with,
        spawn_permission_migration_worker_with, status_name, wait_for_smoke_child,
        AssignmentTracker, DeferredTapState, MicrophonePermissionRuntime, ModelPreparationPlan,
        ModelPreparationTask, PasteFlow, PasteFlowBoundary, PasteInsertion,
        PermissionMigrationTask, RuntimePreferences, TapState, TimerKind, EVENT_DRAIN_MS,
        PERMISSION_POLL_MS,
    };
    use crate::audio::AudioError;
    use crate::constants::{ERROR_VISIBLE_MS, MAX_CAPTURE_MS, RELEASE_GRACE_MS};
    use crate::hotkey::{HotkeyControl, HotkeySignal, KeyboardObservation, ObservationKind};
    use crate::inserter::InsertError;
    use crate::menu::MenuCommand;
    use crate::model_store::{verify_model_directory, ModelManifest, ModelStoreError, MODEL_ID};
    use crate::output_preferences::{
        OutputPreferenceController, OutputPreferenceError, OutputPreferenceRepository,
        RawOutputPreferenceStore,
    };
    use crate::permission_migration::{
        PermissionMigrationError, PermissionMigrationRunError, PermissionMigrationSuccess,
        TccService,
    };
    use crate::permissions::{MicrophoneAuthorization, MicrophonePermissionBoundary};
    use crate::preferences::{
        HoldThreshold, PreferenceError, PreferenceRepository, Preferences, RawPreferenceStore,
        TriggerKey,
    };
    use crate::state::{AppController, AppEvent, AppStatus, Effect, PermissionSnapshot};

    struct RecordingMicrophoneBoundary {
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    fn verified_model_fixture() -> (tempfile::TempDir, crate::model_store::VerifiedModel) {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join(MODEL_ID);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("encoder.int8.onnx"), b"enc").unwrap();
        fs::write(directory.join("decoder.onnx"), b"dec").unwrap();
        fs::write(directory.join("joiner.onnx"), b"join").unwrap();
        fs::write(directory.join("tokens.txt"), b"tok").unwrap();
        let manifest = ModelManifest::from_bytes(
            br#"{"schema":1,"id":"gigaam-v3-rnnt-v1","files":[{"name":"encoder.int8.onnx","size":3,"sha256":"5fb2ab76ed9bda034b192c48c7069359252fccda168d925acc0ae7d316c0b53e"},{"name":"decoder.onnx","size":3,"sha256":"e7502c799b8f76fbed077ff2cd55c906ab144d5b88ef09a71abc70b5fad601f1"},{"name":"joiner.onnx","size":4,"sha256":"58393216032be6257784ac0c6a73efb2a084e27b4cfff1e6acee7b7e6ab93b10"},{"name":"tokens.txt","size":3,"sha256":"1a7674eb4ee78df7e1ac439a93c3fa8e3c945784d4dec9fd8e3011738b2f1d62"}]}"#,
        )
        .unwrap();
        let verified = verify_model_directory(&directory, &manifest).unwrap();
        (temp, verified)
    }

    #[test]
    fn model_preparation_success_only_starts_permission_migration() {
        let failed = model_preparation_plan(Err(ModelStoreError::RepairRequired));
        assert!(matches!(failed, ModelPreparationPlan::Failed(_)));
        assert!(!failed.starts_permission_migration());

        let (_temp, verified) = verified_model_fixture();
        let ready = model_preparation_plan(Ok(verified));
        assert!(matches!(
            ready,
            ModelPreparationPlan::BeginPermissionMigration(_)
        ));
        assert!(ready.starts_permission_migration());
    }

    #[test]
    fn model_preparation_worker_runs_away_from_the_appkit_caller_thread() {
        let caller = thread::current().id();
        let (thread_sender, thread_receiver) = std::sync::mpsc::channel();
        let (results, worker) = spawn_model_preparation_worker_with(move || {
            thread_sender.send(thread::current().id()).unwrap();
            Err(ModelStoreError::RepairRequired)
        });

        assert_ne!(thread_receiver.recv().unwrap(), caller);
        assert!(matches!(
            results.recv().unwrap(),
            Err(ModelStoreError::RepairRequired)
        ));
        worker.join().unwrap();
    }

    #[test]
    fn dropping_model_preparation_task_does_not_wait_for_blocked_worker() {
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
        let (events, worker) = spawn_model_preparation_worker_with(move || {
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            finished_sender.send(()).unwrap();
            Err(ModelStoreError::RepairRequired)
        });
        started_receiver.recv().unwrap();
        let task = ModelPreparationTask::new(events, worker);
        let (dropped_sender, dropped_receiver) = std::sync::mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(task);
            dropped_sender.send(()).unwrap();
        });

        let returned_before_release = dropped_receiver
            .recv_timeout(Duration::from_millis(200))
            .is_ok();
        release_sender.send(()).unwrap();
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        dropper.join().unwrap();

        assert!(
            returned_before_release,
            "dropping a blocked model preparation task waited for its worker"
        );
    }

    #[test]
    fn permission_migration_worker_runs_off_appkit_and_returns_paths_on_failure() {
        let caller = thread::current().id();
        let (_temp, verified) = verified_model_fixture();
        let expected_encoder = verified.paths().encoder().to_owned();
        let (thread_sender, thread_receiver) = std::sync::mpsc::channel();
        let (events, worker) =
            spawn_permission_migration_worker_with(verified.into_paths(), move || {
                thread_sender.send(thread::current().id()).unwrap();
                Err(PermissionMigrationRunError::Migration(
                    PermissionMigrationError::ResetFailed(TccService::ListenEvent),
                ))
            });

        assert_ne!(thread_receiver.recv().unwrap(), caller);
        let result = events.recv().unwrap();
        assert_eq!(result.paths.encoder(), expected_encoder);
        assert!(result.migration.is_err());
        worker.join().unwrap();
    }

    #[test]
    fn dropping_permission_migration_task_does_not_wait_for_blocked_worker() {
        let (_temp, verified) = verified_model_fixture();
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
        let (events, worker) =
            spawn_permission_migration_worker_with(verified.into_paths(), move || {
                started_sender.send(()).unwrap();
                release_receiver.recv().unwrap();
                finished_sender.send(()).unwrap();
                Ok(PermissionMigrationSuccess::DevelopmentBypass)
            });
        started_receiver.recv().unwrap();
        let task = PermissionMigrationTask::new(events, worker);
        let (dropped_sender, dropped_receiver) = std::sync::mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(task);
            dropped_sender.send(()).unwrap();
        });

        let returned_before_release = dropped_receiver
            .recv_timeout(Duration::from_millis(200))
            .is_ok();
        release_sender.send(()).unwrap();
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        dropper.join().unwrap();

        assert!(
            returned_before_release,
            "dropping a blocked permission migration task waited for its worker"
        );
    }

    impl MicrophonePermissionBoundary for RecordingMicrophoneBoundary {
        fn request_access(&mut self) -> bool {
            self.events.borrow_mut().push("request");
            true
        }

        fn open_settings(&mut self) -> bool {
            self.events.borrow_mut().push("open");
            true
        }
    }

    #[derive(Default)]
    struct MemoryRawStore {
        trigger: Option<String>,
        threshold: Option<u64>,
    }

    impl RawPreferenceStore for MemoryRawStore {
        fn trigger_value(&self) -> Option<String> {
            self.trigger.clone()
        }

        fn threshold_value(&self) -> Option<u64> {
            self.threshold
        }

        fn set_trigger_value(&mut self, value: &str) -> Result<(), PreferenceError> {
            self.trigger = Some(value.to_owned());
            Ok(())
        }

        fn set_threshold_value(&mut self, value: u64) -> Result<(), PreferenceError> {
            self.threshold = Some(value);
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryOutputStore {
        value: Option<bool>,
        fail_writes: bool,
    }

    impl RawOutputPreferenceStore for MemoryOutputStore {
        fn append_space(&self) -> Option<bool> {
            self.value
        }

        fn set_append_space(&mut self, value: bool) -> Result<(), OutputPreferenceError> {
            if self.fail_writes {
                Err(OutputPreferenceError::WriteFailed)
            } else {
                self.value = Some(value);
                Ok(())
            }
        }
    }

    #[test]
    fn threshold_command_updates_menu_store_and_future_gate_preferences() {
        let mut model = RuntimePreferences::new(
            Preferences::default(),
            PreferenceRepository::new(MemoryRawStore::default()),
        );
        assert_eq!(
            model.apply(MenuCommand::SetThreshold(HoldThreshold::MS_750)),
            Ok(())
        );
        assert_eq!(model.current().threshold, HoldThreshold::MS_750);
        assert_eq!(model.saved().threshold, HoldThreshold::MS_750);
    }

    #[test]
    fn trailing_space_command_keeps_live_state_when_persistence_fails() {
        let mut preferences =
            OutputPreferenceController::load(OutputPreferenceRepository::new(MemoryOutputStore {
                value: Some(false),
                fail_writes: true,
            }));

        assert_eq!(
            apply_output_menu_command(MenuCommand::SetAppendSpace(true), &mut preferences),
            Err(OutputPreferenceError::WriteFailed)
        );
        assert!(preferences.current().append_space);
    }

    fn key_down(keycode: u16) -> KeyboardObservation {
        KeyboardObservation {
            kind: ObservationKind::KeyDown,
            keycode,
            flags: 0,
            autorepeat: false,
            replay_marker: false,
        }
    }

    #[test]
    fn recorder_start_failure_guards_pending_trigger_release_without_replay() {
        let start = std::time::Instant::now();
        let control = HotkeyControl::new(Preferences::default());
        assert_eq!(
            control.observe_for_test(key_down(63), start),
            Some(HotkeySignal::Pressed)
        );

        let mut controller = AppController::new();
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        assert_eq!(
            controller.handle(AppEvent::TriggerPressed),
            vec![Effect::StartCapture]
        );

        let failure = capture_start_result_event(
            Err(AudioError::StartStream("test failure".to_owned())),
            &control,
        )
        .unwrap_err();
        assert_eq!(failure, AppEvent::CaptureFailed);
        controller.handle(failure);

        assert_eq!(
            control.observe_outcome_for_test(
                KeyboardObservation {
                    kind: ObservationKind::KeyUp,
                    ..key_down(63)
                },
                start + std::time::Duration::from_millis(100),
            ),
            (true, None, 0)
        );
        assert!(matches!(
            controller.status(),
            AppStatus::Error {
                recoverable: true,
                ..
            }
        ));
    }

    #[test]
    fn assignment_is_cancelled_when_status_leaves_ready_for_permission_or_error() {
        for status in [
            AppStatus::PermissionBlocked(crate::state::PermissionKind::InputMonitoring),
            AppStatus::Error {
                message: "runtime error",
                recoverable: true,
            },
        ] {
            let control = HotkeyControl::new(Preferences::default());
            let epoch = control.begin_assignment().unwrap();
            let mut assignment = AssignmentTracker::default();
            assignment.begin(epoch);

            assignment.cancel_unless_ready(&status, &control);

            assert!(!assignment.is_active());
            assert_eq!(
                control.observe_for_test(key_down(49), std::time::Instant::now()),
                None
            );
        }
    }

    #[test]
    fn stale_assignment_selection_cannot_replace_a_newer_assignment() {
        let control = HotkeyControl::new(Preferences::default());
        let first_epoch = control.begin_assignment().unwrap();
        let first_signal = control.observe_for_test(key_down(49), std::time::Instant::now());
        control.observe_for_test(
            KeyboardObservation {
                kind: ObservationKind::KeyUp,
                ..key_down(49)
            },
            std::time::Instant::now(),
        );

        let second_epoch = control.begin_assignment().unwrap();
        let mut assignment = AssignmentTracker::default();
        assignment.begin(second_epoch);
        let mut model = RuntimePreferences::new(
            Preferences::default(),
            PreferenceRepository::new(MemoryRawStore::default()),
        );

        let Some(HotkeySignal::AssignmentSelected {
            trigger,
            epoch: stale_epoch,
        }) = first_signal
        else {
            panic!("first assignment must produce a selected signal");
        };
        assert_eq!(stale_epoch, first_epoch);
        assert_eq!(
            assignment.accept_selection(trigger, stale_epoch, &AppStatus::Ready),
            None
        );
        assert_eq!(model.current().trigger, TriggerKey::FnGlobe);
        assert_eq!(
            assignment.accept_selection(TriggerKey::KeyCode(54), second_epoch, &AppStatus::Ready,),
            Some(TriggerKey::KeyCode(54))
        );
        model.select_trigger(TriggerKey::KeyCode(54));
        assert_eq!(model.current().trigger, TriggerKey::KeyCode(54));
    }

    #[test]
    fn status_cancellation_keeps_only_the_consumed_release_guard() {
        let control = HotkeyControl::new(Preferences::default());
        let epoch = control.begin_assignment().unwrap();
        control.observe_for_test(key_down(49), std::time::Instant::now());
        let mut assignment = AssignmentTracker::default();
        assignment.begin(epoch);

        assignment.cancel_unless_ready(
            &AppStatus::Error {
                message: "runtime error",
                recoverable: true,
            },
            &control,
        );

        assert_eq!(control.current_assignment_epoch(), None);
        assert!(control.suppresses_for_test(
            KeyboardObservation {
                kind: ObservationKind::KeyUp,
                ..key_down(49)
            },
            std::time::Instant::now(),
        ));
    }

    #[test]
    fn listener_removal_cancels_after_an_already_queued_press() {
        let control = HotkeyControl::new(Preferences::default());
        let (sender, receiver) = std::sync::mpsc::channel();
        let pressed = control
            .observe_for_test(key_down(63), std::time::Instant::now())
            .unwrap();
        sender.send(pressed).unwrap();

        reset_hotkey_before_drop(&control, &sender);

        assert_eq!(receiver.recv().unwrap(), HotkeySignal::Pressed);
        assert_eq!(receiver.recv().unwrap(), HotkeySignal::Cancelled);
        assert!(control.begin_assignment().is_some());
    }

    #[test]
    fn quick_assignment_release_before_drain_persists_selected_trigger() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let control = HotkeyControl::new(Preferences::default());
        let epoch = control.begin_assignment().unwrap();
        sender
            .send(MenuCommand::BeginTriggerAssignment { epoch })
            .unwrap();
        let selected = control
            .observe_for_test(key_down(49), std::time::Instant::now())
            .unwrap();
        assert!(control.suppresses_for_test(
            KeyboardObservation {
                kind: ObservationKind::KeyUp,
                ..key_down(49)
            },
            std::time::Instant::now(),
        ));

        let mut assignment = AssignmentTracker::default();
        let MenuCommand::BeginTriggerAssignment {
            epoch: command_epoch,
        } = receiver.recv().unwrap()
        else {
            panic!("assignment command must retain its gate epoch");
        };
        assignment.begin(command_epoch);
        let HotkeySignal::AssignmentSelected {
            trigger,
            epoch: selected_epoch,
        } = selected
        else {
            panic!("assignment key must produce a selected signal");
        };
        let trigger = assignment
            .accept_selection(trigger, selected_epoch, &AppStatus::Ready)
            .unwrap();
        let mut model = RuntimePreferences::new(
            Preferences::default(),
            PreferenceRepository::new(MemoryRawStore::default()),
        );
        model.select_trigger(trigger);
        model.persist().unwrap();

        assert_eq!(command_epoch, epoch);
        assert_eq!(model.saved().trigger, TriggerKey::KeyCode(49));
    }

    fn recognizing_controller() -> AppController {
        let mut controller = AppController::new();
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        controller.handle(AppEvent::TriggerPressed);
        controller.handle(AppEvent::TriggerReleased { short: false });
        assert_eq!(controller.status(), &AppStatus::Recognizing);
        controller
    }

    #[test]
    fn runtime_timings_match_the_product_contract() {
        assert_eq!(EVENT_DRAIN_MS, 50);
        assert_eq!(PERMISSION_POLL_MS, 1_000);
        assert_eq!(RELEASE_GRACE_MS, 180);
        assert_eq!(MAX_CAPTURE_MS, 25_000);
        assert_eq!(ERROR_VISIBLE_MS, 3_000);
        assert_eq!(milliseconds_to_seconds(RELEASE_GRACE_MS), 0.18);
    }

    #[test]
    fn paste_flow_orders_command_restore_finish_and_hotkey_drain() {
        struct RecordingInsertion {
            events: Rc<RefCell<Vec<&'static str>>>,
        }

        impl PasteInsertion for RecordingInsertion {
            fn paste(&mut self) -> Result<(), InsertError> {
                self.events.borrow_mut().push("paste");
                Ok(())
            }

            fn restore(&mut self) -> Result<(), InsertError> {
                self.events.borrow_mut().push("restore");
                Ok(())
            }

            fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
                self.events.borrow_mut().push("failure_restore");
                primary
            }
        }

        struct RecordingBoundary {
            events: Rc<RefCell<Vec<&'static str>>>,
        }

        impl PasteFlowBoundary for RecordingBoundary {
            fn schedule(&mut self, kind: TimerKind, delay_ms: u64) {
                match (kind, delay_ms) {
                    (TimerKind::PasteCommand, 30) => {
                        self.events.borrow_mut().push("schedule_paste")
                    }
                    (TimerKind::RestorePasteboard, 1_000) => {
                        self.events.borrow_mut().push("schedule_restore")
                    }
                    _ => panic!("unexpected timer"),
                }
            }

            fn finish_and_drain_hotkeys(&mut self, result: Result<(), InsertError>) {
                assert_eq!(result, Ok(()));
                self.events.borrow_mut().push("finish");
                self.events.borrow_mut().push("drain_hotkeys");
            }
        }

        let events = Rc::new(RefCell::new(Vec::new()));
        let insertion = RecordingInsertion {
            events: Rc::clone(&events),
        };
        let mut boundary = RecordingBoundary {
            events: Rc::clone(&events),
        };
        let mut flow = PasteFlow::begin(insertion, &mut boundary);

        flow.handle_timer(TimerKind::RestorePasteboard, &mut boundary);
        flow.handle_timer(TimerKind::PasteCommand, &mut boundary);
        flow.handle_timer(TimerKind::PasteCommand, &mut boundary);
        flow.handle_timer(TimerKind::RestorePasteboard, &mut boundary);

        assert_eq!(
            events.borrow().as_slice(),
            [
                "schedule_paste",
                "paste",
                "schedule_restore",
                "restore",
                "finish",
                "drain_hotkeys",
            ]
        );
    }

    #[test]
    fn paste_flow_preserves_insert_error_for_runtime_diagnostics() {
        struct FailingInsertion;

        impl PasteInsertion for FailingInsertion {
            fn paste(&mut self) -> Result<(), InsertError> {
                Err(InsertError::KeyboardEvent)
            }

            fn restore(&mut self) -> Result<(), InsertError> {
                Ok(())
            }

            fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
                primary
            }
        }

        struct RecordingBoundary {
            result: Rc<RefCell<Option<Result<(), InsertError>>>>,
        }

        impl PasteFlowBoundary for RecordingBoundary {
            fn schedule(&mut self, _kind: TimerKind, _delay_ms: u64) {}

            fn finish_and_drain_hotkeys(&mut self, result: Result<(), InsertError>) {
                *self.result.borrow_mut() = Some(result);
            }
        }

        let result = Rc::new(RefCell::new(None));
        let mut boundary = RecordingBoundary {
            result: Rc::clone(&result),
        };
        let mut flow = PasteFlow::begin(FailingInsertion, &mut boundary);

        flow.handle_timer(TimerKind::PasteCommand, &mut boundary);

        assert_eq!(*result.borrow(), Some(Err(InsertError::KeyboardEvent)));
    }

    #[test]
    fn paste_flow_drops_its_restoration_owner_if_boundary_panics() {
        struct RestoringInsertion {
            events: Rc<RefCell<Vec<&'static str>>>,
            restored: bool,
        }

        impl PasteInsertion for RestoringInsertion {
            fn paste(&mut self) -> Result<(), InsertError> {
                self.events.borrow_mut().push("paste");
                Ok(())
            }

            fn restore(&mut self) -> Result<(), InsertError> {
                self.restored = true;
                self.events.borrow_mut().push("restore");
                Ok(())
            }

            fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
                self.restore().unwrap();
                primary
            }
        }

        impl Drop for RestoringInsertion {
            fn drop(&mut self) {
                if !self.restored {
                    self.events.borrow_mut().push("restore_on_drop");
                    self.restored = true;
                }
            }
        }

        struct PanickingBoundary {
            events: Rc<RefCell<Vec<&'static str>>>,
        }

        impl PasteFlowBoundary for PanickingBoundary {
            fn schedule(&mut self, kind: TimerKind, _delay_ms: u64) {
                match kind {
                    TimerKind::PasteCommand => {
                        self.events.borrow_mut().push("schedule_paste");
                    }
                    TimerKind::RestorePasteboard => {
                        self.events.borrow_mut().push("schedule_restore");
                        panic!("timer scheduling failed");
                    }
                    _ => panic!("unexpected timer"),
                }
            }

            fn finish_and_drain_hotkeys(&mut self, _result: Result<(), InsertError>) {
                panic!("paste flow must not finish after scheduling panic");
            }
        }

        let events = Rc::new(RefCell::new(Vec::new()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let events = Rc::clone(&events);
            move || {
                let insertion = RestoringInsertion {
                    events: Rc::clone(&events),
                    restored: false,
                };
                let mut boundary = PanickingBoundary {
                    events: Rc::clone(&events),
                };
                let mut flow = PasteFlow::begin(insertion, &mut boundary);
                flow.handle_timer(TimerKind::PasteCommand, &mut boundary);
            }
        }));

        assert!(result.is_err());
        assert_eq!(
            events.borrow().as_slice(),
            [
                "schedule_paste",
                "paste",
                "schedule_restore",
                "restore_on_drop",
            ]
        );
    }

    #[test]
    fn smoke_watchdog_returns_success() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();

        assert_eq!(
            wait_for_smoke_child(&mut child, std::time::Duration::from_secs(1)),
            0
        );
    }

    #[test]
    fn smoke_watchdog_returns_child_failure() {
        let mut child = std::process::Command::new("/usr/bin/false")
            .spawn()
            .unwrap();

        assert_eq!(
            wait_for_smoke_child(&mut child, std::time::Duration::from_secs(1)),
            1
        );
    }

    #[test]
    fn smoke_watchdog_terminates_a_hung_child() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .unwrap();

        assert_eq!(
            wait_for_smoke_child(&mut child, std::time::Duration::from_millis(10)),
            124
        );
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn status_logging_uses_only_category_names() {
        assert_eq!(status_name(&AppStatus::Ready), "ready");
        assert_eq!(
            status_name(&AppStatus::Error {
                message: "private detail",
                recoverable: true,
            }),
            "error"
        );
    }

    #[test]
    fn microphone_callback_rechecks_before_opening_settings_and_requests_repoll() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut boundary = RecordingMicrophoneBoundary {
            events: Rc::clone(&events),
        };
        let mut permissions = MicrophonePermissionRuntime::default();

        permissions.permission_needed(MicrophoneAuthorization::NotDetermined, &mut boundary);
        permissions.permission_needed(MicrophoneAuthorization::NotDetermined, &mut boundary);
        assert_eq!(*events.borrow(), vec!["request"]);

        permissions.completion_sender().send(()).unwrap();
        let should_repoll = permissions.drain_completions(
            || {
                events.borrow_mut().push("recheck");
                MicrophoneAuthorization::Denied
            },
            &mut boundary,
        );

        assert!(should_repoll);
        assert_eq!(*events.borrow(), vec!["request", "recheck", "open"]);
        assert!(!permissions
            .drain_completions(|| panic!("no second authorization recheck"), &mut boundary,));
    }

    #[test]
    fn observed_grant_allows_settings_to_open_after_future_revocation() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut boundary = RecordingMicrophoneBoundary {
            events: Rc::clone(&events),
        };
        let mut permissions = MicrophonePermissionRuntime::default();

        permissions.permission_needed(MicrophoneAuthorization::Denied, &mut boundary);
        permissions.permission_needed(MicrophoneAuthorization::Authorized, &mut boundary);
        permissions.permission_needed(MicrophoneAuthorization::Denied, &mut boundary);

        assert_eq!(*events.borrow(), vec!["open", "open"]);
    }

    #[test]
    fn tap_loss_and_restore_do_not_end_in_flight_recognition() {
        let mut controller = recognizing_controller();
        let mut tap = DeferredTapState::default();

        assert_eq!(tap.observe(controller.status(), TapState::Lost), None);
        assert_eq!(tap.observe(controller.status(), TapState::Restored), None);
        assert!(controller.handle(AppEvent::TriggerPressed).is_empty());
        assert_eq!(controller.status(), &AppStatus::Recognizing);

        assert_eq!(
            controller.handle(AppEvent::RecognitionFinished(Ok("старый результат".into()))),
            vec![Effect::InsertText("старый результат".into())]
        );
        controller.handle(AppEvent::PasteFinished(Ok(())));
        assert_eq!(
            tap.take_when_idle(controller.status()),
            Some(AppEvent::EventTapRestored)
        );
        controller.handle(AppEvent::EventTapRestored);

        assert_eq!(
            controller.handle(AppEvent::TriggerPressed),
            vec![Effect::StartCapture]
        );
    }

    #[test]
    fn unresolved_tap_loss_blocks_new_cycle_after_old_result_completes() {
        let mut controller = recognizing_controller();
        let mut tap = DeferredTapState::default();

        assert_eq!(tap.observe(controller.status(), TapState::Lost), None);
        assert!(controller.handle(AppEvent::TriggerPressed).is_empty());
        assert_eq!(
            controller.handle(AppEvent::RecognitionFinished(Ok("первый".into()))),
            vec![Effect::InsertText("первый".into())]
        );
        controller.handle(AppEvent::PasteFinished(Ok(())));

        let deferred = tap.take_when_idle(controller.status()).unwrap();
        controller.handle(deferred);
        assert!(matches!(
            controller.status(),
            AppStatus::Error {
                recoverable: true,
                ..
            }
        ));
        assert!(controller.handle(AppEvent::TriggerPressed).is_empty());

        let restored = tap
            .observe(controller.status(), TapState::Restored)
            .unwrap();
        controller.handle(restored);
        assert_eq!(
            controller.handle(AppEvent::TriggerPressed),
            vec![Effect::StartCapture]
        );
    }
}
