use core_foundation::{
    base::TCFType,
    mach_port::{CFMachPort, CFMachPortInvalidate, CFMachPortRef},
    runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopSource, CFRunLoopSourceInvalidate},
};
use core_graphics::{
    event::{
        CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventTapProxy, CGEventType, EventField,
    },
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
        Mutex,
    },
    time::Instant,
};

const FN_KEYCODE: u16 = 63;
const GLOBE_KEYCODE: u16 = 179;
const SECONDARY_FN_FLAG: u64 = 0x0080_0000;
const KEYBOARD_EVENT_MASK: CGEventMask = (1 << CGEventType::KeyDown as CGEventMask)
    | (1 << CGEventType::KeyUp as CGEventMask)
    | (1 << CGEventType::FlagsChanged as CGEventMask);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotkeySignal {
    Pressed { observed_at: Instant },
    Released { observed_at: Instant },
    TapLost,
    TapRestored,
}

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
    pub fn_flag: bool,
}

#[derive(Default)]
pub struct FnTracker {
    pressed: bool,
}

impl FnTracker {
    pub fn handle_at(
        &mut self,
        observation: KeyboardObservation,
        observed_at: Instant,
    ) -> Option<HotkeySignal> {
        let next_pressed = match observation.kind {
            ObservationKind::KeyDown if observation.is_fn_or_globe() => Some(true),
            ObservationKind::KeyUp if observation.is_fn_or_globe() => Some(false),
            ObservationKind::FlagsChanged if observation.is_fn_or_globe() => {
                Some(observation.fn_flag)
            }
            ObservationKind::TapDisabledByTimeout | ObservationKind::TapDisabledByUserInput => {
                Some(false)
            }
            _ => None,
        }?;

        if next_pressed == self.pressed {
            return None;
        }

        self.pressed = next_pressed;
        Some(if next_pressed {
            HotkeySignal::Pressed { observed_at }
        } else {
            HotkeySignal::Released { observed_at }
        })
    }
}

impl KeyboardObservation {
    fn is_fn_or_globe(self) -> bool {
        matches!(self.keycode, FN_KEYCODE | GLOBE_KEYCODE)
    }

    fn should_suppress(self) -> bool {
        matches!(
            self.kind,
            ObservationKind::KeyDown | ObservationKind::KeyUp | ObservationKind::FlagsChanged
        ) && self.is_fn_or_globe()
    }
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
    tracker: Mutex<FnTracker>,
    sender: Sender<HotkeySignal>,
    tap: AtomicPtr<c_void>,
}

impl CallbackState {
    fn emit_observation(&self, observation: KeyboardObservation) {
        let signal = {
            let mut tracker = self
                .tracker
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            tracker.handle_at(observation, Instant::now())
        };

        if let Some(signal) = signal {
            let _ = self.sender.send(signal);
        }
    }

    fn recover_tap(&self, kind: ObservationKind) {
        self.emit_observation(KeyboardObservation {
            kind,
            keycode: 0,
            fn_flag: false,
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
    pub fn install(sender: Sender<HotkeySignal>) -> Result<Self, HotkeyInstallError> {
        let callback_state = Box::new(CallbackState {
            tracker: Mutex::new(FnTracker::default()),
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
    _proxy: CGEventTapProxy,
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
            fn_flag: cg_event_get_flags(event) & SECONDARY_FN_FLAG != 0,
        };
        state.emit_observation(observation);

        if observation.should_suppress() {
            null_mut()
        } else {
            event
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
    use std::sync::{
        atomic::AtomicPtr,
        mpsc::{self, TryRecvError},
        Mutex,
    };

    fn observation(kind: ObservationKind, keycode: u16, fn_flag: bool) -> KeyboardObservation {
        KeyboardObservation {
            kind,
            keycode,
            fn_flag,
        }
    }

    #[test]
    fn fn_flags_changed_emits_only_press_and_release_edges() {
        let mut tracker = FnTracker::default();
        let fn_down = observation(ObservationKind::FlagsChanged, 63, true);
        let fn_up = observation(ObservationKind::FlagsChanged, 63, false);
        let observed_at = std::time::Instant::now();

        assert_eq!(
            tracker.handle_at(fn_down, observed_at),
            Some(HotkeySignal::Pressed { observed_at })
        );
        assert_eq!(tracker.handle_at(fn_down, observed_at), None);
        assert_eq!(
            tracker.handle_at(fn_up, observed_at),
            Some(HotkeySignal::Released { observed_at })
        );
        assert_eq!(tracker.handle_at(fn_up, observed_at), None);
    }

    #[test]
    fn signals_keep_callback_timestamps_when_processing_is_delayed() {
        let mut tracker = FnTracker::default();
        let pressed_at = std::time::Instant::now();
        let released_at = pressed_at + std::time::Duration::from_millis(900);

        assert_eq!(
            tracker.handle_at(observation(ObservationKind::KeyDown, 63, false), pressed_at),
            Some(HotkeySignal::Pressed {
                observed_at: pressed_at
            })
        );
        assert_eq!(
            tracker.handle_at(observation(ObservationKind::KeyUp, 63, false), released_at),
            Some(HotkeySignal::Released {
                observed_at: released_at
            })
        );
    }

    #[test]
    fn globe_key_events_emit_only_press_and_release_edges() {
        let mut tracker = FnTracker::default();
        let globe_down = observation(ObservationKind::KeyDown, 179, false);
        let globe_up = observation(ObservationKind::KeyUp, 179, false);
        let observed_at = std::time::Instant::now();

        assert_eq!(
            tracker.handle_at(globe_down, observed_at),
            Some(HotkeySignal::Pressed { observed_at })
        );
        assert_eq!(tracker.handle_at(globe_down, observed_at), None);
        assert_eq!(
            tracker.handle_at(globe_up, observed_at),
            Some(HotkeySignal::Released { observed_at })
        );
        assert_eq!(tracker.handle_at(globe_up, observed_at), None);
    }

    #[test]
    fn function_key_key_events_are_supported() {
        let mut tracker = FnTracker::default();
        let observed_at = std::time::Instant::now();

        assert_eq!(
            tracker.handle_at(
                observation(ObservationKind::KeyDown, 63, false),
                observed_at
            ),
            Some(HotkeySignal::Pressed { observed_at })
        );
        assert_eq!(
            tracker.handle_at(observation(ObservationKind::KeyUp, 63, false), observed_at),
            Some(HotkeySignal::Released { observed_at })
        );
    }

    #[test]
    fn unrelated_keyboard_observations_are_ignored() {
        let mut tracker = FnTracker::default();
        let observed_at = std::time::Instant::now();

        assert_eq!(
            tracker.handle_at(
                observation(ObservationKind::KeyDown, 56, false),
                observed_at
            ),
            None
        );
        assert_eq!(
            tracker.handle_at(observation(ObservationKind::KeyUp, 56, false), observed_at),
            None
        );
        assert_eq!(
            tracker.handle_at(
                observation(ObservationKind::FlagsChanged, 56, true),
                observed_at
            ),
            None
        );
    }

    #[test]
    fn tap_timeout_forces_exactly_one_release() {
        let mut tracker = FnTracker::default();
        let timeout = observation(ObservationKind::TapDisabledByTimeout, 0, false);
        let observed_at = std::time::Instant::now();

        assert_eq!(
            tracker.handle_at(
                observation(ObservationKind::FlagsChanged, 63, true),
                observed_at
            ),
            Some(HotkeySignal::Pressed { observed_at })
        );
        assert_eq!(
            tracker.handle_at(timeout, observed_at),
            Some(HotkeySignal::Released { observed_at })
        );
        assert_eq!(tracker.handle_at(timeout, observed_at), None);
    }

    #[test]
    fn only_fn_and_globe_keyboard_events_are_suppressed() {
        for kind in [
            ObservationKind::KeyDown,
            ObservationKind::KeyUp,
            ObservationKind::FlagsChanged,
        ] {
            assert!(observation(kind, 63, false).should_suppress());
            assert!(observation(kind, 179, false).should_suppress());
            assert!(!observation(kind, 56, true).should_suppress());
        }

        assert!(!observation(ObservationKind::TapDisabledByTimeout, 0, false).should_suppress());
        assert!(!observation(ObservationKind::TapDisabledByUserInput, 0, false).should_suppress());
    }

    #[test]
    fn tap_loss_releases_a_held_key_before_reporting_the_loss() {
        let (sender, receiver) = mpsc::channel();
        let state = CallbackState {
            tracker: Mutex::new(FnTracker::default()),
            sender,
            tap: AtomicPtr::new(null_mut()),
        };
        state.emit_observation(observation(ObservationKind::KeyDown, 63, false));
        assert!(matches!(
            receiver.recv().unwrap(),
            HotkeySignal::Pressed { .. }
        ));

        state.recover_tap(ObservationKind::TapDisabledByTimeout);

        assert!(matches!(
            receiver.recv().unwrap(),
            HotkeySignal::Released { .. }
        ));
        assert_eq!(receiver.recv().unwrap(), HotkeySignal::TapLost);
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    }
}
