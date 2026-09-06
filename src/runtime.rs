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
use crate::asr_task::{AsrTask, AsrTaskError};
use crate::audio::{AudioError, AudioRecorder};
use crate::audio_task::AudioPreparationTask;
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
    PasteCommand(u64),
    RestorePasteboard(u64),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteFlowState {
    AwaitingPaste,
    AwaitingRestore,
    Finished,
}

#[derive(Debug, PartialEq, Eq)]
enum PasteOutcome {
    Delivered,
    PasteFailed(InsertError),
    Restored(Result<(), InsertError>),
}

impl PasteOutcome {
    fn foreground_event(&self, generation: u64, capture: &DictationCapture) -> Option<AppEvent> {
        match self {
            Self::Delivered if generation == capture.generation => {
                Some(AppEvent::PasteFinished(Ok(())))
            }
            Self::PasteFailed(_) if generation == capture.generation => {
                Some(AppEvent::PasteFinished(Err("insert failed".to_owned())))
            }
            Self::Restored(Err(_))
                if generation == capture.latest_started_generation
                    && capture.phase == CapturePhase::Inactive =>
            {
                Some(AppEvent::ClipboardRestoreFailed)
            }
            _ => None,
        }
    }
}

struct PasteFlow<I> {
    insertion: I,
    generation: u64,
    state: PasteFlowState,
}

impl<I: PasteInsertion> PasteFlow<I> {
    fn begin(generation: u64, insertion: I, boundary: &mut impl PasteFlowBoundary) -> Self {
        boundary.schedule(
            TimerKind::PasteCommand(generation),
            PASTEBOARD_SETTLE_DELAY_MS,
        );
        Self {
            insertion,
            generation,
            state: PasteFlowState::AwaitingPaste,
        }
    }

    fn expects_timer(&self, kind: TimerKind) -> bool {
        matches!((self.state, kind),
            (PasteFlowState::AwaitingPaste, TimerKind::PasteCommand(generation))
            | (PasteFlowState::AwaitingRestore, TimerKind::RestorePasteboard(generation))
            if generation == self.generation)
    }

    fn handle_timer(
        &mut self,
        kind: TimerKind,
        boundary: &mut impl PasteFlowBoundary,
    ) -> Option<PasteOutcome> {
        if !self.expects_timer(kind) {
            return None;
        }
        match self.state {
            PasteFlowState::AwaitingPaste => match self.insertion.paste() {
                Ok(()) => {
                    self.state = PasteFlowState::AwaitingRestore;
                    boundary.schedule(
                        TimerKind::RestorePasteboard(self.generation),
                        PASTEBOARD_RESTORE_DELAY_MS,
                    );
                    Some(PasteOutcome::Delivered)
                }
                Err(primary) => {
                    let error = self.insertion.restore_after_paste_failure(primary);
                    self.state = PasteFlowState::Finished;
                    Some(PasteOutcome::PasteFailed(error))
                }
            },
            PasteFlowState::AwaitingRestore => {
                let result = self.insertion.restore();
                self.state = PasteFlowState::Finished;
                Some(PasteOutcome::Restored(result))
            }
            PasteFlowState::Finished => None,
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

// Only recognized text waits here; no audio, native objects or hotkeys are queued.
struct QueuedInsertion {
    generation: u64,
    text: String,
    append_space: bool,
}

#[derive(Default)]
struct InsertionQueue(Option<QueuedInsertion>);

impl InsertionQueue {
    fn park(&mut self, insertion: QueuedInsertion) -> bool {
        if self.0.is_some() {
            return false;
        }
        self.0 = Some(insertion);
        true
    }

    fn discard_unless_current(&mut self, capture: &DictationCapture, status: &AppStatus) {
        if self.0.as_ref().is_some_and(|queued| {
            queued.generation != capture.generation
                || capture.phase != CapturePhase::Inserting
                || *status != AppStatus::Recognizing
        }) {
            self.0 = None;
        }
    }

    fn take_if_unblocked(&mut self, paste_pending: bool) -> Option<QueuedInsertion> {
        if paste_pending {
            None
        } else {
            self.0.take()
        }
    }
}

fn capture_start_allowed(
    status: &AppStatus,
    paste_state: Option<PasteFlowState>,
    queued: bool,
) -> bool {
    *status == AppStatus::Ready && paste_state != Some(PasteFlowState::AwaitingPaste) && !queued
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

#[derive(Default, PartialEq, Eq)]
enum CapturePhase {
    #[default]
    Inactive,
    Recording,
    Preparing,
    Submitted,
    Inserting,
}

/// Dictation identity survives controller status changes. Recognizing alone is
/// insufficient: a later dictation can have the same status as a cancelled one.
#[derive(Default)]
struct DictationCapture {
    generation: u64,
    latest_started_generation: u64,
    phase: CapturePhase,
}

impl DictationCapture {
    fn begin(&mut self) {
        self.abandon();
        self.latest_started_generation = self.generation;
        self.phase = CapturePhase::Recording;
    }

    fn abandon(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.phase = CapturePhase::Inactive;
    }

    fn abandon_unless_active(&mut self, status: &AppStatus) -> bool {
        if self.phase != CapturePhase::Inactive
            && !matches!(status, AppStatus::Recording | AppStatus::Recognizing)
        {
            self.abandon();
            true
        } else {
            false
        }
    }

    fn accept_recognition(&mut self, generation: Option<u64>) -> bool {
        if generation != Some(self.generation) || self.phase != CapturePhase::Submitted {
            return false;
        }
        self.phase = CapturePhase::Inserting;
        true
    }

    fn expect_preparation(&mut self) -> Option<u64> {
        if self.phase != CapturePhase::Recording {
            return None;
        }
        self.phase = CapturePhase::Preparing;
        Some(self.generation)
    }

    fn accept(&mut self, generation: u64, status: &AppStatus) -> bool {
        if self.phase != CapturePhase::Preparing
            || self.generation != generation
            || *status != AppStatus::Recognizing
        {
            return false;
        }
        self.phase = CapturePhase::Submitted;
        true
    }
}

#[derive(Default)]
struct AsrRecovery {
    paths: Option<ModelPaths>,
    attempted: bool,
    unavailable: bool,
}

impl AsrRecovery {
    fn failure(&mut self) -> Option<ModelPaths> {
        if self.attempted || self.unavailable {
            self.unavailable = true;
            return None;
        }
        self.attempted = true;
        let paths = self.paths.clone();
        self.unavailable = paths.is_none();
        paths
    }

    fn retry(&mut self) -> Option<ModelPaths> {
        if !self.unavailable {
            return None;
        }
        let paths = self.paths.clone()?;
        self.unavailable = false;
        self.attempted = true;
        Some(paths)
    }

    fn loaded(&mut self) {
        self.attempted = false;
        self.unavailable = false;
    }
}

const ASR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct AsrShutdown {
    started: Option<Instant>,
    failure_reported: bool,
}
impl AsrShutdown {
    fn begin(&mut self, now: Instant) -> bool {
        if self.started.is_some() {
            return false;
        }
        self.started = Some(now);
        true
    }
    fn report_failure(&mut self, now: Instant, cleaned: bool, cleanup_failed: bool) -> bool {
        if cleaned || self.failure_reported {
            return false;
        }
        if cleanup_failed
            || self
                .started
                .is_some_and(|start| now.saturating_duration_since(start) >= ASR_SHUTDOWN_TIMEOUT)
        {
            self.failure_reported = true;
            return true;
        }
        false
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
    audio_preparation: AudioPreparationTask,
    dictation_capture: DictationCapture,
    hotkey: Option<HotkeyListener>,
    hotkey_sender: Sender<HotkeySignal>,
    hotkey_events: Receiver<HotkeySignal>,
    asr: AsrTask,
    asr_recovery: AsrRecovery,
    asr_generation: Option<u64>,
    asr_shutdown: AsrShutdown,
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
    insertion_queue: InsertionQueue,
    applied_permissions: PermissionSnapshot,
    microphone_permissions: MicrophonePermissionRuntime,
    tap_needs_retry: bool,
    deferred_tap_state: DeferredTapState,
    _pin: PhantomPinned,
}

/// Completes shutdown after the application's outer run loop has returned.
/// Borrow only the pinned allocation's owner handle across callback pumping:
/// no `&mut Runtime` (including one nested in `Pin`) may span a nested run loop
/// that can invoke timers through their previously stored Runtime pointers.
pub fn finish_after_run(owner: &mut Pin<Box<Runtime>>) {
    pump_until_cleanup(owner, Runtime::shutdown_after_run_step, || {
        CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::from_millis(20),
            true,
        );
    });
}

/// Every pointee borrow is created for one step and ends when that call returns.
/// The higher-ranked step returns only a boolean, so it cannot retain its
/// temporary `Pin<&mut T>` for use while the following pump invokes callbacks.
/// The outer function retains only a reference to the stable owner handle.
fn pump_until_cleanup<T>(
    owner: &mut Pin<Box<T>>,
    mut step: impl for<'access> FnMut(Pin<&'access mut T>) -> bool,
    mut pump: impl FnMut(),
) {
    loop {
        let complete = step(owner.as_mut());
        if complete {
            return;
        }
        pump();
    }
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
        let asr = AsrTask::spawn();
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
            audio_preparation: AudioPreparationTask::spawn(),
            dictation_capture: DictationCapture::default(),
            hotkey: None,
            hotkey_sender,
            hotkey_events,
            asr,
            asr_recovery: AsrRecovery::default(),
            asr_generation: None,
            asr_shutdown: AsrShutdown::default(),
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
            insertion_queue: InsertionQueue::default(),
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
            TimerKind::PasteCommand(_) | TimerKind::RestorePasteboard(_) => {
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
        if self.asr_shutdown.started.is_some() {
            self.try_orderly_quit();
            return;
        }
        self.drain_updater_worker_results();
        self.drain_updater_actions();
        self.drain_model_preparation();
        self.drain_permission_migration();
        self.drain_microphone_permission_completions();

        self.drain_menu_commands();

        self.drain_hotkey_events();

        self.drain_audio_preparation();

        while let Some(result) = self.asr.poll(Instant::now()) {
            match result {
                Ok(AsrEvent::Loaded(Ok(()))) => {
                    self.asr_recovery.loaded();
                    self.poll_permissions();
                    self.dispatch(AppEvent::ModelLoaded(Ok(())));
                }
                Ok(AsrEvent::Loaded(Err(_))) => self.handle_asr_error(AsrTaskError::WorkerFailed),
                Ok(AsrEvent::Recognized(result)) => {
                    if self
                        .dictation_capture
                        .accept_recognition(self.asr_generation.take())
                    {
                        self.dispatch(AppEvent::RecognitionFinished(result));
                    }
                }
                Err(error) => {
                    self.handle_asr_error(error);
                    break;
                }
            }
        }
        if self.controller.status() == &AppStatus::AsrCleanupPending && self.asr.retry_ready() {
            self.dispatch(AppEvent::AsrUnavailable);
        }
        self.try_orderly_quit();
    }

    fn handle_asr_error(&mut self, error: AsrTaskError) {
        tracing::error!(error_category = "asr_worker", error = ?error);
        self.asr.invalidate();
        self.asr_generation = None;
        cancel_timer(&self.run_loop, &mut self.error_timer);
        self.dispatch(AppEvent::AsrUnavailable);
        if let Some(paths) = self.asr_recovery.failure() {
            self.reload_asr(paths);
        } else if !self.asr.retry_ready() {
            self.dispatch(AppEvent::AsrCleanupPending);
        }
    }

    fn reload_asr(&mut self, paths: ModelPaths) {
        if self.asr_shutdown.started.is_some() {
            return;
        }
        self.dispatch(AppEvent::AsrRecoveryStarted);
        if let Err(error) = self.asr.send(AsrCommand::Load(paths), Instant::now()) {
            tracing::error!(error_category = "asr_reload", error = ?error);
            self.asr_recovery.unavailable = true;
            self.dispatch(if self.asr.retry_ready() {
                AppEvent::AsrUnavailable
            } else {
                AppEvent::AsrCleanupPending
            });
        }
    }

    fn retry_asr(&mut self) {
        if self.controller.status() != &AppStatus::AsrUnavailable {
            return;
        }
        if self.asr.prepare_explicit_retry() {
            if let Some(paths) = self.asr_recovery.retry() {
                self.reload_asr(paths);
            }
        } else {
            self.dispatch(AppEvent::AsrCleanupPending);
        }
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
                self.asr_recovery.paths = Some(result.paths.clone());
                if let Err(error) = self
                    .asr
                    .send(AsrCommand::Load(result.paths), Instant::now())
                {
                    self.handle_asr_error(error);
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
            if self.asr_shutdown.started.is_some() {
                continue;
            }
            match action {
                MenuAction::Quit => self.begin_shutdown(),
                MenuAction::RetryAsr => self.retry_asr(),
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
                    self.drain_hotkey_events();
                    if updater_open_allowed(self.controller.status(), self.clipboard_busy()) {
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

    fn clipboard_busy(&self) -> bool {
        self.pending_insertion.is_some() || self.insertion_queue.0.is_some()
    }

    fn render_updater_menu(&self) {
        let open_enabled = updater_open_allowed(self.controller.status(), self.clipboard_busy());
        self.menu.render_updater(
            self.updater.as_ref().map(SystemUpdaterLane::state),
            open_enabled,
        );
    }

    fn begin_shutdown(&mut self) {
        if !self.asr_shutdown.begin(Instant::now()) {
            return;
        }
        self.menu_readiness.set_ready(false);
        self.dictation_capture.abandon();
        self.asr_generation = None;
        self.insertion_queue.0 = None;
        self.audio_preparation.stop();
        self.recorder.abort();
        self.hotkey.take();
        self.cancel_finish_timer();
        self.cancel_capture_limit_timer();
        cancel_timer(&self.run_loop, &mut self.permission_timer);
        cancel_timer(&self.run_loop, &mut self.updater_timer);
        cancel_timer(&self.run_loop, &mut self.error_timer);
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        if let Some(mut flow) = self.pending_insertion.take() {
            if flow.restore_on_shutdown().is_err() {
                tracing::warn!(error_category = "pasteboard_restore_on_shutdown");
            }
        }
        self.asr.stop();
    }

    fn try_orderly_quit(&mut self) {
        if self.asr_shutdown.started.is_none()
            && self.orderly_quit.take_if_ready(
                self.controller.status(),
                self.pending_insertion.is_some() || self.insertion_queue.0.is_some(),
            )
        {
            self.begin_shutdown();
        }
        if self.asr_shutdown.started.is_none() {
            return;
        }
        let cleaned = self.asr.cleanup_complete();
        if self
            .asr_shutdown
            .report_failure(Instant::now(), cleaned, self.asr.cleanup_failed())
        {
            tracing::error!(error_category = "asr_shutdown_cleanup_pending");
        }
        if cleaned {
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let application = NSApplication::sharedApplication(mtm);
            unsafe { application.terminate(None) };
        }
    }

    /// One scoped pointee access for the owner-handle shutdown pump. This
    /// function never pumps callbacks, and its mutable argument ends on return.
    fn shutdown_after_run_step(self: Pin<&mut Self>) -> bool {
        let runtime = unsafe { self.get_unchecked_mut() };
        runtime.begin_shutdown();
        let cleaned = runtime.asr.cleanup_complete();
        if runtime.asr_shutdown.report_failure(
            Instant::now(),
            cleaned,
            runtime.asr.cleanup_failed(),
        ) {
            tracing::error!(error_category = "asr_shutdown_cleanup_pending");
        }
        cleaned
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
                if capture_start_allowed(
                    self.controller.status(),
                    self.pending_insertion.as_ref().map(|flow| flow.state),
                    self.insertion_queue.0.is_some(),
                ) {
                    self.dispatch(AppEvent::TriggerPressed);
                }
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
        if self
            .dictation_capture
            .abandon_unless_active(self.controller.status())
        {
            self.audio_preparation.cancel();
            self.cancel_finish_timer();
            self.cancel_capture_limit_timer();
            self.recorder.abort();
        }
        self.insertion_queue
            .discard_unless_current(&self.dictation_capture, self.controller.status());
        self.menu_readiness.set_ready(capture_start_allowed(
            self.controller.status(),
            self.pending_insertion.as_ref().map(|flow| flow.state),
            self.insertion_queue.0.is_some(),
        ));
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
                self.audio_preparation.cancel();
                self.dictation_capture.begin();
                match capture_start_result_event(self.recorder.start(), &self.hotkey_control) {
                    Ok(()) => {
                        crate::browser_accessibility::prepare_focused_browser();
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
                self.dictation_capture.abandon();
                self.audio_preparation.cancel();
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
                self.asr_generation = Some(self.dictation_capture.generation);
                tracing::debug!(
                    sample_count = samples.len(),
                    lifecycle = "recognition_started"
                );
                if let Err(error) = self
                    .asr
                    .send(AsrCommand::Transcribe(samples), Instant::now())
                {
                    self.handle_asr_error(error);
                }
            }
            Effect::InsertText(text) => {
                let queued = QueuedInsertion {
                    generation: self.dictation_capture.generation,
                    text,
                    append_space: self.output_preferences.current().append_space,
                };
                if !self.insertion_queue.park(queued) {
                    tracing::error!(error_category = "insertion_queue_occupied");
                }
                self.drain_insertion_queue();
            }
            Effect::ScheduleErrorReset { delay_ms } => {
                self.replace_error_timer(delay_ms);
            }
        }
    }

    fn finish_capture(&mut self) {
        self.cancel_finish_timer();
        if self.controller.status() != &AppStatus::Recognizing {
            return;
        }
        let Some(generation) = self.dictation_capture.expect_preparation() else {
            return;
        };
        match self.recorder.finish() {
            Ok(capture) => {
                if let Err(error) = self.audio_preparation.submit(generation, capture) {
                    tracing::warn!(error_category = "audio_preparation_submit", error = ?error);
                    self.dispatch(AppEvent::CaptureFailed);
                }
            }
            Err(_) => {
                tracing::warn!(error_category = "microphone_stop");
                self.dispatch(AppEvent::CaptureFailed);
            }
        }
    }

    fn drain_audio_preparation(&mut self) {
        while let Some(prepared) = self.audio_preparation.poll() {
            if !self
                .dictation_capture
                .accept(prepared.generation, self.controller.status())
            {
                continue;
            }
            tracing::debug!(
                native_sample_count = prepared.native_sample_count,
                sample_count = prepared
                    .result
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .map_or(0, Vec::len),
                elapsed_ms = prepared.elapsed.as_millis() as u64,
                lifecycle = "capture_prepared"
            );
            if let Err(error) = &prepared.result {
                tracing::warn!(error_category = "audio_preparation", error = ?error);
            }
            self.dispatch(capture_result_event(prepared.result));
        }
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

    fn drain_insertion_queue(&mut self) {
        self.insertion_queue
            .discard_unless_current(&self.dictation_capture, self.controller.status());
        if self.asr_shutdown.started.is_some() {
            return;
        }
        let Some(queued) = self
            .insertion_queue
            .take_if_unblocked(self.pending_insertion.is_some())
        else {
            return;
        };
        match text_inserter::begin(&queued.text, queued.append_space) {
            Ok(insertion) => {
                let flow = PasteFlow::begin(queued.generation, insertion, self);
                self.pending_insertion = Some(flow);
                self.render_updater_menu();
            }
            Err(error) => {
                self.handle_paste_outcome(queued.generation, PasteOutcome::PasteFailed(error))
            }
        }
    }

    fn advance_pending_paste(&mut self, kind: TimerKind) {
        let Some(mut flow) = self.pending_insertion.take() else {
            return;
        };
        if !flow.expects_timer(kind) {
            self.pending_insertion = Some(flow);
            return;
        }
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        let generation = flow.generation;
        let outcome = flow.handle_timer(kind, self);
        if flow.is_finished() {
            // A failed restore can retry in Drop. Finish that ownership scope
            // before taking the next snapshot or mutating the clipboard again.
            drop(flow);
        } else {
            self.pending_insertion = Some(flow);
        }
        if let Some(outcome) = outcome {
            self.handle_paste_outcome(generation, outcome);
        }
        self.drain_insertion_queue();
        self.render_updater_menu();
        self.try_orderly_quit();
    }

    fn handle_paste_outcome(&mut self, generation: u64, outcome: PasteOutcome) {
        match &outcome {
            PasteOutcome::Delivered => {
                tracing::debug!(
                    method = "clipboard",
                    lifecycle = "text_inserted",
                    generation
                );
                // Consume the busy prefix before Ready. Never replay a press
                // made before Command-V, even when the drain timer has not run.
                self.drain_hotkey_events();
            }
            PasteOutcome::PasteFailed(error) | PasteOutcome::Restored(Err(error)) => {
                tracing::warn!(
                    error_category = "dictation_clipboard",
                    generation,
                    stage = if matches!(outcome, PasteOutcome::PasteFailed(_)) {
                        "paste"
                    } else {
                        "restore"
                    }
                );
                log_text_insertion_error(*error);
                if matches!(outcome, PasteOutcome::PasteFailed(_)) {
                    self.drain_hotkey_events();
                }
            }
            PasteOutcome::Restored(Ok(())) => {}
        }
        if let Some(event) = outcome.foreground_event(generation, &self.dictation_capture) {
            self.dispatch(event);
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

pub(crate) fn capture_result_event<E>(result: Result<Option<Vec<f32>>, E>) -> AppEvent {
    match result {
        Ok(samples) => AppEvent::AudioReady(samples),
        Err(_) => AppEvent::CaptureFailed,
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.dictation_capture.abandon();
        self.audio_preparation.stop();
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
        self.asr.stop();
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
        AppStatus::AsrUnavailable => "asr_unavailable",
        AppStatus::AsrCleanupPending => "asr_cleanup_pending",
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

    #[test]
    fn asr_recovery_has_one_reload_and_explicit_retry_without_migration() {
        let paths = crate::model::ModelPaths::from_verified_directory(std::path::Path::new(
            "/tmp/synthetic-model",
        ));
        let mut recovery = super::AsrRecovery {
            paths: Some(paths),
            ..Default::default()
        };
        assert!(recovery.failure().is_some());
        assert!(recovery.failure().is_none());
        assert!(recovery.failure().is_none());
        assert!(recovery.unavailable);
        assert!(recovery.retry().is_some());
        assert!(recovery.retry().is_none());
        recovery.loaded();
        assert!(
            recovery.failure().is_some(),
            "new episode gets one recovery after success"
        );
        assert!(recovery.failure().is_none());
        // Recovery owns only retained paths/attempt state; it cannot call model
        // preparation or permission migration and never stores PCM for replay.
    }

    #[test]
    fn post_run_shutdown_api_borrows_only_the_pinned_owner_handle() {
        // Compile-time API regression: Pin<&mut Runtime> is deliberately not
        // an accepted argument to the function that pumps reentrant callbacks.
        let _: fn(&mut std::pin::Pin<Box<super::Runtime>>) = super::finish_after_run;
    }

    #[test]
    fn post_run_shutdown_pump_runs_callback_before_cleanup_acknowledgment() {
        use core_foundation::date::CFDate;
        use core_foundation::runloop::{
            kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopTimer, CFRunLoopTimerContext,
            CFRunLoopTimerRef,
        };
        use std::ffi::c_void;
        use std::marker::PhantomPinned;
        use std::pin::Pin;
        use std::time::Instant;

        #[derive(Default)]
        struct Probe {
            cleanup_acknowledged: bool,
            callback_before_ack: bool,
            callback_during_step: bool,
            in_step: bool,
            callback_count: usize,
            step_count: usize,
            _pin: PhantomPinned,
        }

        extern "C" fn callback(_timer: CFRunLoopTimerRef, context: *mut c_void) {
            // Same lifetime shape as TimerContext.runtime: this pointer was
            // obtained before the owner-handle pump, and the pinned allocation
            // outlives its timer. No probe reference is held by that pump.
            let probe = unsafe { &mut *context.cast::<Probe>() };
            probe.callback_before_ack = !probe.cleanup_acknowledged;
            probe.callback_during_step = probe.in_step;
            probe.callback_count += 1;
            probe.cleanup_acknowledged = true;
        }

        let mut owner = Box::pin(Probe::default());
        let pointer = unsafe { owner.as_mut().get_unchecked_mut() } as *mut Probe;
        let run_loop = CFRunLoop::get_current();
        let mode = unsafe { kCFRunLoopDefaultMode };
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: pointer.cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer =
            CFRunLoopTimer::new(CFDate::now().abs_time(), 0.0, 0, 0, callback, &mut context);
        run_loop.add_timer(&timer, mode);
        let watchdog = Instant::now() + Duration::from_secs(1);
        let mut pump_count = 0;
        super::pump_until_cleanup(
            &mut owner,
            |probe: Pin<&mut Probe>| {
                let probe = unsafe { probe.get_unchecked_mut() };
                probe.in_step = true;
                probe.step_count += 1;
                let complete = probe.cleanup_acknowledged;
                probe.in_step = false;
                complete
            },
            || {
                assert!(Instant::now() < watchdog, "cleanup callback watchdog");
                pump_count += 1;
                CFRunLoop::run_in_mode(mode, Duration::from_millis(20), true);
            },
        );
        run_loop.remove_timer(&timer, mode);

        let probe = owner.as_ref().get_ref();
        assert!(pump_count >= 1);
        assert_eq!(probe.callback_count, 1);
        assert!(probe.callback_before_ack);
        assert!(!probe.callback_during_step);
        assert!(probe.cleanup_acknowledged);
        assert!(
            probe.step_count >= 2,
            "cleanup must be checked after callback"
        );
    }

    #[test]
    fn asynchronous_shutdown_reports_deadline_once_and_keeps_waiting_for_ack() {
        let now = std::time::Instant::now();
        let mut shutdown = super::AsrShutdown::default();
        assert!(shutdown.begin(now));
        assert!(!shutdown.begin(now));
        assert!(!shutdown.report_failure(now, false, false));
        assert!(shutdown.report_failure(now + super::ASR_SHUTDOWN_TIMEOUT, false, false));
        assert!(!shutdown.report_failure(now + super::ASR_SHUTDOWN_TIMEOUT, false, false));
        assert!(
            shutdown.started.is_some(),
            "timeout must retain shutdown ownership"
        );
        assert!(!shutdown.report_failure(now, true, true));
    }

    #[test]
    fn recovery_transition_invalidates_preparation_before_new_recognizing_state() {
        let mut capture = super::DictationCapture::default();
        let mut controller = AppController::new();
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        controller.handle(AppEvent::TriggerPressed);
        capture.begin();
        controller.handle(AppEvent::TriggerReleased { short: false });
        let old = capture.expect_preparation().unwrap();
        controller.handle(AppEvent::AsrRecoveryStarted);
        assert!(capture.abandon_unless_active(controller.status()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        controller.handle(AppEvent::TriggerPressed);
        capture.begin();
        controller.handle(AppEvent::TriggerReleased { short: false });
        let current = capture.expect_preparation().unwrap();
        assert!(!capture.accept(old, controller.status()));
        assert!(capture.accept(current, controller.status()));
        assert!(
            !capture.accept(current, controller.status()),
            "duplicate audio accepted"
        );
    }

    #[test]
    fn preparation_failures_are_recoverable_and_never_recognize_or_insert() {
        use crate::audio_task::AudioPreparationError;
        for error in [
            AudioPreparationError::Busy,
            AudioPreparationError::Panicked,
            AudioPreparationError::Disconnected,
        ] {
            let mut controller = AppController::new();
            controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
            controller.handle(AppEvent::ModelLoaded(Ok(())));
            controller.handle(AppEvent::TriggerPressed);
            controller.handle(AppEvent::TriggerReleased { short: false });
            let effects = controller.handle(super::capture_result_event(Err(error)));
            assert!(effects
                .iter()
                .all(|effect| !matches!(effect, Effect::Recognize(_) | Effect::InsertText(_))));
            assert!(matches!(
                controller.status(),
                AppStatus::Error {
                    recoverable: true,
                    ..
                }
            ));
            controller.handle(AppEvent::ErrorTimerFired);
            assert_eq!(controller.status(), &AppStatus::Ready);
        }
    }

    #[test]
    fn aborted_capture_and_shutdown_reject_old_audio() {
        let mut capture = super::DictationCapture::default();
        capture.begin();
        let old = capture.expect_preparation().unwrap();
        capture.abandon();
        assert!(!capture.accept(old, &AppStatus::Recognizing));
        assert_eq!(capture.expect_preparation(), None);
        capture.begin();
        let next = capture.expect_preparation().unwrap();
        assert_ne!(old, next);
        assert!(!capture.accept(next, &AppStatus::Ready));
        capture.abandon();
        assert!(!capture.accept(next, &AppStatus::Recognizing));
    }

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

    struct TestPasteInsertion {
        events: Rc<RefCell<Vec<&'static str>>>,
        paste_error: Option<InsertError>,
        restore_error: Option<InsertError>,
    }

    impl PasteInsertion for TestPasteInsertion {
        fn paste(&mut self) -> Result<(), InsertError> {
            self.events.borrow_mut().push("paste");
            self.paste_error.map_or(Ok(()), Err)
        }
        fn restore(&mut self) -> Result<(), InsertError> {
            self.events.borrow_mut().push("restore");
            self.restore_error.map_or(Ok(()), Err)
        }
        fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
            self.events.borrow_mut().push("failure_restore");
            primary
        }
    }

    #[derive(Default)]
    struct TestPasteTimers {
        now_ms: u64,
        scheduled: Vec<(u64, TimerKind)>,
    }

    impl PasteFlowBoundary for TestPasteTimers {
        fn schedule(&mut self, kind: TimerKind, delay_ms: u64) {
            self.scheduled.push((self.now_ms + delay_ms, kind));
        }
    }

    fn synthetic_insertion() -> TestPasteInsertion {
        TestPasteInsertion {
            events: Rc::new(RefCell::new(Vec::new())),
            paste_error: None,
            restore_error: None,
        }
    }

    fn apply_paste_outcome(
        controller: &mut AppController,
        capture: &mut super::DictationCapture,
        generation: u64,
        outcome: super::PasteOutcome,
    ) {
        if let Some(event) = outcome.foreground_event(generation, capture) {
            controller.handle(event);
            capture.abandon_unless_active(controller.status());
        }
    }

    fn inserting_capture() -> super::DictationCapture {
        let mut capture = super::DictationCapture::default();
        capture.begin();
        let generation = capture.expect_preparation().unwrap();
        assert!(capture.accept(generation, &AppStatus::Recognizing));
        assert!(capture.accept_recognition(Some(generation)));
        capture
    }

    #[test]
    fn paste_delivery_opens_next_capture_before_restore() {
        let mut controller = recognizing_controller();
        let mut capture = inserting_capture();
        let generation = capture.generation;
        let mut timers = TestPasteTimers::default();
        let mut flow = PasteFlow::begin(generation, synthetic_insertion(), &mut timers);
        assert!(!super::capture_start_allowed(
            controller.status(),
            Some(flow.state),
            false
        ));
        timers.now_ms = 30;
        let outcome = flow
            .handle_timer(TimerKind::PasteCommand(generation), &mut timers)
            .unwrap();
        apply_paste_outcome(&mut controller, &mut capture, generation, outcome);
        assert!(!flow.is_finished(), "clipboard owner must survive delivery");
        assert!(super::capture_start_allowed(
            controller.status(),
            Some(flow.state),
            false
        ));
        assert_eq!(
            controller.handle(AppEvent::TriggerPressed),
            vec![Effect::StartCapture]
        );
        assert_eq!(
            timers.scheduled,
            vec![
                (30, TimerKind::PasteCommand(1)),
                (1030, TimerKind::RestorePasteboard(1))
            ]
        );
    }

    #[test]
    fn paste_flow_orders_command_restore_and_rejects_stale_timers() {
        let insertion = synthetic_insertion();
        let events = Rc::clone(&insertion.events);
        let mut timers = TestPasteTimers::default();
        let mut flow = PasteFlow::begin(7, insertion, &mut timers);
        assert_eq!(
            flow.handle_timer(TimerKind::RestorePasteboard(7), &mut timers),
            None
        );
        assert_eq!(
            flow.handle_timer(TimerKind::PasteCommand(6), &mut timers),
            None
        );
        assert_eq!(
            flow.handle_timer(TimerKind::PasteCommand(7), &mut timers),
            Some(super::PasteOutcome::Delivered)
        );
        assert_eq!(
            flow.handle_timer(TimerKind::PasteCommand(7), &mut timers),
            None
        );
        assert_eq!(
            flow.handle_timer(TimerKind::RestorePasteboard(6), &mut timers),
            None
        );
        assert_eq!(events.borrow().as_slice(), ["paste"]);
        assert_eq!(
            flow.handle_timer(TimerKind::RestorePasteboard(7), &mut timers),
            Some(super::PasteOutcome::Restored(Ok(())))
        );
        assert_eq!(
            flow.handle_timer(TimerKind::RestorePasteboard(7), &mut timers),
            None
        );
        assert_eq!(events.borrow().as_slice(), ["paste", "restore"]);
    }

    #[test]
    fn paste_flow_preserves_insert_error_for_runtime_diagnostics() {
        let mut insertion = synthetic_insertion();
        insertion.paste_error = Some(InsertError::KeyboardEvent);
        let events = Rc::clone(&insertion.events);
        let mut timers = TestPasteTimers::default();
        let mut flow = PasteFlow::begin(1, insertion, &mut timers);
        let outcome = flow
            .handle_timer(TimerKind::PasteCommand(1), &mut timers)
            .unwrap();
        assert_eq!(
            outcome,
            super::PasteOutcome::PasteFailed(InsertError::KeyboardEvent)
        );
        assert!(flow.is_finished());
        assert_eq!(events.borrow().as_slice(), ["paste", "failure_restore"]);
        assert_eq!(timers.scheduled, [(30, TimerKind::PasteCommand(1))]);
        let mut controller = recognizing_controller();
        let mut capture = inserting_capture();
        apply_paste_outcome(&mut controller, &mut capture, 1, outcome);
        assert!(matches!(
            controller.status(),
            AppStatus::Error {
                recoverable: true,
                ..
            }
        ));
    }

    #[test]
    fn overlapping_phrases_at_exact_gaps_park_one_result_until_restore() {
        for gap_ms in [0, 100, 500, 1_000] {
            let mut controller = recognizing_controller();
            let mut capture = inserting_capture();
            let first = capture.generation;
            let mut timers = TestPasteTimers::default();
            let mut flow = PasteFlow::begin(first, synthetic_insertion(), &mut timers);
            timers.now_ms = 30;
            let delivered = flow
                .handle_timer(TimerKind::PasteCommand(first), &mut timers)
                .unwrap();
            apply_paste_outcome(&mut controller, &mut capture, first, delivered);
            if gap_ms == 1_000 {
                timers.now_ms = 1_030;
                let restored = flow
                    .handle_timer(TimerKind::RestorePasteboard(first), &mut timers)
                    .unwrap();
                apply_paste_outcome(&mut controller, &mut capture, first, restored);
            }
            timers.now_ms = 30 + gap_ms;
            assert!(super::capture_start_allowed(
                controller.status(),
                Some(flow.state),
                false
            ));
            assert_eq!(
                controller.handle(AppEvent::TriggerPressed),
                vec![Effect::StartCapture]
            );
            capture.begin();
            let second = capture.generation;
            assert_eq!(
                controller.handle(AppEvent::TriggerReleased { short: false }),
                vec![Effect::FinishCaptureAfter { delay_ms: 180 }]
            );
            timers.now_ms += 180;
            assert_eq!(capture.expect_preparation(), Some(second));
            assert!(capture.accept(second, controller.status()));
            assert!(
                !capture.accept_recognition(Some(first)),
                "old ASR cannot fill the slot"
            );
            assert!(capture.accept_recognition(Some(second)));
            assert!(
                !capture.accept_recognition(Some(second)),
                "duplicate ASR cannot overwrite text"
            );
            assert_eq!(
                controller.handle(AppEvent::RecognitionFinished(Ok("synthetic second".into()))),
                vec![Effect::InsertText("synthetic second".into())]
            );
            let mut queue = super::InsertionQueue::default();
            assert!(queue.park(super::QueuedInsertion {
                generation: second,
                text: "synthetic second".into(),
                append_space: true
            }));
            assert!(!queue.park(super::QueuedInsertion {
                generation: second,
                text: "synthetic duplicate".into(),
                append_space: false
            }));
            assert!(controller.handle(AppEvent::TriggerPressed).is_empty());
            assert!(!super::capture_start_allowed(
                controller.status(),
                Some(flow.state),
                true
            ));
            if gap_ms < 1_000 {
                assert!(
                    queue.take_if_unblocked(true).is_none(),
                    "cannot begin another clipboard transaction"
                );
                timers.now_ms = 1_030;
                let restored = flow
                    .handle_timer(TimerKind::RestorePasteboard(first), &mut timers)
                    .unwrap();
                apply_paste_outcome(&mut controller, &mut capture, first, restored);
                assert_eq!(controller.status(), &AppStatus::Recognizing);
                assert_eq!(capture.generation, second);
            }
            assert!(flow.is_finished());
            drop(flow);
            queue.discard_unless_current(&capture, controller.status());
            let waiting = queue.take_if_unblocked(false).unwrap();
            assert_eq!(waiting.text, "synthetic second");
            assert_eq!(waiting.generation, second);
            assert!(waiting.append_space);
            assert!(queue.take_if_unblocked(false).is_none());
            let mut next = PasteFlow::begin(waiting.generation, synthetic_insertion(), &mut timers);
            assert_eq!(
                next.handle_timer(TimerKind::RestorePasteboard(first), &mut timers),
                None
            );
            let delivered = next
                .handle_timer(TimerKind::PasteCommand(second), &mut timers)
                .unwrap();
            apply_paste_outcome(&mut controller, &mut capture, second, delivered);
            assert_eq!(controller.status(), &AppStatus::Ready);
        }
    }

    #[test]
    fn old_restore_failure_preserves_new_recording_preparation_asr_and_queued_text() {
        for phase in [
            super::CapturePhase::Recording,
            super::CapturePhase::Preparing,
            super::CapturePhase::Submitted,
            super::CapturePhase::Inserting,
        ] {
            let mut controller = recognizing_controller();
            let mut capture = inserting_capture();
            let old = capture.generation;
            apply_paste_outcome(
                &mut controller,
                &mut capture,
                old,
                super::PasteOutcome::Delivered,
            );
            controller.handle(AppEvent::TriggerPressed);
            capture.begin();
            let generation = capture.generation;
            if phase != super::CapturePhase::Recording {
                controller.handle(AppEvent::TriggerReleased { short: false });
                capture.expect_preparation();
            }
            if matches!(
                phase,
                super::CapturePhase::Submitted | super::CapturePhase::Inserting
            ) {
                assert!(capture.accept(generation, controller.status()));
            }
            if phase == super::CapturePhase::Inserting {
                assert!(capture.accept_recognition(Some(generation)));
            }
            let before = controller.status().clone();
            let mut queue = super::InsertionQueue::default();
            if phase == super::CapturePhase::Inserting {
                assert!(queue.park(super::QueuedInsertion {
                    generation,
                    text: "synthetic waiting".into(),
                    append_space: false
                }));
            }
            apply_paste_outcome(
                &mut controller,
                &mut capture,
                old,
                super::PasteOutcome::Restored(Err(InsertError::PasteboardRestore)),
            );
            assert_eq!(controller.status(), &before);
            assert_eq!(capture.generation, generation);
            assert!(capture.phase == phase);
            queue.discard_unless_current(&capture, controller.status());
            if phase == super::CapturePhase::Inserting {
                assert_eq!(
                    queue.take_if_unblocked(false).unwrap().text,
                    "synthetic waiting"
                );
            }
            if phase == super::CapturePhase::Submitted {
                assert!(capture.accept_recognition(Some(generation)));
            }
        }
    }

    #[test]
    fn old_restore_cannot_replace_new_empty_failed_or_cancelled_cycle() {
        for event in [
            AppEvent::RecognitionFinished(Ok(String::new())),
            AppEvent::RecognitionFinished(Err("synthetic failure".into())),
            AppEvent::AudioReady(None),
            AppEvent::CaptureFailed,
            AppEvent::AsrRecoveryStarted,
            AppEvent::AsrUnavailable,
            AppEvent::TriggerReleased { short: true },
        ] {
            let mut controller = recognizing_controller();
            let mut capture = inserting_capture();
            let old = capture.generation;
            apply_paste_outcome(
                &mut controller,
                &mut capture,
                old,
                super::PasteOutcome::Delivered,
            );
            controller.handle(AppEvent::TriggerPressed);
            capture.begin();
            if !matches!(event, AppEvent::TriggerReleased { short: true }) {
                controller.handle(AppEvent::TriggerReleased { short: false });
            }
            controller.handle(event);
            capture.abandon_unless_active(controller.status());
            let before = controller.status().clone();
            let generation = capture.generation;
            for result in [Ok(()), Err(InsertError::PasteboardRestore)] {
                apply_paste_outcome(
                    &mut controller,
                    &mut capture,
                    old,
                    super::PasteOutcome::Restored(result),
                );
                assert_eq!(controller.status(), &before);
                assert_eq!(capture.generation, generation);
            }
            let mut queue = super::InsertionQueue::default();
            assert!(
                queue.take_if_unblocked(false).is_none(),
                "empty/failed cycle produces no insertion"
            );
        }
    }

    #[test]
    fn latest_idle_restore_failure_is_visible_without_replaying_delivered_text() {
        let mut controller = recognizing_controller();
        let mut capture = inserting_capture();
        let generation = capture.generation;
        apply_paste_outcome(
            &mut controller,
            &mut capture,
            generation,
            super::PasteOutcome::Delivered,
        );
        let event = super::PasteOutcome::Restored(Err(InsertError::PasteboardRestore))
            .foreground_event(generation, &capture)
            .unwrap();
        assert_eq!(
            controller.handle(event),
            vec![Effect::ScheduleErrorReset { delay_ms: 3_000 }]
        );
        assert_eq!(
            controller.status(),
            &AppStatus::Error {
                message: "Текст вставлен, но не удалось восстановить буфер обмена",
                recoverable: true
            }
        );
        assert!(controller.handle(AppEvent::ErrorTimerFired).is_empty());
        assert_eq!(controller.status(), &AppStatus::Ready);
        assert!(controller
            .handle(AppEvent::RecognitionFinished(Ok("synthetic old".into())))
            .is_empty());
    }

    #[test]
    fn restore_warning_does_not_override_non_ready_foreground_states() {
        for event in [
            AppEvent::PermissionsChanged(PermissionSnapshot::default()),
            AppEvent::AsrRecoveryStarted,
            AppEvent::AsrUnavailable,
            AppEvent::EventTapLost,
        ] {
            let mut controller = recognizing_controller();
            let mut capture = inserting_capture();
            let generation = capture.generation;
            apply_paste_outcome(
                &mut controller,
                &mut capture,
                generation,
                super::PasteOutcome::Delivered,
            );
            controller.handle(event);
            let before = controller.status().clone();
            apply_paste_outcome(
                &mut controller,
                &mut capture,
                generation,
                super::PasteOutcome::Restored(Err(InsertError::PasteboardRestore)),
            );
            assert_eq!(controller.status(), &before);
        }
    }

    #[test]
    fn queued_text_is_discarded_on_invalidation_and_never_revived_after_reload() {
        let mut capture = inserting_capture();
        let mut queue = super::InsertionQueue::default();
        assert!(queue.park(super::QueuedInsertion {
            generation: capture.generation,
            text: "synthetic cancelled".into(),
            append_space: false
        }));
        capture.abandon();
        queue.discard_unless_current(&capture, &AppStatus::AsrUnavailable);
        capture.begin();
        assert!(queue.take_if_unblocked(false).is_none());
    }

    #[test]
    fn quit_and_update_open_wait_for_both_dictation_and_clipboard_lanes() {
        use crate::updater_runtime::{updater_open_allowed, OrderlyQuitGate};
        let mut quit = OrderlyQuitGate::default();
        quit.request();
        for (status, paste_pending) in [
            (AppStatus::Recognizing, true), // AwaitingPaste
            (AppStatus::Ready, true),       // Delivered, AwaitingRestore
            (AppStatus::Recording, true),
            (AppStatus::Recognizing, true), // preparation / ASR / parked text
            (AppStatus::Recognizing, false), // old restored, foreground still active
        ] {
            assert!(!updater_open_allowed(&status, paste_pending));
            assert!(!quit.take_if_ready(&status, paste_pending));
        }
        assert!(updater_open_allowed(&AppStatus::Ready, false));
        assert!(quit.take_if_ready(&AppStatus::Ready, false));
        assert!(!quit.take_if_ready(&AppStatus::Ready, false));
    }

    #[test]
    fn shutdown_restores_each_clipboard_phase_without_pasting_queued_text() {
        for delivered in [false, true] {
            let insertion = synthetic_insertion();
            let events = Rc::clone(&insertion.events);
            let mut timers = TestPasteTimers::default();
            let mut flow = PasteFlow::begin(1, insertion, &mut timers);
            if delivered {
                flow.handle_timer(TimerKind::PasteCommand(1), &mut timers);
            }
            let mut capture = inserting_capture();
            let mut queue = super::InsertionQueue::default();
            queue.park(super::QueuedInsertion {
                generation: capture.generation,
                text: "synthetic pending".into(),
                append_space: false,
            });
            capture.abandon();
            queue.discard_unless_current(&capture, &AppStatus::Recognizing);
            assert_eq!(flow.restore_on_shutdown(), Ok(()));
            assert_eq!(flow.restore_on_shutdown(), Ok(()));
            assert!(queue.take_if_unblocked(false).is_none());
            assert_eq!(
                flow.handle_timer(TimerKind::PasteCommand(1), &mut timers),
                None
            );
            assert_eq!(
                flow.handle_timer(TimerKind::RestorePasteboard(1), &mut timers),
                None
            );
            assert_eq!(
                events.borrow().as_slice(),
                if delivered {
                    &["paste", "restore"][..]
                } else {
                    &["restore"][..]
                }
            );
        }
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
                    TimerKind::PasteCommand(_) => {
                        self.events.borrow_mut().push("schedule_paste");
                    }
                    TimerKind::RestorePasteboard(_) => {
                        self.events.borrow_mut().push("schedule_restore");
                        panic!("timer scheduling failed");
                    }
                    _ => panic!("unexpected timer"),
                }
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
                let mut flow = PasteFlow::begin(1, insertion, &mut boundary);
                flow.handle_timer(TimerKind::PasteCommand(1), &mut boundary);
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
