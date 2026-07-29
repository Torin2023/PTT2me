use core_foundation::{
    base::TCFType,
    mach_port::{CFMachPort, CFMachPortInvalidate, CFMachPortRef},
    runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopSource, CFRunLoopSourceInvalidate},
};
use core_graphics::{
    event::{
        CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType, EventField,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
    sys::CGEventRef,
};
use std::{
    error::Error,
    ffi::c_void,
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::{null_mut, NonNull},
    sync::{
        atomic::{AtomicPtr, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
    time::Instant,
};

use crate::preferences::{HoldThreshold, Preferences, TriggerKey};

const FN_KEYCODE: u16 = 63;
const GLOBE_KEYCODE: u16 = 179;
const SECONDARY_FN_FLAG: u64 = 0x0080_0000;
const REPLAY_MARKER: i64 = 0x5054_5432_4D45;
const KEYBOARD_EVENT_MASK: CGEventMask = (1 << CGEventType::KeyDown as CGEventMask)
    | (1 << CGEventType::KeyUp as CGEventMask)
    | (1 << CGEventType::FlagsChanged as CGEventMask);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotkeySignal {
    Pressed,
    Released {
        short: bool,
    },
    Cancelled,
    AssignmentSelected {
        trigger: TriggerKey,
        epoch: AssignmentEpoch,
    },
    AssignmentCancelled {
        epoch: AssignmentEpoch,
    },
    TapLost,
    TapRestored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssignmentEpoch(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationKind {
    KeyDown,
    KeyUp,
    FlagsChanged,
    TapDisabledByTimeout,
    TapDisabledByUserInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardObservation {
    pub kind: ObservationKind,
    pub keycode: u16,
    pub flags: u64,
    pub autorepeat: bool,
    pub replay_marker: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventDisposition {
    Pass,
    Suppress,
}

struct InputGate {
    preferences: Preferences,
    mode: GateMode,
    modifier_state: ModifierState,
    next_assignment_epoch: u64,
}

#[derive(Clone)]
pub struct HotkeyControl {
    gate: Arc<Mutex<InputGate>>,
}

impl HotkeyControl {
    pub fn new(preferences: Preferences) -> Self {
        Self {
            gate: Arc::new(Mutex::new(InputGate::new(preferences))),
        }
    }

    pub fn set_preferences(&self, preferences: Preferences) {
        self.with_gate(|gate| gate.set_preferences(preferences));
    }

    pub fn set_trigger(&self, trigger: TriggerKey) {
        self.with_gate(|gate| {
            let mut preferences = gate.preferences;
            preferences.trigger = trigger;
            gate.set_preferences(preferences);
        });
    }

    pub fn set_threshold(&self, threshold: HoldThreshold) {
        self.with_gate(|gate| {
            let mut preferences = gate.preferences;
            preferences.threshold = threshold;
            gate.set_preferences(preferences);
        });
    }

    pub fn begin_assignment(&self) -> Option<AssignmentEpoch> {
        self.with_gate(InputGate::begin_assignment)
    }

    pub fn current_assignment_epoch(&self) -> Option<AssignmentEpoch> {
        self.with_gate(|gate| gate.current_assignment_epoch())
    }

    pub fn cancel_assignment(&self, epoch: AssignmentEpoch) -> bool {
        self.with_gate(|gate| gate.cancel_assignment(epoch))
    }

    pub fn cancel_current_assignment(&self) -> bool {
        self.with_gate(|gate| {
            gate.current_assignment_epoch()
                .is_some_and(|epoch| gate.cancel_assignment(epoch))
        })
    }

    pub fn reset_for_listener_removal(&self) -> bool {
        self.with_gate(InputGate::reset_for_listener_removal)
    }

    pub(crate) fn guard_pending_release_after_capture_failure(&self) {
        self.with_gate(InputGate::guard_pending_release_after_capture_failure);
    }

    fn handle(&self, observation: KeyboardObservation, now: Instant) -> GateDecision {
        self.with_gate(|gate| gate.handle(observation, now))
    }

    fn with_gate<T>(&self, operation: impl FnOnce(&mut InputGate) -> T) -> T {
        let mut gate = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation(&mut gate)
    }

    #[cfg(test)]
    pub(crate) fn observe_for_test(
        &self,
        observation: KeyboardObservation,
        now: Instant,
    ) -> Option<HotkeySignal> {
        self.handle(observation, now).signal
    }

    #[cfg(test)]
    pub(crate) fn suppresses_for_test(
        &self,
        observation: KeyboardObservation,
        now: Instant,
    ) -> bool {
        self.handle(observation, now).disposition == EventDisposition::Suppress
    }

    #[cfg(test)]
    pub(crate) fn observe_outcome_for_test(
        &self,
        observation: KeyboardObservation,
        now: Instant,
    ) -> (bool, Option<HotkeySignal>, usize) {
        let decision = self.handle(observation, now);
        (
            decision.disposition == EventDisposition::Suppress,
            decision.signal,
            decision.replay.len(),
        )
    }
}

struct ModifierState {
    pressed: [bool; 256],
}

impl Default for ModifierState {
    fn default() -> Self {
        Self {
            pressed: [false; 256],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservationEdge {
    Press,
    Release,
    None,
}

enum GateMode {
    Idle,
    Pending {
        physical_keycode: u16,
        pressed_at: Instant,
        threshold: HoldThreshold,
        down: ReplayEvent,
    },
    Combination {
        physical_keycode: u16,
    },
    Assigning {
        epoch: AssignmentEpoch,
    },
    AssignmentConsumed {
        physical_keycode: u16,
        epoch: AssignmentEpoch,
    },
    AssignmentReleaseGuard {
        physical_keycode: u16,
    },
    CaptureFailureReleaseGuard {
        physical_keycode: u16,
    },
}

struct PendingPress {
    physical_keycode: u16,
    pressed_at: Instant,
    threshold: HoldThreshold,
    down: ReplayEvent,
}

struct GateDecision {
    disposition: EventDisposition,
    signal: Option<HotkeySignal>,
    replay: Vec<ReplayEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReplayEvent {
    kind: ObservationKind,
    keycode: u16,
    flags: u64,
}

impl ReplayEvent {
    fn event_type(self) -> CGEventType {
        match self.kind {
            ObservationKind::KeyDown => CGEventType::KeyDown,
            ObservationKind::KeyUp => CGEventType::KeyUp,
            ObservationKind::FlagsChanged => CGEventType::FlagsChanged,
            ObservationKind::TapDisabledByTimeout | ObservationKind::TapDisabledByUserInput => {
                CGEventType::Null
            }
        }
    }
}

trait ReplayBackend {
    type Prepared;

    fn prepare(&mut self, replay: ReplayEvent) -> Result<Self::Prepared, ()>;
    fn post(&mut self, prepared: Self::Prepared, proxy: CGEventTapProxy);
}

struct CoreGraphicsReplayBackend;

impl ReplayBackend for CoreGraphicsReplayBackend {
    type Prepared = CGEvent;

    fn prepare(&mut self, replay: ReplayEvent) -> Result<Self::Prepared, ()> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;
        let event = CGEvent::new_keyboard_event(
            source,
            replay.keycode,
            !matches!(replay.kind, ObservationKind::KeyUp),
        )?;
        event.set_type(replay.event_type());
        event.set_flags(CGEventFlags::from_bits_retain(replay.flags));
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, REPLAY_MARKER);
        Ok(event)
    }

    fn post(&mut self, prepared: Self::Prepared, proxy: CGEventTapProxy) {
        prepared.post_from_tap(proxy);
    }
}

fn post_replay_batch<B: ReplayBackend>(
    replay: &[ReplayEvent],
    proxy: CGEventTapProxy,
    backend: &mut B,
) -> Result<(), ()> {
    let prepared = replay
        .iter()
        .copied()
        .map(|event| backend.prepare(event))
        .collect::<Result<Vec<_>, _>>()?;
    for event in prepared {
        backend.post(event, proxy);
    }
    Ok(())
}

impl GateDecision {
    fn pass() -> Self {
        Self {
            disposition: EventDisposition::Pass,
            signal: None,
            replay: Vec::new(),
        }
    }

    fn suppress(signal: Option<HotkeySignal>) -> Self {
        Self {
            disposition: EventDisposition::Suppress,
            signal,
            replay: Vec::new(),
        }
    }
}

impl InputGate {
    fn new(preferences: Preferences) -> Self {
        Self {
            preferences,
            mode: GateMode::Idle,
            modifier_state: ModifierState::default(),
            next_assignment_epoch: 0,
        }
    }

    fn set_preferences(&mut self, preferences: Preferences) {
        self.preferences = preferences;
    }

    #[cfg(test)]
    fn preferences(&self) -> Preferences {
        self.preferences
    }

    fn begin_assignment(&mut self) -> Option<AssignmentEpoch> {
        if !matches!(self.mode, GateMode::Idle) {
            return None;
        }
        self.next_assignment_epoch = self.next_assignment_epoch.wrapping_add(1);
        let epoch = AssignmentEpoch(self.next_assignment_epoch);
        self.mode = GateMode::Assigning { epoch };
        Some(epoch)
    }

    fn current_assignment_epoch(&self) -> Option<AssignmentEpoch> {
        match self.mode {
            GateMode::Assigning { epoch } | GateMode::AssignmentConsumed { epoch, .. } => {
                Some(epoch)
            }
            GateMode::Idle
            | GateMode::Pending { .. }
            | GateMode::Combination { .. }
            | GateMode::AssignmentReleaseGuard { .. }
            | GateMode::CaptureFailureReleaseGuard { .. } => None,
        }
    }

    fn cancel_assignment(&mut self, epoch: AssignmentEpoch) -> bool {
        match self.mode {
            GateMode::Assigning { epoch: active } if active == epoch => {
                self.mode = GateMode::Idle;
                true
            }
            GateMode::AssignmentConsumed {
                physical_keycode,
                epoch: active,
            } if active == epoch => {
                self.mode = GateMode::AssignmentReleaseGuard { physical_keycode };
                true
            }
            GateMode::Idle
            | GateMode::Pending { .. }
            | GateMode::Combination { .. }
            | GateMode::Assigning { .. }
            | GateMode::AssignmentConsumed { .. }
            | GateMode::AssignmentReleaseGuard { .. }
            | GateMode::CaptureFailureReleaseGuard { .. } => false,
        }
    }

    fn guard_pending_release_after_capture_failure(&mut self) {
        if let GateMode::Pending {
            physical_keycode, ..
        } = self.mode
        {
            self.mode = GateMode::CaptureFailureReleaseGuard { physical_keycode };
        }
    }

    fn reset_for_listener_removal(&mut self) -> bool {
        let had_pending_press = matches!(self.mode, GateMode::Pending { .. });
        self.mode = GateMode::Idle;
        self.modifier_state.clear();
        had_pending_press
    }

    fn handle(&mut self, observation: KeyboardObservation, now: Instant) -> GateDecision {
        if observation.replay_marker {
            return GateDecision::pass();
        }
        if matches!(
            observation.kind,
            ObservationKind::TapDisabledByTimeout | ObservationKind::TapDisabledByUserInput
        ) {
            let signal =
                matches!(self.mode, GateMode::Pending { .. }).then_some(HotkeySignal::Cancelled);
            self.mode = GateMode::Idle;
            self.modifier_state.clear();
            return GateDecision::suppress(signal);
        }
        let edge = self.modifier_state.observe(observation);

        match self.mode {
            GateMode::Idle => self.handle_idle(observation, edge, now),
            GateMode::Pending {
                physical_keycode,
                pressed_at,
                threshold,
                down,
            } => self.handle_pending(
                observation,
                now,
                edge,
                PendingPress {
                    physical_keycode,
                    pressed_at,
                    threshold,
                    down,
                },
            ),
            GateMode::Combination { physical_keycode } => {
                if observation.is_physical_release(physical_keycode, edge) {
                    self.mode = GateMode::Idle;
                }
                GateDecision::pass()
            }
            GateMode::Assigning { epoch } => self.handle_assigning(observation, edge, epoch),
            GateMode::AssignmentConsumed {
                physical_keycode, ..
            }
            | GateMode::AssignmentReleaseGuard { physical_keycode }
            | GateMode::CaptureFailureReleaseGuard { physical_keycode } => {
                if observation.is_physical_release(physical_keycode, edge) {
                    self.mode = GateMode::Idle;
                    GateDecision::suppress(None)
                } else if observation.keycode == physical_keycode
                    && (observation.autorepeat || edge == ObservationEdge::Press)
                {
                    GateDecision::suppress(None)
                } else {
                    GateDecision::pass()
                }
            }
        }
    }

    fn handle_idle(
        &mut self,
        observation: KeyboardObservation,
        edge: ObservationEdge,
        now: Instant,
    ) -> GateDecision {
        if !observation.is_trigger_press(self.preferences.trigger, edge) {
            return GateDecision::pass();
        }

        if observation.autorepeat {
            return GateDecision::suppress(None);
        }

        self.mode = GateMode::Pending {
            physical_keycode: observation.keycode,
            pressed_at: now,
            threshold: self.preferences.threshold,
            down: observation.into_replay(),
        };
        GateDecision::suppress(Some(HotkeySignal::Pressed))
    }

    fn handle_pending(
        &mut self,
        observation: KeyboardObservation,
        now: Instant,
        edge: ObservationEdge,
        pending: PendingPress,
    ) -> GateDecision {
        if observation.is_physical_release(pending.physical_keycode, edge) {
            self.mode = GateMode::Idle;
            let short = now.duration_since(pending.pressed_at).as_millis()
                < u128::from(pending.threshold.millis());
            let replay = if short {
                vec![pending.down, observation.into_replay()]
            } else {
                Vec::new()
            };
            return GateDecision {
                disposition: EventDisposition::Suppress,
                signal: Some(HotkeySignal::Released { short }),
                replay,
            };
        }

        if observation.keycode != pending.physical_keycode && edge == ObservationEdge::Press {
            self.mode = GateMode::Combination {
                physical_keycode: pending.physical_keycode,
            };
            return GateDecision {
                disposition: EventDisposition::Suppress,
                signal: Some(HotkeySignal::Cancelled),
                replay: vec![pending.down, observation.into_replay()],
            };
        }

        if observation.keycode != pending.physical_keycode {
            return GateDecision::pass();
        }

        if observation.autorepeat || edge == ObservationEdge::Press {
            return GateDecision::suppress(None);
        }

        GateDecision::pass()
    }

    fn handle_assigning(
        &mut self,
        observation: KeyboardObservation,
        edge: ObservationEdge,
        epoch: AssignmentEpoch,
    ) -> GateDecision {
        if edge != ObservationEdge::Press {
            return GateDecision::pass();
        }

        if observation.keycode == 53 {
            self.mode = GateMode::AssignmentConsumed {
                physical_keycode: observation.keycode,
                epoch,
            };
            return GateDecision::suppress(Some(HotkeySignal::AssignmentCancelled { epoch }));
        }

        let selected = if observation.is_fn_or_globe() {
            Some(TriggerKey::FnGlobe)
        } else {
            TriggerKey::from_keycode(observation.keycode)
        };
        let Some(selected) = selected else {
            self.mode = GateMode::Idle;
            return GateDecision {
                disposition: EventDisposition::Pass,
                signal: Some(HotkeySignal::AssignmentCancelled { epoch }),
                replay: Vec::new(),
            };
        };

        self.mode = GateMode::AssignmentConsumed {
            physical_keycode: observation.keycode,
            epoch,
        };
        GateDecision::suppress(Some(HotkeySignal::AssignmentSelected {
            trigger: selected,
            epoch,
        }))
    }
}

impl KeyboardObservation {
    fn is_fn_or_globe(self) -> bool {
        matches!(self.keycode, FN_KEYCODE | GLOBE_KEYCODE)
    }

    fn is_trigger_press(self, trigger: TriggerKey, edge: ObservationEdge) -> bool {
        let matches_trigger = match trigger {
            TriggerKey::FnGlobe => self.is_fn_or_globe(),
            TriggerKey::KeyCode(keycode) => self.keycode == keycode,
        };
        matches_trigger && edge == ObservationEdge::Press
    }

    fn is_physical_release(self, physical_keycode: u16, edge: ObservationEdge) -> bool {
        self.keycode == physical_keycode && edge == ObservationEdge::Release
    }

    fn modifier_flag_is_set(self) -> bool {
        modifier_flag(self.keycode).is_some_and(|flag| self.flags & flag != 0)
            || (self.is_fn_or_globe() && self.flags & SECONDARY_FN_FLAG != 0)
    }

    fn into_replay(self) -> ReplayEvent {
        ReplayEvent {
            kind: self.kind,
            keycode: self.keycode,
            flags: self.flags,
        }
    }
}

impl ModifierState {
    fn observe(&mut self, observation: KeyboardObservation) -> ObservationEdge {
        let is_modifier = observation.is_modifier();
        match observation.kind {
            ObservationKind::KeyDown => {
                if is_modifier {
                    self.pressed[usize::from(observation.keycode)] = true;
                }
                ObservationEdge::Press
            }
            ObservationKind::KeyUp => {
                if is_modifier {
                    self.pressed[usize::from(observation.keycode)] = false;
                }
                ObservationEdge::Release
            }
            ObservationKind::FlagsChanged if is_modifier => {
                let pressed = &mut self.pressed[usize::from(observation.keycode)];
                if let Some(flag) = physical_modifier_flag(observation.keycode) {
                    *pressed = observation.flags & flag != 0;
                    if *pressed {
                        ObservationEdge::Press
                    } else {
                        ObservationEdge::Release
                    }
                } else if *pressed {
                    *pressed = false;
                    ObservationEdge::Release
                } else if observation.modifier_flag_is_set() {
                    *pressed = true;
                    ObservationEdge::Press
                } else {
                    ObservationEdge::Release
                }
            }
            ObservationKind::FlagsChanged
            | ObservationKind::TapDisabledByTimeout
            | ObservationKind::TapDisabledByUserInput => ObservationEdge::None,
        }
    }

    fn clear(&mut self) {
        self.pressed.fill(false);
    }
}

impl KeyboardObservation {
    fn is_modifier(self) -> bool {
        modifier_flag(self.keycode).is_some() || self.is_fn_or_globe()
    }
}

fn modifier_flag(keycode: u16) -> Option<u64> {
    Some(match keycode {
        57 => 0x0001_0000,
        54 | 55 => 0x0010_0000,
        56 | 60 => 0x0002_0000,
        58 | 61 => 0x0008_0000,
        59 | 62 => 0x0004_0000,
        _ => return None,
    })
}

// Device-dependent bits from IOKit/hidsystem/IOLLEvent.h. Unlike the
// aggregate flags above, these preserve the physical left/right key state.
fn physical_modifier_flag(keycode: u16) -> Option<u64> {
    Some(match keycode {
        59 => 0x0000_0001,
        56 => 0x0000_0002,
        60 => 0x0000_0004,
        55 => 0x0000_0008,
        54 => 0x0000_0010,
        58 => 0x0000_0020,
        61 => 0x0000_0040,
        62 => 0x0000_2000,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotkeyInstallError {
    TapCreationFailed,
    RunLoopSourceCreationFailed,
    TapEnableFailed,
}

impl fmt::Display for HotkeyInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::TapCreationFailed => "cannot create Fn/Globe event tap",
            Self::RunLoopSourceCreationFailed => "cannot create Fn/Globe event-tap run-loop source",
            Self::TapEnableFailed => "cannot enable Fn/Globe event tap",
        };
        formatter.write_str(message)
    }
}

impl Error for HotkeyInstallError {}

struct CallbackState {
    control: HotkeyControl,
    sender: Sender<HotkeySignal>,
    tap: AtomicPtr<c_void>,
}

impl CallbackState {
    fn emit_observation(&self, observation: KeyboardObservation) -> GateDecision {
        let decision = self.control.handle(observation, Instant::now());

        if let Some(signal) = decision.signal {
            let _ = self.sender.send(signal);
        }
        decision
    }

    fn recover_tap(&self, kind: ObservationKind) {
        self.emit_observation(KeyboardObservation {
            kind,
            keycode: 0,
            flags: 0,
            autorepeat: false,
            replay_marker: false,
        });
        let _ = self.sender.send(HotkeySignal::TapLost);

        let tap = self.tap.load(Ordering::Acquire).cast();
        let Some(tap) = NonNull::new(tap) else {
            return;
        };

        unsafe {
            cg_event_tap_enable(tap.as_ptr(), true);
            if cg_event_tap_is_enabled(tap.as_ptr()) {
                let _ = self.sender.send(HotkeySignal::TapRestored);
            }
        }
    }
}

pub struct HotkeyListener {
    tap: CFMachPort,
    source: CFRunLoopSource,
    run_loop: CFRunLoop,
    _callback_state: Box<CallbackState>,
}

impl HotkeyListener {
    pub fn install(
        sender: Sender<HotkeySignal>,
        control: HotkeyControl,
    ) -> Result<Self, HotkeyInstallError> {
        let callback_state = Box::new(CallbackState {
            control,
            sender,
            tap: AtomicPtr::new(null_mut()),
        });
        let user_info = (&*callback_state as *const CallbackState)
            .cast_mut()
            .cast::<c_void>();

        let tap_ref = unsafe {
            cg_event_tap_create(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                KEYBOARD_EVENT_MASK,
                hotkey_event_callback,
                user_info,
            )
        };
        if tap_ref.is_null() {
            return Err(HotkeyInstallError::TapCreationFailed);
        }

        callback_state
            .tap
            .store(tap_ref.cast::<c_void>(), Ordering::Release);
        let tap = unsafe { CFMachPort::wrap_under_create_rule(tap_ref) };
        let source = tap.create_runloop_source(0).map_err(|()| {
            unsafe {
                cg_event_tap_enable(tap_ref, false);
                CFMachPortInvalidate(tap_ref);
            }
            HotkeyInstallError::RunLoopSourceCreationFailed
        })?;
        let run_loop = CFRunLoop::get_main();
        run_loop.add_source(&source, unsafe { kCFRunLoopCommonModes });

        unsafe {
            cg_event_tap_enable(tap_ref, true);
        }
        if !unsafe { cg_event_tap_is_enabled(tap_ref) } {
            run_loop.remove_source(&source, unsafe { kCFRunLoopCommonModes });
            unsafe {
                CFRunLoopSourceInvalidate(source.as_concrete_TypeRef());
                CFMachPortInvalidate(tap_ref);
            }
            return Err(HotkeyInstallError::TapEnableFailed);
        }

        Ok(Self {
            tap,
            source,
            run_loop,
            _callback_state: callback_state,
        })
    }

    pub fn set_preferences(&self, preferences: Preferences) {
        self._callback_state.control.set_preferences(preferences);
    }

    pub fn begin_assignment(&self) -> Option<AssignmentEpoch> {
        self._callback_state.control.begin_assignment()
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.run_loop
            .remove_source(&self.source, unsafe { kCFRunLoopCommonModes });
        unsafe {
            cg_event_tap_enable(self.tap.as_concrete_TypeRef(), false);
            CFMachPortInvalidate(self.tap.as_concrete_TypeRef());
            CFRunLoopSourceInvalidate(self.source.as_concrete_TypeRef());
        }
    }
}

unsafe extern "C" fn hotkey_event_callback(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let callback = catch_unwind(AssertUnwindSafe(|| {
        let Some(state) = user_info.cast::<CallbackState>().as_ref() else {
            return event;
        };

        let kind = match event_type {
            CGEventType::TapDisabledByTimeout => {
                state.recover_tap(ObservationKind::TapDisabledByTimeout);
                return event;
            }
            CGEventType::TapDisabledByUserInput => {
                state.recover_tap(ObservationKind::TapDisabledByUserInput);
                return event;
            }
            CGEventType::KeyDown => ObservationKind::KeyDown,
            CGEventType::KeyUp => ObservationKind::KeyUp,
            CGEventType::FlagsChanged => ObservationKind::FlagsChanged,
            _ => return event,
        };

        if event.is_null() {
            return event;
        }

        let keycode = u16::try_from(cg_event_get_integer_value_field(
            event,
            EventField::KEYBOARD_EVENT_KEYCODE,
        ))
        .unwrap_or(u16::MAX);
        let observation = KeyboardObservation {
            kind,
            keycode,
            flags: cg_event_get_flags(event),
            autorepeat: cg_event_get_integer_value_field(
                event,
                EventField::KEYBOARD_EVENT_AUTOREPEAT,
            ) != 0,
            replay_marker: cg_event_get_integer_value_field(
                event,
                EventField::EVENT_SOURCE_USER_DATA,
            ) == REPLAY_MARKER,
        };
        let decision = state.emit_observation(observation);
        let mut replay_backend = CoreGraphicsReplayBackend;
        if post_replay_batch(&decision.replay, proxy, &mut replay_backend).is_err() {
            tracing::warn!(error_category = "hotkey_replay");
        }

        match decision.disposition {
            EventDisposition::Pass => event,
            EventDisposition::Suppress => null_mut(),
        }
    }));

    callback.unwrap_or(event)
}

type CGEventTapCallback =
    unsafe extern "C" fn(CGEventTapProxy, CGEventType, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    #[link_name = "CGEventTapCreate"]
    fn cg_event_tap_create(
        tap: CGEventTapLocation,
        place: CGEventTapPlacement,
        options: CGEventTapOptions,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    #[link_name = "CGEventTapEnable"]
    fn cg_event_tap_enable(tap: CFMachPortRef, enable: bool);

    #[link_name = "CGEventTapIsEnabled"]
    fn cg_event_tap_is_enabled(tap: CFMachPortRef) -> bool;

    #[link_name = "CGEventGetIntegerValueField"]
    fn cg_event_get_integer_value_field(event: CGEventRef, field: CGEventField) -> i64;

    #[link_name = "CGEventGetFlags"]
    fn cg_event_get_flags(event: CGEventRef) -> u64;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppController, AppEvent, AppStatus, Effect, PermissionSnapshot};
    use std::sync::{
        atomic::AtomicPtr,
        mpsc::{self, TryRecvError},
    };
    use std::time::{Duration, Instant};

    const COMMAND: u64 = 0x0010_0000;
    const DEVICE_LEFT_COMMAND: u64 = 0x0000_0008;
    const DEVICE_RIGHT_COMMAND: u64 = 0x0000_0010;

    fn key_down(keycode: u16) -> KeyboardObservation {
        key_down_with_flags(keycode, 0)
    }

    fn key_down_with_flags(keycode: u16, flags: u64) -> KeyboardObservation {
        KeyboardObservation {
            kind: ObservationKind::KeyDown,
            keycode,
            flags,
            autorepeat: false,
            replay_marker: false,
        }
    }

    fn key_up(keycode: u16) -> KeyboardObservation {
        key_up_with_flags(keycode, 0)
    }

    fn key_up_with_flags(keycode: u16, flags: u64) -> KeyboardObservation {
        KeyboardObservation {
            kind: ObservationKind::KeyUp,
            keycode,
            flags,
            autorepeat: false,
            replay_marker: false,
        }
    }

    fn flags_changed(keycode: u16, flags: u64) -> KeyboardObservation {
        KeyboardObservation {
            kind: ObservationKind::FlagsChanged,
            keycode,
            flags,
            autorepeat: false,
            replay_marker: false,
        }
    }

    fn tap_disabled() -> KeyboardObservation {
        KeyboardObservation {
            kind: ObservationKind::TapDisabledByTimeout,
            keycode: 0,
            flags: 0,
            autorepeat: false,
            replay_marker: false,
        }
    }

    fn replay_down(keycode: u16) -> ReplayEvent {
        ReplayEvent {
            kind: ObservationKind::KeyDown,
            keycode,
            flags: 0,
        }
    }

    fn replay_up(keycode: u16) -> ReplayEvent {
        ReplayEvent {
            kind: ObservationKind::KeyUp,
            keycode,
            flags: 0,
        }
    }

    fn replay_down_with_flags(keycode: u16, flags: u64) -> ReplayEvent {
        ReplayEvent {
            kind: ObservationKind::KeyDown,
            keycode,
            flags,
        }
    }

    #[test]
    fn tap_loss_releases_a_held_key_before_reporting_the_loss() {
        let (sender, receiver) = mpsc::channel();
        let state = CallbackState {
            control: HotkeyControl::new(Preferences::default()),
            sender,
            tap: AtomicPtr::new(null_mut()),
        };
        state.emit_observation(key_down(63));
        assert_eq!(receiver.recv().unwrap(), HotkeySignal::Pressed);

        state.recover_tap(ObservationKind::TapDisabledByTimeout);

        assert_eq!(receiver.recv().unwrap(), HotkeySignal::Cancelled);
        assert_eq!(receiver.recv().unwrap(), HotkeySignal::TapLost);
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn short_press_replays_and_long_press_is_consumed() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences::default());

        assert_eq!(
            gate.handle(key_down(63), start).signal,
            Some(HotkeySignal::Pressed)
        );
        let short = gate.handle(key_up(63), start + Duration::from_millis(499));
        assert_eq!(short.signal, Some(HotkeySignal::Released { short: true }));
        assert_eq!(short.replay, vec![replay_down(63), replay_up(63)]);

        gate.handle(key_down(63), start);
        let long = gate.handle(key_up(63), start + Duration::from_millis(500));
        assert_eq!(long.signal, Some(HotkeySignal::Released { short: false }));
        assert!(long.replay.is_empty());
    }

    #[test]
    fn replay_marked_observation_passes_without_changing_pending_gate() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        assert_eq!(
            gate.handle(key_down(63), start).signal,
            Some(HotkeySignal::Pressed)
        );

        let marked_release = gate.handle(
            KeyboardObservation {
                replay_marker: true,
                ..key_up(63)
            },
            start + Duration::from_millis(50),
        );
        assert_eq!(marked_release.disposition, EventDisposition::Pass);
        assert_eq!(marked_release.signal, None);
        assert!(marked_release.replay.is_empty());

        let physical_release = gate.handle(key_up(63), start + Duration::from_millis(100));
        assert_eq!(
            physical_release.signal,
            Some(HotkeySignal::Released { short: true })
        );
        assert_eq!(
            physical_release.replay,
            vec![replay_down(63), replay_up(63)]
        );
    }

    #[test]
    fn trigger_autorepeat_is_suppressed_without_affecting_pending_capture() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        assert_eq!(
            gate.handle(key_down(63), start).signal,
            Some(HotkeySignal::Pressed)
        );

        let repeat = gate.handle(
            KeyboardObservation {
                autorepeat: true,
                ..key_down(63)
            },
            start + Duration::from_millis(500),
        );
        assert_eq!(repeat.disposition, EventDisposition::Suppress);
        assert_eq!(repeat.signal, None);
        assert!(repeat.replay.is_empty());

        let release = gate.handle(key_up(63), start + Duration::from_millis(600));
        assert_eq!(
            release.signal,
            Some(HotkeySignal::Released { short: false })
        );
        assert!(release.replay.is_empty());
    }

    #[test]
    fn queued_press_rejects_assignment_and_release_completes_capture() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        let pressed = gate.handle(key_down(63), start).signal.unwrap();

        assert!(gate.begin_assignment().is_none());
        let released = gate
            .handle(key_up(63), start + Duration::from_millis(100))
            .signal
            .unwrap();

        let mut controller = AppController::new();
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        assert_eq!(
            controller.handle(AppEvent::TriggerPressed),
            vec![Effect::StartCapture]
        );
        assert_eq!(pressed, HotkeySignal::Pressed);
        assert_eq!(released, HotkeySignal::Released { short: true });
        assert_eq!(
            controller.handle(AppEvent::TriggerReleased { short: true }),
            vec![Effect::AbortCapture]
        );
        assert_eq!(controller.status(), &AppStatus::Ready);
    }

    #[test]
    fn assignment_request_preserves_combination_and_consumed_release_modes() {
        let now = Instant::now();
        let mut combination = InputGate::new(Preferences::default());
        combination.handle(key_down(63), now);
        combination.handle(key_down(8), now);
        assert!(combination.begin_assignment().is_none());
        assert_eq!(
            combination.handle(key_up(63), now).disposition,
            EventDisposition::Pass
        );

        let mut consumed = InputGate::new(Preferences::default());
        assert!(consumed.begin_assignment().is_some());
        consumed.handle(key_down(49), now);
        assert!(consumed.begin_assignment().is_none());
        assert_eq!(
            consumed.handle(key_up(49), now).disposition,
            EventDisposition::Suppress
        );
    }

    #[test]
    fn second_key_cancels_capture_and_replays_combination_in_order() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences {
            trigger: TriggerKey::KeyCode(55),
            threshold: HoldThreshold::MS_500,
        });
        let left_command = COMMAND | DEVICE_LEFT_COMMAND;
        gate.handle(flags_changed(55, left_command), start);

        let chord = gate.handle(
            key_down_with_flags(8, left_command),
            start + Duration::from_millis(600),
        );
        assert_eq!(chord.signal, Some(HotkeySignal::Cancelled));
        assert_eq!(
            chord.replay,
            vec![
                ReplayEvent {
                    kind: ObservationKind::FlagsChanged,
                    keycode: 55,
                    flags: left_command,
                },
                replay_down_with_flags(8, left_command),
            ]
        );
        assert_eq!(
            gate.handle(
                key_up_with_flags(8, left_command),
                start + Duration::from_millis(610)
            )
            .disposition,
            EventDisposition::Pass
        );
        assert_eq!(
            gate.handle(flags_changed(55, 0), start + Duration::from_millis(620))
                .disposition,
            EventDisposition::Pass
        );
    }

    #[test]
    fn preference_change_does_not_change_pending_press() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        gate.handle(key_down(63), start);
        gate.set_preferences(Preferences {
            trigger: TriggerKey::KeyCode(49),
            threshold: HoldThreshold::MS_250,
        });
        let release = gate.handle(key_up(63), start + Duration::from_millis(400));
        assert_eq!(release.signal, Some(HotkeySignal::Released { short: true }));
    }

    #[test]
    fn assignment_selects_supported_key_and_escape_cancels() {
        let now = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        let selected_epoch = gate.begin_assignment().unwrap();
        assert_eq!(
            gate.handle(flags_changed(54, COMMAND | DEVICE_RIGHT_COMMAND), now)
                .signal,
            Some(HotkeySignal::AssignmentSelected {
                trigger: TriggerKey::KeyCode(54),
                epoch: selected_epoch,
            })
        );
        assert_eq!(
            gate.handle(flags_changed(54, 0), now).disposition,
            EventDisposition::Suppress
        );

        let cancelled_epoch = gate.begin_assignment().unwrap();
        assert_eq!(
            gate.handle(key_down(53), now).signal,
            Some(HotkeySignal::AssignmentCancelled {
                epoch: cancelled_epoch,
            })
        );
    }

    #[test]
    fn excluded_assignment_passes_through_and_keeps_binding() {
        let now = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        let epoch = gate.begin_assignment().unwrap();
        let decision = gate.handle(key_down(57), now);
        assert_eq!(decision.disposition, EventDisposition::Pass);
        assert_eq!(
            decision.signal,
            Some(HotkeySignal::AssignmentCancelled { epoch })
        );
        assert_eq!(gate.preferences().trigger, TriggerKey::FnGlobe);
    }

    #[test]
    fn consumed_assignment_suppresses_matching_release_only() {
        let now = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        gate.begin_assignment().unwrap();
        gate.handle(key_down(49), now);

        assert_eq!(
            gate.handle(key_up(8), now).disposition,
            EventDisposition::Pass
        );
        assert_eq!(
            gate.handle(key_up(49), now).disposition,
            EventDisposition::Suppress
        );
        assert_eq!(
            gate.handle(key_up(49), now).disposition,
            EventDisposition::Pass
        );
    }

    #[test]
    fn modifier_release_is_not_accepted_as_assignment() {
        let now = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        let epoch = gate.begin_assignment().unwrap();

        let release = gate.handle(flags_changed(54, 0), now);
        assert_eq!(release.disposition, EventDisposition::Pass);
        assert_eq!(release.signal, None);
        assert_eq!(
            gate.handle(key_down(49), now).signal,
            Some(HotkeySignal::AssignmentSelected {
                trigger: TriggerKey::KeyCode(49),
                epoch,
            })
        );
    }

    #[test]
    fn caps_lock_flags_press_is_excluded_and_passed_through() {
        const ALPHA_SHIFT: u64 = 0x0001_0000;

        let now = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        let epoch = gate.begin_assignment().unwrap();

        let decision = gate.handle(flags_changed(57, ALPHA_SHIFT), now);
        assert_eq!(decision.disposition, EventDisposition::Pass);
        assert_eq!(
            decision.signal,
            Some(HotkeySignal::AssignmentCancelled { epoch })
        );
    }

    #[test]
    fn modifier_replay_preserves_flags_changed_event_type() {
        assert_eq!(
            ReplayEvent {
                kind: ObservationKind::FlagsChanged,
                keycode: 55,
                flags: COMMAND,
            }
            .event_type() as u32,
            CGEventType::FlagsChanged as u32
        );
    }

    #[test]
    fn pending_modifier_releases_when_opposite_side_keeps_aggregate_flag_set() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences {
            trigger: TriggerKey::KeyCode(54),
            threshold: HoldThreshold::MS_500,
        });
        gate.handle(flags_changed(55, COMMAND | DEVICE_LEFT_COMMAND), start);
        gate.handle(
            flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND | DEVICE_RIGHT_COMMAND),
            start,
        );

        assert_eq!(
            gate.handle(
                flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND),
                start + Duration::from_millis(100)
            )
            .signal,
            Some(HotkeySignal::Released { short: true })
        );
    }

    #[test]
    fn released_modifier_with_opposite_side_held_does_not_cancel_pending() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        gate.handle(flags_changed(55, COMMAND | DEVICE_LEFT_COMMAND), start);
        gate.handle(
            flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND | DEVICE_RIGHT_COMMAND),
            start,
        );
        gate.handle(key_down(63), start);

        let released_right_command = gate.handle(
            flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND),
            start + Duration::from_millis(100),
        );
        assert_eq!(released_right_command.disposition, EventDisposition::Pass);
        assert_eq!(released_right_command.signal, None);
        assert_eq!(
            gate.handle(key_up(63), start + Duration::from_millis(200))
                .signal,
            Some(HotkeySignal::Released { short: true })
        );
    }

    #[test]
    fn released_modifier_with_opposite_side_held_does_not_assign() {
        let now = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        gate.handle(flags_changed(55, COMMAND | DEVICE_LEFT_COMMAND), now);
        gate.handle(
            flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND | DEVICE_RIGHT_COMMAND),
            now,
        );
        let epoch = gate.begin_assignment().unwrap();

        let released_right_command =
            gate.handle(flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND), now);
        assert_eq!(released_right_command.disposition, EventDisposition::Pass);
        assert_eq!(released_right_command.signal, None);
        assert_eq!(
            gate.handle(key_down(49), now).signal,
            Some(HotkeySignal::AssignmentSelected {
                trigger: TriggerKey::KeyCode(49),
                epoch,
            })
        );
    }

    #[derive(Default)]
    struct FailingSecondReplayBackend {
        construction_attempts: usize,
        posted: Vec<ReplayEvent>,
    }

    impl ReplayBackend for FailingSecondReplayBackend {
        type Prepared = ReplayEvent;

        fn prepare(&mut self, replay: ReplayEvent) -> Result<Self::Prepared, ()> {
            self.construction_attempts += 1;
            if self.construction_attempts == 2 {
                Err(())
            } else {
                Ok(replay)
            }
        }

        fn post(&mut self, replay: Self::Prepared, _proxy: CGEventTapProxy) {
            self.posted.push(replay);
        }
    }

    #[test]
    fn second_replay_construction_failure_posts_nothing() {
        let replay = [replay_down(63), replay_up(63)];
        let mut backend = FailingSecondReplayBackend::default();

        assert_eq!(
            post_replay_batch(&replay, std::ptr::null(), &mut backend),
            Err(())
        );
        assert_eq!(backend.construction_attempts, 2);
        assert!(backend.posted.is_empty());
    }

    #[test]
    fn tap_loss_reset_does_not_turn_right_command_release_into_pending_chord() {
        let start = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        gate.handle(
            flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND | DEVICE_RIGHT_COMMAND),
            start,
        );
        gate.handle(tap_disabled(), start);
        gate.handle(key_down(63), start);

        let released_right_command = gate.handle(
            flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND),
            start + Duration::from_millis(100),
        );
        assert_eq!(released_right_command.disposition, EventDisposition::Pass);
        assert_eq!(released_right_command.signal, None);
        assert_eq!(
            gate.handle(key_up(63), start + Duration::from_millis(200))
                .signal,
            Some(HotkeySignal::Released { short: true })
        );
    }

    #[test]
    fn cold_assignment_ignores_right_command_release_with_left_held() {
        let now = Instant::now();
        let mut gate = InputGate::new(Preferences::default());
        let epoch = gate.begin_assignment().unwrap();

        let released_right_command =
            gate.handle(flags_changed(54, COMMAND | DEVICE_LEFT_COMMAND), now);
        assert_eq!(released_right_command.disposition, EventDisposition::Pass);
        assert_eq!(released_right_command.signal, None);
        assert_eq!(
            gate.handle(key_down(49), now).signal,
            Some(HotkeySignal::AssignmentSelected {
                trigger: TriggerKey::KeyCode(49),
                epoch,
            })
        );
    }
}
