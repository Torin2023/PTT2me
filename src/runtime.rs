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
use objc2_foundation::MainThreadMarker;

use crate::asr::{spawn_asr_worker, AsrCommand, AsrEvent};
use crate::audio::{AudioError, AudioRecorder};
use crate::constants::{MAX_CAPTURE_MS, RELEASE_GRACE_MS};
use crate::hotkey::{HotkeyListener, HotkeySignal};
use crate::inserter::{
    InsertError, PendingInsertion, PASTEBOARD_RESTORE_DELAY_MS, PASTEBOARD_SETTLE_DELAY_MS,
};
use crate::menu::MenuBar;
use crate::model::{resources_dir_from_executable, ModelPaths};
use crate::permissions::{
    self, MicrophoneAuthorization, MicrophonePermissionBoundary, MicrophonePermissionFlow,
    SystemPermissionProbe,
};
use crate::state::{AppController, AppEvent, AppStatus, Effect, PermissionSnapshot};
use crate::text_inserter::{self, InsertMethod, InsertOutcome};

const EVENT_DRAIN_MS: u64 = 50;
const PERMISSION_POLL_MS: u64 = 1_000;
const SMOKE_MODEL_TIMEOUT: Duration = Duration::from_secs(180);
const SMOKE_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SMOKE_TIMEOUT_EXIT_CODE: i32 = 124;

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

const fn is_dictation_in_flight(status: &AppStatus) -> bool {
    matches!(status, AppStatus::Recording | AppStatus::Recognizing)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerKind {
    DrainEvents,
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

impl PasteInsertion for PendingInsertion {
    fn paste(&mut self) -> Result<(), InsertError> {
        PendingInsertion::paste(self)
    }

    fn restore(&mut self) -> Result<(), InsertError> {
        PendingInsertion::restore(self)
    }

    fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
        PendingInsertion::restore_after_paste_failure(self, primary)
    }
}

trait PasteFlowBoundary {
    fn schedule(&mut self, kind: TimerKind, delay_ms: u64);
    fn finish_and_drain_hotkeys(&mut self, result: Result<(), String>);
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
                        let _ = self.insertion.restore_after_paste_failure(primary);
                        self.state = PasteFlowState::Finished;
                        boundary.finish_and_drain_hotkeys(Err("insert failed".to_owned()));
                    }
                }
            }
            (PasteFlowState::AwaitingRestore, TimerKind::RestorePasteboard) => {
                let result = self
                    .insertion
                    .restore()
                    .map_err(|_| "insert failed".to_owned());
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
    recorder: AudioRecorder,
    hotkey: Option<HotkeyListener>,
    hotkey_sender: Sender<HotkeySignal>,
    hotkey_events: Receiver<HotkeySignal>,
    asr_commands: Sender<AsrCommand>,
    asr_events: Receiver<AsrEvent>,
    asr_worker: Option<JoinHandle<()>>,
    asr_connected: bool,
    run_loop: CFRunLoop,
    drain_timer: Option<ScheduledTimer>,
    permission_timer: Option<ScheduledTimer>,
    finish_timer: Option<ScheduledTimer>,
    capture_limit_timer: Option<ScheduledTimer>,
    insertion_timer: Option<ScheduledTimer>,
    error_timer: Option<ScheduledTimer>,
    pending_insertion: Option<PasteFlow<PendingInsertion>>,
    press_started: Option<Instant>,
    applied_permissions: PermissionSnapshot,
    microphone_permissions: MicrophonePermissionRuntime,
    tap_needs_retry: bool,
    deferred_tap_state: DeferredTapState,
    _pin: PhantomPinned,
}

impl Runtime {
    /// Creates and starts the complete runtime. The returned box must remain
    /// alive until `NSApplication::run` returns.
    pub fn start(_mtm: MainThreadMarker) -> Pin<Box<Self>> {
        let (hotkey_sender, hotkey_events) = mpsc::channel();
        let (asr_commands, asr_command_receiver) = mpsc::channel();
        let (asr_event_sender, asr_events) = mpsc::channel();
        let asr_worker = spawn_asr_worker(asr_command_receiver, asr_event_sender);

        let mut runtime = Box::pin(Self {
            controller: AppController::new(),
            menu: MenuBar::new(),
            recorder: AudioRecorder::new(),
            hotkey: None,
            hotkey_sender,
            hotkey_events,
            asr_commands,
            asr_events,
            asr_worker: Some(asr_worker),
            asr_connected: true,
            run_loop: CFRunLoop::get_main(),
            drain_timer: None,
            permission_timer: None,
            finish_timer: None,
            capture_limit_timer: None,
            insertion_timer: None,
            error_timer: None,
            pending_insertion: None,
            press_started: None,
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
        runtime_ref.begin_model_load();
        runtime_ref.poll_permissions();
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
        self.permission_timer = Some(ScheduledTimer::new(
            &self.run_loop,
            runtime,
            TimerKind::PollPermissions,
            PERMISSION_POLL_MS,
            Some(PERMISSION_POLL_MS),
        ));
    }

    fn begin_model_load(&mut self) {
        let paths = std::env::current_exe()
            .map_err(|_| "current executable unavailable".to_owned())
            .and_then(|executable| resources_dir_from_executable(&executable))
            .and_then(|resources| ModelPaths::from_resources(&resources));

        match paths {
            Ok(paths) => {
                if self.asr_commands.send(AsrCommand::Load(paths)).is_err() {
                    self.dispatch(AppEvent::ModelLoaded(Err(
                        "ASR worker unavailable".to_owned()
                    )));
                }
            }
            Err(error) => self.dispatch(AppEvent::ModelLoaded(Err(error))),
        }
    }

    fn handle_timer(&mut self, kind: TimerKind) {
        match kind {
            TimerKind::DrainEvents => self.drain_events(),
            TimerKind::PollPermissions => self.poll_permissions(),
            TimerKind::FinishCapture => self.finish_capture(),
            TimerKind::CaptureLimit => {
                self.press_started = None;
                self.dispatch(AppEvent::CaptureLimitReached);
            }
            TimerKind::PasteCommand | TimerKind::RestorePasteboard => {
                self.advance_pending_paste(kind);
            }
            TimerKind::ResetError => self.dispatch(AppEvent::ErrorTimerFired),
        }
    }

    fn drain_events(&mut self) {
        self.drain_microphone_permission_completions();

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
    }

    fn drain_hotkey_events(&mut self) {
        let hotkey_events: Vec<_> = self.hotkey_events.try_iter().collect();
        for signal in hotkey_events {
            self.handle_hotkey(signal);
        }
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
            HotkeySignal::Pressed { observed_at } => {
                if self.controller.status() == &AppStatus::Ready {
                    self.press_started = Some(observed_at);
                }
                self.dispatch(AppEvent::FnPressed);
            }
            HotkeySignal::Released { observed_at } => {
                let held_ms = self
                    .press_started
                    .take()
                    .map(|started| held_millis(started, observed_at))
                    .unwrap_or(0);
                self.dispatch(AppEvent::FnReleased { held_ms });
            }
            HotkeySignal::TapLost => {
                self.tap_needs_retry = true;
                self.observe_tap_state(TapState::Lost);
            }
            HotkeySignal::TapRestored => {
                self.tap_needs_retry = false;
                self.observe_tap_state(TapState::Restored);
            }
        }
    }

    fn poll_permissions(&mut self) {
        let permissions = SystemPermissionProbe::check();
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
            self.hotkey.take();
            return;
        }

        if self.tap_needs_retry {
            self.hotkey.take();
        }
        if self.hotkey.is_none() {
            match HotkeyListener::install(self.hotkey_sender.clone()) {
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
        self.menu.render(self.controller.status());
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
            Effect::StartCapture => match self.recorder.start() {
                Ok(()) => {
                    self.replace_capture_limit_timer(MAX_CAPTURE_MS);
                    tracing::debug!(lifecycle = "capture_started");
                }
                Err(_) => {
                    self.press_started = None;
                    tracing::warn!(error_category = "microphone_start");
                    self.dispatch(AppEvent::CaptureFailed);
                }
            },
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
            Effect::InsertText(text) => match text_inserter::begin(&text) {
                Ok(InsertOutcome::Complete(method)) => {
                    let method = match method {
                        InsertMethod::Accessibility => "accessibility",
                        InsertMethod::UnicodeEvents => "unicode_events",
                    };
                    tracing::debug!(method, lifecycle = "text_inserted");
                    self.dispatch(AppEvent::PasteFinished(Ok(())));
                }
                Ok(InsertOutcome::PendingClipboard(insertion)) => {
                    let flow = PasteFlow::begin(insertion, self);
                    self.pending_insertion = Some(flow);
                }
                Err(_) => {
                    tracing::warn!(error_category = "text_insertion");
                    self.dispatch(AppEvent::PasteFinished(Err("insert failed".to_owned())));
                }
            },
            Effect::ScheduleErrorReset { delay_ms } => {
                self.replace_error_timer(delay_ms);
            }
        }
    }

    fn finish_capture(&mut self) {
        self.press_started = None;
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

impl PasteFlowBoundary for Runtime {
    fn schedule(&mut self, kind: TimerKind, delay_ms: u64) {
        self.replace_insertion_timer(kind, delay_ms);
    }

    fn finish_and_drain_hotkeys(&mut self, result: Result<(), String>) {
        cancel_timer(&self.run_loop, &mut self.insertion_timer);
        if result.is_err() {
            tracing::warn!(error_category = "text_insertion");
        }
        self.dispatch(AppEvent::PasteFinished(result));
        self.drain_hotkey_events();
    }
}

fn held_millis(pressed_at: Instant, released_at: Instant) -> u64 {
    let duration = released_at
        .checked_duration_since(pressed_at)
        .unwrap_or_default();
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
        AppStatus::Starting => "starting",
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
    ModelPaths::from_resources(&resources)
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
    use std::rc::Rc;

    use super::{
        held_millis, milliseconds_to_seconds, status_name, wait_for_smoke_child, DeferredTapState,
        MicrophonePermissionRuntime, PasteFlow, PasteFlowBoundary, PasteInsertion, TapState,
        TimerKind, EVENT_DRAIN_MS, PERMISSION_POLL_MS,
    };
    use crate::constants::{ERROR_VISIBLE_MS, MAX_CAPTURE_MS, RELEASE_GRACE_MS};
    use crate::inserter::InsertError;
    use crate::permissions::{MicrophoneAuthorization, MicrophonePermissionBoundary};
    use crate::state::{AppController, AppEvent, AppStatus, Effect, PermissionSnapshot};

    struct RecordingMicrophoneBoundary {
        events: Rc<RefCell<Vec<&'static str>>>,
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

    fn recognizing_controller() -> AppController {
        let mut controller = AppController::new();
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        controller.handle(AppEvent::FnPressed);
        controller.handle(AppEvent::FnReleased { held_ms: 900 });
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
    fn delayed_hotkey_drain_uses_callback_times_for_hold_duration() {
        let pressed_at = std::time::Instant::now();
        let released_at = pressed_at + std::time::Duration::from_millis(900);

        assert_eq!(held_millis(pressed_at, released_at), 900);
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

            fn finish_and_drain_hotkeys(&mut self, result: Result<(), String>) {
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

            fn finish_and_drain_hotkeys(&mut self, _result: Result<(), String>) {
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
        assert!(controller.handle(AppEvent::FnPressed).is_empty());
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
            controller.handle(AppEvent::FnPressed),
            vec![Effect::StartCapture]
        );
    }

    #[test]
    fn unresolved_tap_loss_blocks_new_cycle_after_old_result_completes() {
        let mut controller = recognizing_controller();
        let mut tap = DeferredTapState::default();

        assert_eq!(tap.observe(controller.status(), TapState::Lost), None);
        assert!(controller.handle(AppEvent::FnPressed).is_empty());
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
        assert!(controller.handle(AppEvent::FnPressed).is_empty());

        let restored = tap
            .observe(controller.status(), TapState::Restored)
            .unwrap();
        controller.handle(restored);
        assert_eq!(
            controller.handle(AppEvent::FnPressed),
            vec![Effect::StartCapture]
        );
    }
}
