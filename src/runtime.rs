use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::marker::PhantomPinned;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Child, Command};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
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
use crate::event_wake::{EventNotifier, EventSource, TerminalSender};
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
use crate::performance_diagnostics;
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

#[cfg(test)]
fn spawn_model_preparation_worker_with<F>(
    prepare: F,
) -> (
    Receiver<Result<VerifiedModel, ModelStoreError>>,
    JoinHandle<()>,
)
where
    F: FnOnce() -> Result<VerifiedModel, ModelStoreError> + Send + 'static,
{
    spawn_model_preparation_notified(prepare, EventNotifier::default())
}
fn spawn_model_preparation_notified<F>(
    prepare: F,
    notifier: EventNotifier,
) -> (
    Receiver<Result<VerifiedModel, ModelStoreError>>,
    JoinHandle<()>,
)
where
    F: FnOnce() -> Result<VerifiedModel, ModelStoreError> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let sender = TerminalSender::new(sender, notifier.clone());
        notifier.send(&sender, prepare());
    });
    (receiver, worker)
}
fn spawn_model_preparation_worker(
    notifier: EventNotifier,
) -> (
    Receiver<Result<VerifiedModel, ModelStoreError>>,
    JoinHandle<()>,
) {
    spawn_model_preparation_notified(prepare_runtime_model, notifier)
}

/// Owns one preparation attempt. Dropping an unfinished task deliberately
/// detaches its `JoinHandle`; the transactional model store is restart-safe,
/// and AppKit teardown must never wait for model I/O.
struct ModelPreparationTask {
    events: Receiver<Result<VerifiedModel, ModelStoreError>>,
    _worker: Option<JoinHandle<()>>,
}

impl ModelPreparationTask {
    fn new(
        events: Receiver<Result<VerifiedModel, ModelStoreError>>,
        worker: JoinHandle<()>,
    ) -> Self {
        Self {
            events,
            _worker: Some(worker),
        }
    }

    fn try_recv(&self) -> Result<Result<VerifiedModel, ModelStoreError>, TryRecvError> {
        self.events.try_recv()
    }
}

struct PermissionMigrationWorkerResult {
    paths: ModelPaths,
    migration: Result<PermissionMigrationSuccess, PermissionMigrationRunError>,
}

#[cfg(test)]
fn spawn_permission_migration_worker_with<F>(
    paths: ModelPaths,
    migrate: F,
) -> (Receiver<PermissionMigrationWorkerResult>, JoinHandle<()>)
where
    F: FnOnce() -> Result<PermissionMigrationSuccess, PermissionMigrationRunError> + Send + 'static,
{
    spawn_permission_migration_notified(paths, migrate, EventNotifier::default())
}
fn spawn_permission_migration_notified<F>(
    paths: ModelPaths,
    migrate: F,
    notifier: EventNotifier,
) -> (Receiver<PermissionMigrationWorkerResult>, JoinHandle<()>)
where
    F: FnOnce() -> Result<PermissionMigrationSuccess, PermissionMigrationRunError> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let sender = TerminalSender::new(sender, notifier.clone());
        let migration = migrate();
        notifier.send(
            &sender,
            PermissionMigrationWorkerResult { paths, migration },
        );
    });
    (receiver, worker)
}

fn spawn_permission_migration_worker(
    paths: ModelPaths,
    notifier: EventNotifier,
) -> (Receiver<PermissionMigrationWorkerResult>, JoinHandle<()>) {
    spawn_permission_migration_notified(paths, run_system_permission_migration, notifier)
}

/// Owns one permission migration attempt. Dropping an unfinished task
/// deliberately detaches its worker so AppKit teardown never waits for TCC.
struct PermissionMigrationTask {
    events: Receiver<PermissionMigrationWorkerResult>,
    _worker: Option<JoinHandle<()>>,
}

impl PermissionMigrationTask {
    fn new(events: Receiver<PermissionMigrationWorkerResult>, worker: JoinHandle<()>) -> Self {
        Self {
            events,
            _worker: Some(worker),
        }
    }

    fn try_recv(&self) -> Result<PermissionMigrationWorkerResult, TryRecvError> {
        self.events.try_recv()
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
            MenuCommand::BeginTriggerAssignment { .. }
            | MenuCommand::SetAppendSpace(_)
            | MenuCommand::Boundary(_) => return Err(()),
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
        self.flow.request_completed(authorization(), boundary);
        true
    }
}

struct SystemMicrophonePermissionBoundary {
    notifier: EventNotifier,
    completion_sender: Sender<()>,
}

impl SystemMicrophonePermissionBoundary {
    fn completion(&self) -> impl Fn() + Send + Sync + 'static {
        let completion_sender = self.completion_sender.clone();
        let notifier = self.notifier.clone();
        move || {
            notifier.send(&completion_sender, ());
        }
    }
}
impl MicrophonePermissionBoundary for SystemMicrophonePermissionBoundary {
    fn request_access(&mut self) -> bool {
        permissions::request_microphone_access(self.completion())
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
    AsrWatchdog,
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
    kind: TimerKind,
    active: Cell<bool>,
    fired: Cell<bool>,
    queue: Rc<RefCell<VecDeque<Rc<TimerContext>>>>,
    notifier: EventNotifier,
}
struct ScheduledTimer {
    timer: CFRunLoopTimer,
    context: Rc<TimerContext>,
}
impl ScheduledTimer {
    fn new(
        run_loop: &CFRunLoop,
        queue: Rc<RefCell<VecDeque<Rc<TimerContext>>>>,
        notifier: EventNotifier,
        kind: TimerKind,
        delay_ms: u64,
    ) -> Self {
        let context = Rc::new(TimerContext {
            kind,
            active: Cell::new(true),
            fired: Cell::new(false),
            queue,
            notifier,
        });
        let mut cf_context = CFRunLoopTimerContext {
            version: 0,
            info: Rc::as_ptr(&context).cast_mut().cast(),
            retain: Some(retain_timer_context),
            release: Some(release_timer_context),
            copyDescription: None,
        };
        let timer = CFRunLoopTimer::new(
            CFDate::now().abs_time() + milliseconds_to_seconds(delay_ms),
            0.0,
            0,
            0,
            timer_fired,
            &mut cf_context,
        );
        run_loop.add_timer(&timer, unsafe { kCFRunLoopCommonModes });
        Self { timer, context }
    }
    fn remove(self, run_loop: &CFRunLoop) {
        self.context.active.set(false);
        unsafe {
            core_foundation::runloop::CFRunLoopTimerInvalidate(self.timer.as_concrete_TypeRef())
        };
        run_loop.remove_timer(&self.timer, unsafe { kCFRunLoopCommonModes });
    }
}
impl Drop for ScheduledTimer {
    fn drop(&mut self) {
        self.context.active.set(false);
        unsafe {
            core_foundation::runloop::CFRunLoopTimerInvalidate(self.timer.as_concrete_TypeRef())
        };
    }
}
extern "C" fn retain_timer_context(info: *const c_void) -> *const c_void {
    unsafe { Rc::increment_strong_count(info.cast::<TimerContext>()) };
    info
}
extern "C" fn release_timer_context(info: *const c_void) {
    unsafe { drop(Rc::from_raw(info.cast::<TimerContext>())) };
}
const fn milliseconds_to_seconds(milliseconds: u64) -> f64 {
    milliseconds as f64 / 1_000.0
}
extern "C" fn timer_fired(_timer: CFRunLoopTimerRef, raw_context: *mut c_void) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        let ptr = raw_context.cast::<TimerContext>();
        Rc::increment_strong_count(ptr);
        let context = Rc::from_raw(ptr);
        // Timer callbacks never borrow Runtime, including during initialization
        // and nested AppKit loops. Each token fires once and is invalidated on
        // cancellation; a deferred old generation cannot cancel a newer timer.
        if context.active.get() && !context.fired.replace(true) {
            context.queue.borrow_mut().push_back(context.clone());
            context.notifier.notify();
        }
    }));
    if result.is_err() {
        tracing::error!(error_category = "timer_callback_panic");
    }
}

#[derive(Default)]
struct PermissionRetry {
    delay_ms: u64,
}
impl PermissionRetry {
    fn restart(&mut self) {
        self.delay_ms = 0;
    }
    fn next(&mut self, needed: bool) -> Option<u64> {
        if !needed {
            self.restart();
            return None;
        }
        self.delay_ms = if self.delay_ms == 0 {
            PERMISSION_POLL_MS
        } else {
            (self.delay_ms * 2).min(30_000)
        };
        Some(self.delay_ms)
    }
}

fn asr_wake_deadline(
    now: Instant,
    operation: Option<Instant>,
    cleanup_pending: bool,
    scheduled: Option<Instant>,
    cleanup_check_ms: &mut u64,
) -> Option<Instant> {
    if !cleanup_pending {
        *cleanup_check_ms = 10;
    }
    operation.or_else(|| {
        if !cleanup_pending {
            return None;
        }
        scheduled.or_else(|| {
            let deadline = now + Duration::from_millis(*cleanup_check_ms);
            *cleanup_check_ms = (*cleanup_check_ms * 2).min(1_000);
            Some(deadline)
        })
    })
}

const EVENT_LANES: usize = 11;
const EVENTS_PER_LANE: usize = 32;
const EVENTS_PER_PASS: usize = 128;
const EVENT_BUDGET: Duration = Duration::from_millis(2);

fn drain_event_lanes(
    next_lane: &mut usize,
    mut drain: impl FnMut(usize) -> bool,
    mut elapsed: impl FnMut() -> Duration,
) -> bool {
    let mut counts = [0; EVENT_LANES];
    let mut available = [true; EVENT_LANES];
    let mut total = 0;
    let mut continuation = false;
    while available.iter().any(|ready| *ready) {
        let lane = *next_lane;
        *next_lane = (lane + 1) % EVENT_LANES;
        if !available[lane] {
            continue;
        }
        if drain(lane) {
            counts[lane] += 1;
            total += 1;
            if counts[lane] == EVENTS_PER_LANE {
                available[lane] = false;
                continuation = true;
            }
            if total == EVENTS_PER_PASS || elapsed() >= EVENT_BUDGET {
                continuation = true;
                break;
            }
        } else {
            available[lane] = false;
        }
    }
    continuation
}

// FIFO fences bound the prefix without copying it or allowing later arrivals
// to prolong it. At most one clipboard completion and one open preflight exist.
#[derive(Default)]
struct HotkeyPreflight {
    next: u64,
    paste: Option<(u64, u64, PasteOutcome)>,
    restored: Option<(u64, PasteOutcome)>,
    open: Option<(u64, UpdaterState)>,
    assignment: Option<(u64, HotkeySignal)>,
}
impl HotkeyPreflight {
    fn take_paste(&mut self, marker: u64) -> Option<(u64, PasteOutcome)> {
        if self.paste.as_ref().is_none_or(|(id, _, _)| *id != marker) {
            return None;
        }
        self.paste
            .take()
            .map(|(_, generation, outcome)| (generation, outcome))
    }
    fn take_open(&mut self, marker: u64) -> Option<UpdaterState> {
        if self.open.as_ref().is_none_or(|(id, _)| *id != marker) {
            return None;
        }
        self.open.take().map(|(_, state)| state)
    }
    fn marker(&mut self) -> u64 {
        self.next = self
            .next
            .checked_add(1)
            .expect("hotkey boundary identity exhausted");
        self.next
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
    menu_sender: Sender<MenuCommand>,
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
    asr_phase_started: Option<(&'static str, Instant)>,
    asr_shutdown: AsrShutdown,
    model_preparation: Option<ModelPreparationTask>,
    permission_migration: Option<PermissionMigrationTask>,
    prepared_model_paths: Option<ModelPaths>,
    permission_build_identity: Option<BuildIdentity>,
    updater: Option<SystemUpdaterLane>,
    orderly_quit: OrderlyQuitGate,
    run_loop: CFRunLoop,
    event_source: Rc<EventSource>,
    notifier: EventNotifier,
    timer_events: Rc<RefCell<VecDeque<Rc<TimerContext>>>>,
    next_lane: usize,
    preflight: HotkeyPreflight,
    asr_timer: Option<ScheduledTimer>,
    asr_timer_deadline: Option<Instant>,
    cleanup_check_ms: u64,
    permission_retry: PermissionRetry,
    permission_setup_started: bool,
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
/// that can invoke the event source through its stored Runtime pointer.
pub fn finish_after_run(owner: &mut Pin<Box<Runtime>>) {
    let source = owner.as_ref().event_source.clone();
    pump_until_cleanup_guarded(
        owner,
        || source.suspend(),
        Runtime::shutdown_after_run_step,
        || {
            CFRunLoop::run_in_mode(
                unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                Duration::from_millis(20),
                true,
            );
        },
    );
}

/// Every pointee borrow is created for one step and ends when that call returns.
/// The higher-ranked step returns only a boolean, so it cannot retain its
/// temporary `Pin<&mut T>` for use while the following pump invokes callbacks.
/// The outer function retains only a reference to the stable owner handle.
#[cfg(test)]
fn pump_until_cleanup<T>(
    owner: &mut Pin<Box<T>>,
    mut step: impl for<'access> FnMut(Pin<&'access mut T>) -> bool,
    mut pump: impl FnMut(),
) {
    pump_until_cleanup_guarded(owner, || (), &mut step, &mut pump);
}

fn pump_until_cleanup_guarded<T, G>(
    owner: &mut Pin<Box<T>>,
    mut guard: impl FnMut() -> G,
    mut step: impl for<'access> FnMut(Pin<&'access mut T>) -> bool,
    mut pump: impl FnMut(),
) {
    loop {
        let complete = {
            let _guard = guard();
            step(owner.as_mut())
        };
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
        // Source allocation fails before any worker is launched. It is not
        // callable until attach after the initialization pointee borrow ends.
        let event_source = Rc::new(EventSource::new(CFRunLoop::get_main()));
        let notifier = event_source.notifier();
        let (hotkey_sender, hotkey_events) = mpsc::channel();
        let (menu_sender, menu_commands) = mpsc::channel();
        let asr = AsrTask::spawn_notified(notifier.clone());
        let (model_preparation_events, model_preparation_worker) =
            spawn_model_preparation_worker(notifier.clone());
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
        let updater =
            updater_config.map(|config| SystemUpdaterLane::production(config, notifier.clone()));

        let mut runtime = Box::pin(Self {
            controller: AppController::new(),
            menu: MenuBar::new_notified(
                preferences,
                append_space,
                menu_sender.clone(),
                hotkey_control.clone(),
                menu_readiness.clone(),
                notifier.clone(),
            ),
            preferences: RuntimePreferences::new(preferences, preference_repository),
            output_preferences,
            menu_commands,
            menu_sender,
            hotkey_control,
            assignment: AssignmentTracker::default(),
            menu_readiness,
            recorder: AudioRecorder::new(),
            audio_preparation: AudioPreparationTask::spawn_notified(notifier.clone()),
            dictation_capture: DictationCapture::default(),
            hotkey: None,
            hotkey_sender,
            hotkey_events,
            asr,
            asr_recovery: AsrRecovery::default(),
            asr_generation: None,
            asr_phase_started: None,
            asr_shutdown: AsrShutdown::default(),
            model_preparation: Some(model_preparation),
            permission_migration: None,
            prepared_model_paths: None,
            permission_build_identity: None,
            updater,
            orderly_quit: OrderlyQuitGate::default(),
            run_loop: CFRunLoop::get_main(),
            event_source: event_source.clone(),
            notifier,
            timer_events: Rc::default(),
            next_lane: 0,
            preflight: HotkeyPreflight::default(),
            asr_timer: None,
            asr_timer_deadline: None,
            cleanup_check_ms: 10,
            permission_retry: PermissionRetry::default(),
            permission_setup_started: false,
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
        // and is never exposed without its `Pin`. Its source may therefore
        // retain this address until `Drop` closes the event source.
        let initialization_guard = event_source.suspend();
        let runtime_ref = unsafe { Pin::as_mut(&mut runtime).get_unchecked_mut() };
        let pointer = runtime_ref as *mut Runtime;
        runtime_ref.initialize_updater();
        tracing::info!(
            lifecycle = "started",
            state = status_name(runtime_ref.controller.status())
        );
        event_source.set_handler(move || unsafe { (*pointer).drain_events() });
        // runtime_ref's last use is above. Release the setup guard only after
        // initialization access ends, then register through the separate owner.
        drop(initialization_guard);
        event_source.attach();
        runtime
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
        self.permission_setup_started = true;
        self.permission_retry.restart();
        self.poll_permissions();
    }

    fn handle_timer(&mut self, kind: TimerKind) {
        match kind {
            TimerKind::AsrWatchdog => {
                cancel_timer(&self.run_loop, &mut self.asr_timer);
                self.asr_timer_deadline = None;
            }
            TimerKind::AutomaticUpdateCheck => self.handle_automatic_update_timer(),
            TimerKind::PollPermissions => {
                cancel_timer(&self.run_loop, &mut self.permission_timer);
                self.poll_permissions();
            }
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
        self.updater_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            self.timer_events.clone(),
            self.notifier.clone(),
            TimerKind::AutomaticUpdateCheck,
            delay_ms,
        ));
    }

    fn drain_events(&mut self) {
        let started = Instant::now();
        let mut next_lane = self.next_lane;
        let continuation = drain_event_lanes(
            &mut next_lane,
            |lane| self.drain_lane(lane),
            || started.elapsed(),
        );
        self.next_lane = next_lane;
        if self.controller.status() == &AppStatus::AsrCleanupPending && self.asr.retry_ready() {
            self.dispatch(AppEvent::AsrUnavailable);
        }
        self.try_orderly_quit();
        self.refresh_asr_watchdog();
        if continuation {
            self.notifier.notify();
        }
    }

    fn drain_lane(&mut self, lane: usize) -> bool {
        if lane == 0 {
            let event = self.timer_events.borrow_mut().pop_front();
            if let Some(event) = event {
                if event.active.get() {
                    self.handle_timer(event.kind);
                }
                return true;
            }
            return false;
        }
        if self.asr_shutdown.started.is_some() {
            return false;
        }
        match lane {
            1 => self.drain_menu_actions(),
            2 => self.drain_updater_worker_results(),
            3 => self.drain_updater_actions(),
            4 => self.drain_model_preparation(),
            5 => self.drain_permission_migration(),
            6 => self.drain_microphone_permission_completions(),
            7 => match self.menu_commands.try_recv() {
                Ok(command) => {
                    self.handle_menu_command(command);
                    true
                }
                Err(_) => false,
            },
            8 if self.preflight.assignment.is_some() => false,
            8 => match self.hotkey_events.try_recv() {
                Ok(signal) => {
                    if matches!(
                        signal,
                        HotkeySignal::AssignmentSelected { .. }
                            | HotkeySignal::AssignmentCancelled { .. }
                    ) {
                        let marker = self.preflight.marker();
                        if self
                            .notifier
                            .send(&self.menu_sender, MenuCommand::Boundary(marker))
                        {
                            self.preflight.assignment = Some((marker, signal));
                        } else {
                            self.begin_shutdown();
                        }
                    } else {
                        self.handle_hotkey(signal);
                    }
                    true
                }
                Err(_) => false,
            },
            9 => self.drain_audio_preparation(),
            10 => self.drain_asr_result(),
            _ => unreachable!(),
        }
    }

    fn drain_asr_result(&mut self) -> bool {
        let Some(result) = self.asr.poll_one(Instant::now()) else {
            return false;
        };
        if result.is_some() {
            let outcome = if matches!(
                &result,
                Some(Ok(AsrEvent::Loaded(Ok(())) | AsrEvent::Recognized(Ok(_))))
            ) {
                "ok"
            } else {
                "error"
            };
            if let Some((phase, started)) = self.asr_phase_started.take() {
                performance_diagnostics::log(phase, started.elapsed(), outcome);
            }
        }
        match result {
            Some(Ok(AsrEvent::Loaded(Ok(())))) => {
                self.asr_recovery.loaded();
                self.permission_retry.restart();
                self.poll_permissions();
                self.dispatch(AppEvent::ModelLoaded(Ok(())));
                if self.tap_needs_retry {
                    self.observe_tap_state(TapState::Lost);
                }
            }
            Some(Ok(AsrEvent::Loaded(Err(_)))) => self.handle_asr_error(AsrTaskError::WorkerFailed),
            Some(Ok(AsrEvent::Recognized(result))) => {
                if self
                    .dictation_capture
                    .accept_recognition(self.asr_generation.take())
                {
                    self.dispatch(AppEvent::RecognitionFinished(result));
                }
            }
            Some(Err(error)) => self.handle_asr_error(error),
            None => {} // consumed stale completion still counts toward budget
        }
        true
    }

    fn refresh_asr_watchdog(&mut self) {
        let cleanup_pending = (self.controller.status() == &AppStatus::AsrCleanupPending
            && !self.asr.retry_ready())
            || (self.asr_shutdown.started.is_some() && !self.asr.cleanup_complete());
        let now = Instant::now();
        let deadline = asr_wake_deadline(
            now,
            self.asr.next_deadline(),
            cleanup_pending,
            self.asr_timer_deadline,
            &mut self.cleanup_check_ms,
        );
        if deadline == self.asr_timer_deadline {
            return;
        }
        cancel_timer(&self.run_loop, &mut self.asr_timer);
        self.asr_timer_deadline = deadline;
        if let Some(deadline) = deadline {
            // Ceiling, minimum 1 ms: an early CF firing cannot create a spin.
            let delay = deadline
                .saturating_duration_since(now)
                .as_nanos()
                .div_ceil(1_000_000)
                .max(1) as u64;
            self.asr_timer = Some(ScheduledTimer::new(
                &self.run_loop,
                self.timer_events.clone(),
                self.notifier.clone(),
                TimerKind::AsrWatchdog,
                delay,
            ));
        }
    }

    fn handle_asr_error(&mut self, error: AsrTaskError) {
        tracing::error!(error_category = "asr_worker", error = ?error);
        self.asr_phase_started = None;
        // Cleanup checks must not inherit the retired operation's (possibly
        // minutes-away) watchdog deadline.
        cancel_timer(&self.run_loop, &mut self.asr_timer);
        self.asr_timer_deadline = None;
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
        if let Err(error) = self.send_asr(
            AsrCommand::Load(paths),
            performance_diagnostics::ASR_WORKER_LOAD,
        ) {
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

    fn drain_model_preparation(&mut self) -> bool {
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
            return false;
        };

        self.model_preparation.take();

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
        true
    }

    fn begin_permission_migration(&mut self, paths: ModelPaths) {
        if self.permission_migration.is_some() {
            return;
        }
        self.dispatch(AppEvent::PermissionMigrationStarted);
        self.prepared_model_paths = Some(paths.clone());
        let (events, worker) = spawn_permission_migration_worker(paths, self.notifier.clone());
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

    fn drain_permission_migration(&mut self) -> bool {
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
            return false;
        };

        self.permission_migration.take();

        let Some(result) = completed else {
            tracing::error!(error_category = "permission_migration_worker_disconnected");
            self.dispatch(AppEvent::PermissionMigrationFailed);
            return true;
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
                if let Err(error) = self.send_asr(
                    AsrCommand::Load(result.paths),
                    performance_diagnostics::ASR_WORKER_LOAD,
                ) {
                    self.handle_asr_error(error);
                }
            }
            Err(error) => {
                tracing::error!(error = ?error, error_category = "permission_migration");
                self.prepared_model_paths = Some(result.paths);
                self.dispatch(AppEvent::PermissionMigrationFailed);
            }
        }
        true
    }

    fn begin_model_preparation(&mut self) {
        if self.model_preparation.is_some() {
            return;
        }
        self.dispatch(AppEvent::ModelPreparationStarted);
        let (events, worker) = spawn_model_preparation_worker(self.notifier.clone());
        self.model_preparation = Some(ModelPreparationTask::new(events, worker));
    }

    fn drain_menu_actions(&mut self) -> bool {
        let Some(action) = self.menu.take_action() else {
            return false;
        };
        match action {
            MenuAction::Quit => self.begin_shutdown(),
            MenuAction::RefreshPermissions => self.refresh_permissions_from_interaction(),
            MenuAction::RetryAsr => {
                self.refresh_permissions_from_interaction();
                self.retry_asr();
            }
            MenuAction::OpenPermission(permission) => {
                self.refresh_permissions_from_interaction();
                if !permissions::open_settings(permission) {
                    tracing::warn!(error_category = "open_permission_settings");
                }
            }
            MenuAction::RetryModelPreparation => self.begin_model_preparation(),
            MenuAction::RetryPermissionMigration => self.retry_permission_migration(),
        }
        true
    }

    fn drain_updater_worker_results(&mut self) -> bool {
        let Some(updater) = self.updater.as_mut() else {
            return false;
        };
        let (effects, handled) = updater.poll_worker_result();
        self.apply_updater_effects(effects);
        if handled {
            self.render_updater_menu();
        }
        handled
    }

    fn drain_updater_actions(&mut self) -> bool {
        // Park only the already-queued open preflight, preserving FIFO actions.
        if self.preflight.open.is_some() {
            return false;
        }
        if let Some(action) = self.menu.take_updater_action() {
            if self.updater.is_none() {
                return true;
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
                    if !updater_open_allowed(self.controller.status(), self.clipboard_busy()) {
                        return true;
                    }
                    let marker = self.preflight.marker();
                    let state = self.updater.as_ref().unwrap().state().clone();
                    if self
                        .notifier
                        .send(&self.hotkey_sender, HotkeySignal::Boundary(marker))
                    {
                        self.preflight.open = Some((marker, state));
                    }
                    Vec::new()
                }
            };
            self.apply_updater_effects(effects);
            self.render_updater_menu();
            true
        } else {
            false
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
        self.preflight = HotkeyPreflight::default();
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
        cancel_timer(&self.run_loop, &mut self.asr_timer);
        self.asr_timer_deadline = None;
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
    /// function never explicitly pumps callbacks. Native restoration may pump;
    /// the owner-handle caller holds the shared source guard for that interval.
    fn shutdown_after_run_step(self: Pin<&mut Self>) -> bool {
        let runtime = unsafe { self.get_unchecked_mut() };
        runtime.begin_shutdown();
        runtime.refresh_asr_watchdog();
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

    fn drain_microphone_permission_completions(&mut self) -> bool {
        let mut boundary = SystemMicrophonePermissionBoundary {
            notifier: self.notifier.clone(),
            completion_sender: self.microphone_permissions.completion_sender(),
        };
        let should_repoll = self.microphone_permissions.drain_completions(
            SystemPermissionProbe::microphone_authorization,
            &mut boundary,
        );
        if should_repoll {
            self.refresh_permissions_from_interaction();
        }
        should_repoll
    }

    fn handle_hotkey(&mut self, signal: HotkeySignal) {
        match signal {
            HotkeySignal::Boundary(marker) => self.complete_hotkey_preflight(marker),
            HotkeySignal::Pressed => {
                // Probe before TriggerPressed changes Ready to Recording.
                if self.controller.status() == &AppStatus::Ready
                    || matches!(self.controller.status(), AppStatus::PermissionBlocked(_))
                {
                    self.refresh_permissions_from_interaction();
                }
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
                self.refresh_permissions_from_interaction();
            }
            HotkeySignal::TapRestored => {
                self.tap_needs_retry = false;
                self.observe_tap_state(TapState::Restored);
                self.refresh_permissions_from_interaction();
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
        if let MenuCommand::Boundary(marker) = command {
            if self
                .preflight
                .assignment
                .as_ref()
                .is_some_and(|(id, _)| *id == marker)
            {
                let (_, signal) = self.preflight.assignment.take().unwrap();
                self.handle_hotkey(signal);
                self.notifier.notify();
            }
            return;
        }
        if command == MenuCommand::ResetTrigger {
            let pending_epoch =
                self.preflight
                    .assignment
                    .as_ref()
                    .and_then(|(_, signal)| match signal {
                        HotkeySignal::AssignmentSelected { epoch, .. }
                        | HotkeySignal::AssignmentCancelled { epoch } => Some(*epoch),
                        _ => None,
                    });
            if pending_epoch.is_some() && pending_epoch == self.assignment.active_epoch {
                self.preflight.assignment = None;
                self.notifier.notify();
            }
            self.assignment.active_epoch = None;
        }
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

    fn refresh_permissions_from_interaction(&mut self) {
        self.permission_retry.restart();
        self.poll_permissions();
    }

    fn poll_permissions(&mut self) {
        if !self.permission_setup_started || self.asr_shutdown.started.is_some() {
            return;
        }
        cancel_timer(&self.run_loop, &mut self.permission_timer);
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
                notifier: self.notifier.clone(),
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
            reset_hotkey_before_drop_notified(
                &self.hotkey_control,
                &self.hotkey_sender,
                &self.notifier,
            );
            self.hotkey.take();
            self.schedule_permission_retry(permissions);
            return;
        }

        if self.tap_needs_retry {
            reset_hotkey_before_drop_notified(
                &self.hotkey_control,
                &self.hotkey_sender,
                &self.notifier,
            );
            self.hotkey.take();
        }
        if self.hotkey.is_none() {
            match HotkeyListener::install_notified(
                self.hotkey_sender.clone(),
                self.hotkey_control.clone(),
                self.notifier.clone(),
            ) {
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
        self.schedule_permission_retry(permissions);
    }

    fn schedule_permission_retry(&mut self, permissions: PermissionSnapshot) {
        if let Some(delay) = self
            .permission_retry
            .next(permissions != PermissionSnapshot::all() || self.tap_needs_retry)
        {
            self.permission_timer = Some(ScheduledTimer::new(
                &self.run_loop,
                self.timer_events.clone(),
                self.notifier.clone(),
                TimerKind::PollPermissions,
                delay,
            ));
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
        if self
            .preflight
            .paste
            .as_ref()
            .is_some_and(|(_, generation, _)| *generation != self.dictation_capture.generation)
        {
            self.preflight.paste = None;
            self.preflight.restored = None;
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
                        notifier: self.notifier.clone(),
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
                if let Err(error) = self.send_asr(
                    AsrCommand::Transcribe(samples),
                    performance_diagnostics::ASR_WORKER_TRANSCRIPTION,
                ) {
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

    fn drain_audio_preparation(&mut self) -> bool {
        let Some(prepared) = self.audio_preparation.poll_one() else {
            return false;
        };
        if let Some(prepared) = prepared {
            if !self
                .dictation_capture
                .accept(prepared.generation, self.controller.status())
            {
                return true;
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
            performance_diagnostics::log(
                performance_diagnostics::AUDIO_PREPARATION,
                prepared.elapsed,
                if prepared.result.is_ok() {
                    "ok"
                } else {
                    "error"
                },
            );
            if let Err(error) = &prepared.result {
                tracing::warn!(error_category = "audio_preparation", error = ?error);
            }
            self.dispatch(capture_result_event(prepared.result));
        }
        true
    }

    fn replace_finish_timer(&mut self, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.finish_timer);
        self.finish_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            self.timer_events.clone(),
            self.notifier.clone(),
            TimerKind::FinishCapture,
            delay_ms,
        ));
    }

    fn replace_capture_limit_timer(&mut self, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.capture_limit_timer);
        self.capture_limit_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            self.timer_events.clone(),
            self.notifier.clone(),
            TimerKind::CaptureLimit,
            delay_ms,
        ));
    }

    fn replace_error_timer(&mut self, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.error_timer);
        self.error_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            self.timer_events.clone(),
            self.notifier.clone(),
            TimerKind::ResetError,
            delay_ms,
        ));
    }

    fn replace_insertion_timer(&mut self, kind: TimerKind, delay_ms: u64) {
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        self.insertion_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            self.timer_events.clone(),
            self.notifier.clone(),
            kind,
            delay_ms,
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
        let started = Instant::now();
        let insertion = text_inserter::begin(&queued.text, queued.append_space);
        performance_diagnostics::log(
            performance_diagnostics::INSERTION_PREPARATION,
            started.elapsed(),
            if insertion.is_ok() { "ok" } else { "error" },
        );
        match insertion {
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
                // made before Command-V, even when a source pass was delayed.
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
            }
            PasteOutcome::Restored(Ok(())) => {}
        }
        if matches!(outcome, PasteOutcome::Restored(_))
            && self
                .preflight
                .paste
                .as_ref()
                .is_some_and(|(_, origin, _)| *origin == generation)
        {
            self.preflight.restored = Some((generation, outcome));
            return;
        }
        if matches!(
            outcome,
            PasteOutcome::Delivered | PasteOutcome::PasteFailed(_)
        ) {
            let marker = self.preflight.marker();
            if self
                .notifier
                .send(&self.hotkey_sender, HotkeySignal::Boundary(marker))
            {
                debug_assert!(self.preflight.paste.is_none());
                self.preflight.paste = Some((marker, generation, outcome));
                return;
            }
            // A broken local hotkey channel cannot establish a safe prefix.
            // Fail closed, without leaving clipboard/shutdown ownership parked.
            self.begin_shutdown();
            return;
        }
        if let Some(event) = outcome.foreground_event(generation, &self.dictation_capture) {
            self.dispatch(event);
        }
    }

    fn complete_hotkey_preflight(&mut self, marker: u64) {
        if let Some((generation, outcome)) = self.preflight.take_paste(marker) {
            if let Some(event) = outcome.foreground_event(generation, &self.dictation_capture) {
                self.dispatch(event);
            }
            if let Some((origin, restored)) = self.preflight.restored.take() {
                if let Some(event) = restored.foreground_event(origin, &self.dictation_capture) {
                    self.dispatch(event);
                }
            }
            self.drain_insertion_queue();
        }
        if let Some(state) = self.preflight.take_open(marker) {
            if updater_open_allowed(self.controller.status(), self.clipboard_busy())
                && self
                    .updater
                    .as_ref()
                    .is_some_and(|updater| updater.state() == &state)
            {
                let effects = self.updater.as_mut().unwrap().request_open();
                self.apply_updater_effects(effects);
            }
            self.notifier.notify();
        }
        self.render_updater_menu();
    }

    fn cancel_finish_timer(&mut self) {
        cancel_timer(&self.run_loop, &mut self.finish_timer);
    }

    fn send_asr(&mut self, command: AsrCommand, phase: &'static str) -> Result<(), AsrTaskError> {
        let started = Instant::now();
        match self.asr.send(command, started) {
            Ok(()) => {
                self.asr_phase_started = Some((phase, started));
                Ok(())
            }
            Err(error) => {
                performance_diagnostics::log(phase, started.elapsed(), "error");
                Err(error)
            }
        }
    }

    fn cancel_capture_limit_timer(&mut self) {
        cancel_timer(&self.run_loop, &mut self.capture_limit_timer);
    }
}

#[cfg(test)]
fn reset_hotkey_before_drop(control: &HotkeyControl, sender: &Sender<HotkeySignal>) {
    reset_hotkey_before_drop_notified(control, sender, &EventNotifier::default());
}
fn reset_hotkey_before_drop_notified(
    control: &HotkeyControl,
    sender: &Sender<HotkeySignal>,
    notifier: &EventNotifier,
) {
    if control.reset_for_listener_removal() {
        notifier.send(sender, HotkeySignal::Cancelled);
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
        self.event_source.close();
        self.dictation_capture.abandon();
        self.audio_preparation.stop();
        cancel_timer(&self.run_loop, &mut self.asr_timer);
        cancel_timer(&self.run_loop, &mut self.updater_timer);
        cancel_timer(&self.run_loop, &mut self.permission_timer);
        cancel_timer(&self.run_loop, &mut self.finish_timer);
        cancel_timer(&self.run_loop, &mut self.capture_limit_timer);
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        cancel_timer(&self.run_loop, &mut self.error_timer);
        self.timer_events.borrow_mut().clear();
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
        PermissionMigrationTask, RuntimePreferences, TapState, TimerKind, PERMISSION_POLL_MS,
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
            // Same lifetime shape as the source handler: this pointer was
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
        assert_eq!(super::EVENTS_PER_LANE, 32);
        assert_eq!(super::EVENTS_PER_PASS, 128);
        assert_eq!(super::EVENT_BUDGET, Duration::from_millis(2));
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

#[cfg(test)]
mod event_tests {
    use super::*;
    use core_foundation::runloop::kCFRunLoopDefaultMode;

    fn ready() -> AppController {
        let mut controller = AppController::new();
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        controller
    }

    #[test]
    fn lane_and_pass_caps_are_fair_and_consumed_suppression_continues() {
        let mut cursor = 0;
        let mut counts = [0; EVENT_LANES];
        assert!(drain_event_lanes(
            &mut cursor,
            |lane| {
                counts[lane] += 1;
                true
            },
            || Duration::ZERO
        ));
        assert_eq!(counts.iter().sum::<usize>(), EVENTS_PER_PASS);
        assert!(counts.iter().all(|count| (11..=12).contains(count)));
        let mut remaining = 65;
        let mut passes = 0;
        loop {
            let mut consumed = 0;
            let more = drain_event_lanes(
                &mut cursor,
                |lane| {
                    if lane == 8 && remaining != 0 {
                        remaining -= 1;
                        consumed += 1;
                        true
                    } else {
                        false
                    }
                },
                || Duration::ZERO,
            );
            assert!(consumed <= 32);
            passes += 1;
            if !more {
                break;
            }
        }
        assert_eq!(passes, 3);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn cooperative_time_budget_rotates_even_if_one_handler_takes_two_ms() {
        let mut cursor = 0;
        let mut serviced = Vec::new();
        for _ in 0..EVENT_LANES {
            assert!(drain_event_lanes(
                &mut cursor,
                |lane| {
                    serviced.push(lane);
                    true
                },
                || EVENT_BUDGET
            ));
        }
        assert_eq!(serviced, (0..EVENT_LANES).collect::<Vec<_>>());
        assert!(!drain_event_lanes(&mut cursor, |_| false, || EVENT_BUDGET));
    }

    #[test]
    fn bounded_paste_and_open_fences_preserve_busy_prefix_and_later_press() {
        let mut controller = ready();
        controller.handle(AppEvent::TriggerPressed);
        controller.handle(AppEvent::TriggerReleased { short: false });
        let mut capture = DictationCapture::default();
        capture.begin();
        capture.phase = CapturePhase::Inserting;
        let mut preflight = HotkeyPreflight::default();
        let paste = preflight.marker();
        let open = preflight.marker();
        preflight.paste = Some((paste, capture.generation, PasteOutcome::Delivered));
        preflight.open = Some((open, UpdaterState::Current));
        let mut hotkeys = VecDeque::from(vec![HotkeySignal::Pressed; 70]);
        hotkeys.push_back(HotkeySignal::Boundary(open));
        hotkeys.push_back(HotkeySignal::Boundary(paste));
        hotkeys.push_back(HotkeySignal::Pressed);
        let mut cursor = 0;
        let mut presses = 0;
        let mut opened = false;
        let mut passes = 0;
        loop {
            let more = drain_event_lanes(
                &mut cursor,
                |lane| {
                    if lane != 8 {
                        return false;
                    }
                    let Some(signal) = hotkeys.pop_front() else {
                        return false;
                    };
                    match signal {
                        HotkeySignal::Boundary(marker) => {
                            if preflight.take_open(marker).is_some() {
                                opened = updater_open_allowed(controller.status(), false);
                            }
                            if let Some((generation, outcome)) = preflight.take_paste(marker) {
                                assert_eq!(presses, 0);
                                controller.handle(
                                    outcome.foreground_event(generation, &capture).unwrap(),
                                );
                            }
                        }
                        HotkeySignal::Pressed => {
                            if capture_start_allowed(
                                controller.status(),
                                Some(PasteFlowState::AwaitingRestore),
                                false,
                            ) {
                                presses += 1;
                                controller.handle(AppEvent::TriggerPressed);
                            }
                        }
                        _ => unreachable!(),
                    }
                    true
                },
                || Duration::ZERO,
            );
            passes += 1;
            if passes <= 2 {
                assert_eq!(controller.status(), &AppStatus::Recognizing);
                assert!(preflight.paste.is_some());
                assert!(!opened);
            }
            if !more {
                break;
            }
        }
        assert_eq!(passes, 3);
        assert_eq!(presses, 1);
        assert!(
            !opened,
            "open prefix was completed while foreground remained busy"
        );
        assert!(preflight.take_paste(paste).is_none());
        assert!(preflight.take_open(open).is_none());
        assert_eq!(controller.status(), &AppStatus::Recording);
    }

    #[test]
    fn assignment_waits_for_bounded_command_prefix_and_keeps_other_lanes_live() {
        let control = HotkeyControl::new(Preferences::default());
        let epoch = control.begin_assignment().unwrap();
        let mut assignment = AssignmentTracker::default();
        let mut preflight = HotkeyPreflight::default();
        let marker = preflight.marker();
        let selected = TriggerKey::FnGlobe;
        preflight.assignment = Some((
            marker,
            HotkeySignal::AssignmentSelected {
                trigger: selected,
                epoch,
            },
        ));
        let mut commands = VecDeque::from(vec![MenuCommand::SetAppendSpace(true); 40]);
        commands.push_back(MenuCommand::BeginTriggerAssignment { epoch });
        commands.push_back(MenuCommand::Boundary(marker));
        let mut cursor = 8; // hotkey lane can be first on this continuation
        let mut accepted = None;
        let mut other_results = 1;
        let mut passes = 0;
        loop {
            let more = drain_event_lanes(
                &mut cursor,
                |lane| match lane {
                    7 => {
                        let Some(command) = commands.pop_front() else {
                            return false;
                        };
                        match command {
                            MenuCommand::BeginTriggerAssignment { epoch } => {
                                assignment.begin(epoch)
                            }
                            MenuCommand::Boundary(id) if id == marker => {
                                let (_, signal) = preflight.assignment.take().unwrap();
                                if let HotkeySignal::AssignmentSelected { trigger, epoch } = signal
                                {
                                    accepted = assignment.accept_selection(
                                        trigger,
                                        epoch,
                                        &AppStatus::Ready,
                                    );
                                }
                            }
                            _ => {}
                        }
                        true
                    }
                    8 => {
                        assert!(preflight.assignment.is_some() || accepted.is_some());
                        false
                    }
                    10 if other_results != 0 => {
                        other_results -= 1;
                        true
                    }
                    _ => false,
                },
                || Duration::ZERO,
            );
            passes += 1;
            if passes == 1 {
                assert!(accepted.is_none());
                assert_eq!(other_results, 0);
            }
            if !more {
                break;
            }
        }
        assert_eq!(accepted, Some(selected));
        assert_eq!(passes, 2);
        assert!(assignment
            .accept_selection(selected, epoch, &AppStatus::Ready)
            .is_none());
        assert!(!assignment.accept_cancellation(epoch));
    }

    #[test]
    fn permission_backoff_keeps_late_grants_observable_and_healthy_idle_has_no_timer() {
        let mut retry = PermissionRetry::default();
        assert_eq!(
            (0..8)
                .map(|_| retry.next(true).unwrap())
                .collect::<Vec<_>>(),
            [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]
        );
        for _ in 0..100 {
            assert_eq!(retry.next(true), Some(30_000));
        }
        assert_eq!(retry.next(false), None);
        for _ in 0..100 {
            assert_eq!(retry.next(false), None);
        }
        retry.restart();
        assert_eq!(retry.next(true), Some(1_000));
        let mut controller = ready();
        let revoked = PermissionSnapshot {
            microphone: false,
            ..PermissionSnapshot::all()
        };
        // Runtime probes before sending TriggerPressed, and before ModelLoaded.
        controller.handle(AppEvent::PermissionsChanged(revoked));
        assert!(!capture_start_allowed(controller.status(), None, false));
        assert!(controller.handle(AppEvent::TriggerPressed).is_empty());
        controller.handle(AppEvent::AsrRecoveryStarted);
        controller.handle(AppEvent::PermissionsChanged(revoked));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        assert!(matches!(
            controller.status(),
            AppStatus::PermissionBlocked(_)
        ));
    }

    #[test]
    fn timer_tokens_defer_during_initialization_and_cancel_old_generation() {
        let source = EventSource::new(CFRunLoop::get_current());
        let queue = Rc::new(RefCell::new(VecDeque::new()));
        let old = ScheduledTimer::new(
            &CFRunLoop::get_current(),
            queue.clone(),
            source.notifier(),
            TimerKind::ResetError,
            0,
        );
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(10),
            true,
        );
        assert_eq!(
            queue.borrow().len(),
            1,
            "timer queues before source registration"
        );
        old.remove(&CFRunLoop::get_current());
        let timer = ScheduledTimer::new(
            &CFRunLoop::get_current(),
            queue.clone(),
            source.notifier(),
            TimerKind::FinishCapture,
            0,
        );
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        let pending = queue.clone();
        source.set_handler(move || {
            while let Some(event) = pending.borrow_mut().pop_front() {
                if event.active.get() {
                    assert_eq!(event.kind, TimerKind::FinishCapture);
                    seen.set(seen.get() + 1);
                }
            }
        });
        source.attach();
        let deadline = Instant::now() + Duration::from_secs(1);
        while calls.get() == 0 {
            assert!(Instant::now() < deadline);
            CFRunLoop::run_in_mode(
                unsafe { kCFRunLoopDefaultMode },
                Duration::from_millis(10),
                true,
            );
        }
        assert_eq!(calls.get(), 1);
        timer.remove(&CFRunLoop::get_current());
        source.close();
    }

    #[test]
    fn guarded_owner_step_can_pump_timers_without_reborrowing_pointee() {
        let source = EventSource::new(CFRunLoop::get_current());
        let mut owner = Box::pin(0usize);
        let pointer = &mut *owner as *mut usize;
        source.set_handler(move || unsafe {
            *pointer += 1;
        });
        source.attach();
        pump_until_cleanup_guarded(
            &mut owner,
            || source.suspend(),
            |probe| {
                if *probe == 1 {
                    return true;
                }
                // Deliberately pump the source inside the exclusive step access.
                // The callback is deferred before it constructs another reference.
                CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, Duration::ZERO, true);
                assert_eq!(*probe, 0);
                false
            },
            || {
                CFRunLoop::run_in_mode(
                    unsafe { kCFRunLoopDefaultMode },
                    Duration::from_millis(10),
                    true,
                );
            },
        );
        assert_eq!(*owner, 1);
        source.close();
    }
}

#[cfg(test)]
mod producer_wake_tests {
    use super::*;
    use crate::event_wake::tests::pump_until;

    #[test]
    fn microphone_completion_reaches_flow_and_late_callback_is_inert() {
        struct Boundary;
        impl MicrophonePermissionBoundary for Boundary {
            fn request_access(&mut self) -> bool {
                true
            }
            fn open_settings(&mut self) -> bool {
                true
            }
        }
        let source = EventSource::new(CFRunLoop::get_current());
        let mut microphone = MicrophonePermissionRuntime::default();
        let system = SystemMicrophonePermissionBoundary {
            notifier: source.notifier(),
            completion_sender: microphone.completion_sender(),
        };
        let first = system.completion();
        let late = system.completion();
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        source.set_handler(move || {
            if microphone.drain_completions(|| MicrophoneAuthorization::Authorized, &mut Boundary) {
                seen.set(seen.get() + 1);
            }
        });
        source.attach();
        CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::ZERO,
            true,
        );
        thread::spawn(first).join().unwrap();
        pump_until(|| calls.get() == 1);
        source.close();
        thread::spawn(late).join().unwrap();
        CFRunLoop::run_in_mode(
            unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
            Duration::ZERO,
            true,
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn model_and_migration_success_or_panic_are_observed_without_periodic_poll() {
        for panics in [false, true] {
            let source = EventSource::new(CFRunLoop::get_current());
            let (release, blocked) = mpsc::channel();
            let (events, worker) = spawn_model_preparation_notified(
                move || {
                    blocked.recv().unwrap();
                    if panics {
                        panic!("synthetic model preparation panic");
                    }
                    Err(ModelStoreError::RepairRequired)
                },
                source.notifier(),
            );
            let seen = Rc::new(Cell::new(false));
            let result = seen.clone();
            source.set_handler(move || match events.try_recv() {
                Ok(Err(ModelStoreError::RepairRequired)) if !panics => result.set(true),
                Err(TryRecvError::Disconnected) if panics => result.set(true),
                Err(TryRecvError::Empty) => {}
                other => panic!("unexpected model completion: {other:?}"),
            });
            source.attach();
            CFRunLoop::run_in_mode(
                unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                Duration::ZERO,
                true,
            );
            assert!(!seen.get());
            release.send(()).unwrap();
            pump_until(|| seen.get());
            assert_eq!(worker.join().is_err(), panics);
            source.close();

            let source = EventSource::new(CFRunLoop::get_current());
            let (release, blocked) = mpsc::channel();
            let paths = ModelPaths::from_verified_directory(std::path::Path::new(
                "/synthetic-unused-model",
            ));
            let (events, worker) = spawn_permission_migration_notified(
                paths,
                move || {
                    blocked.recv().unwrap();
                    if panics {
                        panic!("synthetic migration panic");
                    }
                    Ok(PermissionMigrationSuccess::DevelopmentBypass)
                },
                source.notifier(),
            );
            let seen = Rc::new(Cell::new(false));
            let result = seen.clone();
            source.set_handler(move || match events.try_recv() {
                Ok(result_value) if !panics => {
                    assert!(matches!(
                        result_value.migration,
                        Ok(PermissionMigrationSuccess::DevelopmentBypass)
                    ));
                    result.set(true);
                }
                Err(TryRecvError::Disconnected) if panics => result.set(true),
                Err(TryRecvError::Empty) => {}
                _ => panic!("unexpected migration completion"),
            });
            source.attach();
            CFRunLoop::run_in_mode(
                unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                Duration::ZERO,
                true,
            );
            assert!(!seen.get());
            release.send(()).unwrap();
            pump_until(|| seen.get());
            assert_eq!(worker.join().is_err(), panics);
        }
    }

    #[test]
    fn completion_watchdog_backoff_is_pending_only_and_keeps_exact_operation_deadline() {
        let now = Instant::now();
        let mut delay = 10;
        assert_eq!(asr_wake_deadline(now, None, false, None, &mut delay), None);
        let operation = now + Duration::from_secs(60);
        assert_eq!(
            asr_wake_deadline(now, Some(operation), false, None, &mut delay),
            Some(operation)
        );
        assert_eq!(
            asr_wake_deadline(now, None, false, Some(operation), &mut delay),
            None
        );
        let first = asr_wake_deadline(now, None, true, None, &mut delay).unwrap();
        assert_eq!(first, now + Duration::from_millis(10));
        assert_eq!(
            asr_wake_deadline(now, None, true, Some(first), &mut delay),
            Some(first)
        );
        assert_eq!(delay, 20);
        let mut instant = first;
        for _ in 0..20 {
            let next = asr_wake_deadline(instant, None, true, None, &mut delay).unwrap();
            assert!(next > instant);
            assert!(next <= instant + Duration::from_secs(1));
            instant = next;
        }
        assert_eq!(
            asr_wake_deadline(instant, None, false, Some(instant), &mut delay),
            None
        );
        assert_eq!(delay, 10);
    }

    #[test]
    fn nested_timer_is_queued_once_and_never_calls_runtime_during_outer_access() {
        let source = Rc::new(EventSource::new(CFRunLoop::get_current()));
        let queue = Rc::new(RefCell::new(VecDeque::new()));
        let mut timer = Some(ScheduledTimer::new(
            &CFRunLoop::get_current(),
            queue.clone(),
            source.notifier(),
            TimerKind::ResetError,
            60_000,
        ));
        let context = timer.as_ref().unwrap().context.clone();
        let weak = Rc::downgrade(&source);
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        source.set_handler(move || {
            seen.set(seen.get() + 1);
            if seen.get() == 1 {
                for _ in 0..3 {
                    timer_fired(std::ptr::null_mut(), Rc::as_ptr(&context).cast_mut().cast());
                }
                assert_eq!(queue.borrow().len(), 1);
                // Nested source entry is deferred before handler/Runtime borrow.
                let source = weak.upgrade().unwrap();
                CFRunLoop::run_in_mode(
                    unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                    Duration::ZERO,
                    true,
                );
                assert_eq!(seen.get(), 1);
                source.notifier().notify();
            } else {
                let queued = queue.borrow_mut().pop_front().unwrap();
                assert!(queued.active.get());
                cancel_timer(&CFRunLoop::get_current(), &mut timer);
                assert!(!queued.active.get());
                assert!(queue.borrow().is_empty());
            }
        });
        source.attach();
        pump_until(|| calls.get() == 2);
        source.close();
    }
}
