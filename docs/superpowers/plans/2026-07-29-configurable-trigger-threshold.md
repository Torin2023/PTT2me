# Configurable Trigger and Hold Threshold Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add persistent menu controls for a 250/500/750 ms hold threshold and an assignable keyboard trigger while preserving immediate recording, short presses, and keyboard combinations.

**Architecture:** Introduce a pure preference model and a pure keyboard-event gate, then keep macOS persistence, event posting, menu actions, and audio effects at their existing platform boundaries. The runtime snapshots preferences at trigger-down, the reducer continues to own recording state transitions, and replay-marked Core Graphics events bypass the gate.

**Tech Stack:** Rust 2021, Core Graphics event taps, AppKit `NSMenu`, Foundation `NSUserDefaults`, existing reducer/runtime/audio test suite.

## Global Constraints

- Supported hold thresholds are exactly `250`, `500`, and `750` milliseconds.
- New installations default to Fn/Globe and `500` milliseconds.
- Audio capture starts immediately on trigger-down.
- A short trigger press aborts capture and is replayed to macOS.
- A long trigger press is consumed and follows the existing recognition flow.
- A second key cancels capture and preserves the original keyboard combination.
- Escape cancels assignment; Caps Lock, Escape, media keys, Power, and Touch ID cannot be assigned.
- Replayed events must never re-enter trigger tracking.
- The application remains local-only and supports Apple Silicon on macOS 13 or newer.

---

## File Structure

- Create `src/preferences.rs`: pure `HoldThreshold`, `TriggerKey`, `Preferences`, storage codec, and `NSUserDefaults` boundary.
- Modify `src/lib.rs`: export the preference module.
- Modify `Cargo.toml`: enable the existing `objc2-foundation` `NSUserDefaults` feature.
- Modify `src/constants.rs`: remove the global `MIN_HOLD_MS`; retain unrelated audio timing constants.
- Modify `src/state.rs`: rename Fn-specific reducer events to trigger events and add explicit cancellation.
- Modify `tests/ptt_flow.rs`: express the end-to-end reducer flow with generic trigger events.
- Modify `src/hotkey.rs`: replace `FnTracker` with a pure configurable input gate and add marked replay/assignment controls to `HotkeyListener`.
- Modify `src/menu.rs`: add menu command transport, trigger assignment/reset controls, and threshold checkmarks.
- Modify `src/runtime.rs`: load/save preferences, drain menu commands, snapshot each press, and connect gate signals to reducer effects.
- Modify `README.md`: document the controls and correct the statement that the app stores no settings.

---

### Task 1: Preference Model and Persistence

**Files:**
- Create: `src/preferences.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Produces: `HoldThreshold::{MS_250, MS_500, MS_750, OPTIONS, millis, from_millis}`
- Produces: `TriggerKey::{FnGlobe, KeyCode(u16), from_keycode, display_name, storage_value, from_storage_value}`
- Produces: `Preferences { trigger: TriggerKey, threshold: HoldThreshold }`
- Produces: `RawPreferenceStore`, `PreferenceRepository<R>`, and `SystemPreferenceStore`

- [ ] **Step 1: Add failing pure-model tests**

Add tests in `src/preferences.rs` before defining the production types:

```rust
#[test]
fn thresholds_are_exact_and_default_to_500_ms() {
    assert_eq!(
        HoldThreshold::OPTIONS.map(HoldThreshold::millis),
        [250, 500, 750]
    );
    assert_eq!(HoldThreshold::default(), HoldThreshold::MS_500);
    assert_eq!(HoldThreshold::from_millis(1_000), None);
}

#[test]
fn trigger_storage_round_trips_and_rejects_excluded_keys() {
    assert_eq!(
        TriggerKey::from_storage_value("fn_globe"),
        Some(TriggerKey::FnGlobe)
    );
    assert_eq!(
        TriggerKey::from_storage_value("keycode:54"),
        Some(TriggerKey::KeyCode(54))
    );
    for code in [53, 57, 72, 73, 74, 127] {
        assert_eq!(TriggerKey::from_keycode(code), None);
    }
    assert_eq!(TriggerKey::from_storage_value("keycode:not-a-number"), None);
}

#[test]
fn invalid_stored_values_fall_back_independently() {
    let loaded = Preferences::from_stored(Some("keycode:57"), Some(1_000));
    assert_eq!(loaded.trigger, TriggerKey::FnGlobe);
    assert_eq!(loaded.threshold, HoldThreshold::MS_500);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test preferences::tests --lib`

Expected: compilation fails because `HoldThreshold`, `TriggerKey`, and `Preferences` do not exist.

- [ ] **Step 3: Implement the pure preference types**

Implement these exact public shapes:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldThreshold(u64);

impl HoldThreshold {
    pub const MS_250: Self = Self(250);
    pub const MS_500: Self = Self(500);
    pub const MS_750: Self = Self(750);
    pub const OPTIONS: [Self; 3] = [Self::MS_250, Self::MS_500, Self::MS_750];

    pub const fn millis(self) -> u64 { self.0 }
    pub const fn from_millis(value: u64) -> Option<Self> {
        match value {
            250 => Some(Self::MS_250),
            500 => Some(Self::MS_500),
            750 => Some(Self::MS_750),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKey {
    FnGlobe,
    KeyCode(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    pub trigger: TriggerKey,
    pub threshold: HoldThreshold,
}
```

`TriggerKey::from_keycode` must map keycodes `63` and `179` to `FnGlobe`;
reject `53` (Escape), `57` (Caps Lock), `72`, `73`, `74` (volume/mute), and
`127` (Power); accept other keyboard keycodes in `0..=126`.
`display_name` must distinguish left/right Command, Shift, Option, and Control,
name F1–F20 and navigation keys from `core_graphics::event::KeyCode`, return
`Fn / Globe` for the default, and use `Клавиша <code>` for an accepted code
without a fixed label.

- [ ] **Step 4: Run the pure-model tests and verify GREEN**

Run: `cargo test preferences::tests --lib`

Expected: all preference model tests pass.

- [ ] **Step 5: Add failing store-boundary tests**

Define an internal raw-store seam and test validation without writing real user defaults:

```rust
trait RawPreferenceStore {
    fn trigger_value(&self) -> Option<String>;
    fn threshold_value(&self) -> Option<u64>;
    fn set_trigger_value(&mut self, value: &str) -> Result<(), ()>;
    fn set_threshold_value(&mut self, value: u64) -> Result<(), ()>;
}

#[derive(Default)]
struct MemoryRawStore {
    trigger: Option<String>,
    threshold: Option<u64>,
}

impl MemoryRawStore {
    fn with(trigger: &str, threshold: u64) -> Self {
        Self {
            trigger: Some(trigger.to_owned()),
            threshold: Some(threshold),
        }
    }
}

impl RawPreferenceStore for MemoryRawStore {
    fn trigger_value(&self) -> Option<String> { self.trigger.clone() }
    fn threshold_value(&self) -> Option<u64> { self.threshold }
    fn set_trigger_value(&mut self, value: &str) -> Result<(), ()> {
        self.trigger = Some(value.to_owned());
        Ok(())
    }
    fn set_threshold_value(&mut self, value: u64) -> Result<(), ()> {
        self.threshold = Some(value);
        Ok(())
    }
}

#[test]
fn preference_store_loads_and_saves_validated_values() {
    let mut raw = MemoryRawStore::with("keycode:54", 750);
    let mut store = PreferenceRepository::new(raw);
    assert_eq!(
        store.load(),
        Preferences {
            trigger: TriggerKey::KeyCode(54),
            threshold: HoldThreshold::MS_750,
        }
    );
    assert_eq!(
        store.save(Preferences::default()),
        Ok(())
    );
    raw = store.into_inner();
    assert_eq!(raw.trigger.as_deref(), Some("fn_globe"));
    assert_eq!(raw.threshold, Some(500));
}
```

- [ ] **Step 6: Run the store test and verify RED**

Run: `cargo test preference_store_loads_and_saves_validated_values --lib`

Expected: compilation fails because `RawPreferenceStore` and
`PreferenceRepository` are incomplete.

- [ ] **Step 7: Implement the store seam and macOS defaults adapter**

Implement `PreferenceRepository<R: RawPreferenceStore>` with `new`, `load`,
`save`, and test-only `into_inner`; `load` validates through
`Preferences::from_stored`, while `save` calls both raw setters.

Add `NSUserDefaults` to the enabled `objc2-foundation` features in `Cargo.toml`.
Use keys `ptt2me.trigger-key` and `ptt2me.hold-threshold-ms`.
`SystemPreferenceStore` implements `RawPreferenceStore`, uses `objectForKey` to
distinguish a missing integer from value zero, and returns `Err(())` from a
setter only when `NSUserDefaults::synchronize()` returns false.

- [ ] **Step 8: Run tests and commit**

Run: `cargo test preferences::tests --lib`

Expected: PASS.

Commit:

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/preferences.rs
git commit -m "feat: add trigger preferences"
```

---

### Task 2: Generic Trigger Reducer Events

**Files:**
- Modify: `src/constants.rs`
- Modify: `src/state.rs`
- Modify: `tests/ptt_flow.rs`

**Interfaces:**
- Consumes: short/long classification from the input gate added in Task 3.
- Produces: `AppEvent::{TriggerPressed, TriggerReleased { short: bool }, TriggerCancelled}`
- Keeps: `Effect::{StartCapture, AbortCapture, FinishCaptureAfter}`

- [ ] **Step 1: Replace the reducer tests first**

Change tests to express the required behavior before changing production matches:

```rust
#[test]
fn trigger_press_starts_capture_immediately() {
    let mut controller = AppController::ready_for_test();
    assert_eq!(
        controller.handle(AppEvent::TriggerPressed),
        vec![Effect::StartCapture]
    );
}

#[test]
fn short_release_and_combination_cancel_capture() {
    for event in [
        AppEvent::TriggerReleased { short: true },
        AppEvent::TriggerCancelled,
    ] {
        let mut controller = AppController::recording_for_test();
        assert_eq!(controller.handle(event), vec![Effect::AbortCapture]);
        assert_eq!(controller.status(), &AppStatus::Ready);
    }
}

#[test]
fn long_release_finishes_capture() {
    let mut controller = AppController::recording_for_test();
    assert_eq!(
        controller.handle(AppEvent::TriggerReleased { short: false }),
        vec![Effect::FinishCaptureAfter { delay_ms: 180 }]
    );
}
```

- [ ] **Step 2: Run the reducer tests and verify RED**

Run: `cargo test state::tests --lib`

Expected: compilation fails because the generic trigger variants do not exist.

- [ ] **Step 3: Implement the generic events**

Rename `FnPressed` to `TriggerPressed`. Replace `FnReleased { held_ms }` with
`TriggerReleased { short: bool }`. Add `TriggerCancelled`. Both the short
release and cancellation branches must return to `Ready` with
`Effect::AbortCapture`; the long branch must retain `RELEASE_GRACE_MS`.
Remove `MIN_HOLD_MS` from `src/constants.rs`.

- [ ] **Step 4: Update the integration flow and verify GREEN**

Update `tests/ptt_flow.rs` to use `TriggerPressed` and
`TriggerReleased { short: false }`.

Run: `cargo test state::tests --lib && cargo test --test ptt_flow`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/constants.rs src/state.rs tests/ptt_flow.rs
git commit -m "refactor: generalize push-to-talk trigger events"
```

---

### Task 3: Configurable Input Gate and Event Replay

**Files:**
- Modify: `src/hotkey.rs`

**Interfaces:**
- Consumes: `TriggerKey`, `HoldThreshold`
- Produces: `HotkeySignal::{Pressed, Released { short: bool }, Cancelled, AssignmentSelected(TriggerKey), AssignmentCancelled, TapLost, TapRestored}`
- Produces: `HotkeyListener::{set_preferences, begin_assignment}`
- Internal pure API: `InputGate::handle(KeyboardObservation, Instant) -> GateDecision`

- [ ] **Step 1: Add failing gate tests for short and long presses**

Define test observations with event kind, keycode, flags, repeat bit, and replay
marker. The test module uses these exact helpers:

```rust
const COMMAND: u64 = 0x0010_0000;

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
```

Add:

```rust
#[test]
fn short_press_replays_and_long_press_is_consumed() {
    let start = Instant::now();
    let mut gate = InputGate::new(Preferences::default());

    assert_eq!(
        gate.handle(key_down(63), start).signal,
        Some(HotkeySignal::Pressed)
    );
    let short = gate.handle(key_up(63), start + Duration::from_millis(499));
    assert_eq!(
        short.signal,
        Some(HotkeySignal::Released { short: true })
    );
    assert_eq!(short.replay, vec![replay_down(63), replay_up(63)]);

    gate.handle(key_down(63), start);
    let long = gate.handle(key_up(63), start + Duration::from_millis(500));
    assert_eq!(
        long.signal,
        Some(HotkeySignal::Released { short: false })
    );
    assert!(long.replay.is_empty());
}
```

- [ ] **Step 2: Run and verify RED**

Run: `cargo test hotkey::tests::short_press_replays_and_long_press_is_consumed --lib`

Expected: compilation fails because `InputGate` and replay decisions do not exist.

- [ ] **Step 3: Implement pending-press classification**

Replace `FnTracker` with:

```rust
struct InputGate {
    preferences: Preferences,
    mode: GateMode,
}

enum GateMode {
    Idle,
    Pending {
        physical_keycode: u16,
        pressed_at: Instant,
        threshold: HoldThreshold,
        down: ReplayEvent,
    },
    Combination { physical_keycode: u16 },
    Assigning,
    AssignmentConsumed { physical_keycode: u16 },
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
```

The pending mode must snapshot the threshold and physical Fn/Globe
representation. A marked replay event always returns `Pass` without changing
mode. Trigger auto-repeat returns `Suppress` without a signal.

- [ ] **Step 4: Add failing combination and preference-snapshot tests**

```rust
#[test]
fn second_key_cancels_capture_and_replays_combination_in_order() {
    let start = Instant::now();
    let mut gate = InputGate::new(Preferences {
        trigger: TriggerKey::KeyCode(55),
        threshold: HoldThreshold::MS_500,
    });
    gate.handle(flags_changed(55, COMMAND), start);

    let chord = gate.handle(key_down_with_flags(8, COMMAND), start + Duration::from_millis(600));
    assert_eq!(chord.signal, Some(HotkeySignal::Cancelled));
    assert_eq!(chord.replay, vec![
        ReplayEvent {
            kind: ObservationKind::FlagsChanged,
            keycode: 55,
            flags: COMMAND,
        },
        replay_down_with_flags(8, COMMAND),
    ]);
    assert_eq!(
        gate.handle(key_up_with_flags(8, COMMAND), start + Duration::from_millis(610)).disposition,
        EventDisposition::Pass
    );
    assert_eq!(
        gate.handle(flags_changed(55, 0), start + Duration::from_millis(620)).disposition,
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
    assert_eq!(
        release.signal,
        Some(HotkeySignal::Released { short: true })
    );
}
```

- [ ] **Step 5: Run RED, then implement combination mode**

Run: `cargo test hotkey::tests --lib`

Expected before implementation: the two new tests fail.

Implement replay of the buffered trigger-down and the first different
key-down. Enter `Combination`, pass all following events, and return to `Idle`
only when the physical trigger releases.

- [ ] **Step 6: Add failing assignment tests**

```rust
#[test]
fn assignment_selects_supported_key_and_escape_cancels() {
    let now = Instant::now();
    let mut gate = InputGate::new(Preferences::default());
    gate.begin_assignment();
    assert_eq!(
        gate.handle(flags_changed(54, COMMAND), now).signal,
        Some(HotkeySignal::AssignmentSelected(TriggerKey::KeyCode(54)))
    );

    gate.begin_assignment();
    assert_eq!(
        gate.handle(key_down(53), now).signal,
        Some(HotkeySignal::AssignmentCancelled)
    );
}

#[test]
fn excluded_assignment_passes_through_and_keeps_binding() {
    let now = Instant::now();
    let mut gate = InputGate::new(Preferences::default());
    gate.begin_assignment();
    let decision = gate.handle(key_down(57), now);
    assert_eq!(decision.disposition, EventDisposition::Pass);
    assert_eq!(decision.signal, Some(HotkeySignal::AssignmentCancelled));
    assert_eq!(gate.preferences().trigger, TriggerKey::FnGlobe);
}
```

- [ ] **Step 7: Run RED, implement assignment, then run GREEN**

Run before implementation: `cargo test hotkey::tests --lib`

Expected: assignment tests fail.

Implement `begin_assignment`, supported selection consumption, Escape
cancellation consumption, excluded-key pass-through, and modifier press-edge
detection from `FlagsChanged` flags. A modifier release is never accepted as a
new assignment. After consuming a supported selection or Escape press, enter
`AssignmentConsumed` and suppress the matching release before returning to
`Idle`; this prevents an unmatched key-up from leaking to macOS.

Run after implementation: `cargo test hotkey::tests --lib`

Expected: PASS.

- [ ] **Step 8: Wire replay into the Core Graphics callback**

Extend `KeyboardObservation` with:

```rust
flags: u64,
autorepeat: bool,
replay_marker: bool,
```

Read `KEYBOARD_EVENT_AUTOREPEAT` and `EVENT_SOURCE_USER_DATA`. Use a private
nonzero `REPLAY_MARKER`. For each `ReplayEvent`, create a
`CGEvent::new_keyboard_event`, call `set_type` so modifier replay preserves
`FlagsChanged`, restore flags, set
`EVENT_SOURCE_USER_DATA = REPLAY_MARKER`, and call `post_from_tap(proxy)`.
Return the original event only for `EventDisposition::Pass`; otherwise return
null. Preserve the existing tap-loss recovery.

- [ ] **Step 9: Run all hotkey tests and commit**

Run: `cargo test hotkey::tests --lib`

Expected: PASS, including existing tap-loss coverage adapted to `InputGate`.

Commit:

```bash
git add src/hotkey.rs
git commit -m "feat: preserve configurable trigger input"
```

---

### Task 4: Menu and Runtime Wiring

**Files:**
- Modify: `src/menu.rs`
- Modify: `src/runtime.rs`

**Interfaces:**
- Consumes: `Preferences`, `HoldThreshold`, `TriggerKey`, `PreferenceRepository`
- Consumes: `HotkeyListener::{set_preferences, begin_assignment}`
- Produces: `MenuCommand::{BeginTriggerAssignment, ResetTrigger, SetThreshold(HoldThreshold)}`
- Produces: `MenuBar::new(Preferences, Sender<MenuCommand>)` and `MenuBar::render_preferences`

- [ ] **Step 1: Add failing menu descriptor and projection tests**

Replace the four-entry invariant with the new exact order:

```rust
#[test]
fn menu_descriptor_has_adjacent_trigger_and_threshold_controls() {
    assert_eq!(
        MENU_DESCRIPTOR,
        [
            MenuEntry::Status,
            MenuEntry::Version,
            MenuEntry::Trigger,
            MenuEntry::Threshold,
            MenuEntry::Separator,
            MenuEntry::Quit,
        ]
    );
}

#[test]
fn preference_projection_marks_only_the_selected_threshold() {
    let projection = PreferenceProjection::from(Preferences {
        trigger: TriggerKey::KeyCode(54),
        threshold: HoldThreshold::MS_750,
    });
    assert_eq!(projection.trigger_title, "Правый Command");
    assert_eq!(projection.threshold_states, [false, false, true]);
}
```

- [ ] **Step 2: Run and verify RED**

Run: `cargo test menu::tests --lib`

Expected: tests fail because trigger/threshold entries and projection do not exist.

- [ ] **Step 3: Implement menu commands and submenus**

Give `MenuTarget` a `Sender<MenuCommand>` ivar. Add Objective-C actions
`assignTrigger:`, `resetTrigger:`, and `selectThreshold:`. Store 250/500/750 in
the threshold items' `tag` values. Build two adjacent submenus:

```text
Клавиша активации
  Текущая: <display name>   [disabled]
  Назначить…
  Сбросить на Fn / Globe
Порог удержания
  250 мс
  500 мс                  [checkmark when selected]
  750 мс
```

Retain the current-trigger row and all three threshold rows in `MenuBar`.
`render_preferences` updates their title/state without rebuilding the menu.

- [ ] **Step 4: Run menu tests and verify GREEN**

Run: `cargo test menu::tests --lib`

Expected: PASS.

- [ ] **Step 5: Add a failing runtime preference-command test**

Add a menu-command helper test using `PreferenceRepository<MemoryRawStore>`:

```rust
#[derive(Default)]
struct MemoryRawStore {
    trigger: Option<String>,
    threshold: Option<u64>,
}

impl RawPreferenceStore for MemoryRawStore {
    fn trigger_value(&self) -> Option<String> { self.trigger.clone() }
    fn threshold_value(&self) -> Option<u64> { self.threshold }
    fn set_trigger_value(&mut self, value: &str) -> Result<(), ()> {
        self.trigger = Some(value.to_owned());
        Ok(())
    }
    fn set_threshold_value(&mut self, value: u64) -> Result<(), ()> {
        self.threshold = Some(value);
        Ok(())
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
```

`RuntimePreferences<R: RawPreferenceStore>` owns the current `Preferences` and
its `PreferenceRepository<R>`. `apply` accepts only preference-changing menu
commands, updates the in-memory value first, persists the complete value, and
returns the persistence result. `saved()` is a test-only accessor that reloads
the repository.

- [ ] **Step 6: Run runtime tests and verify RED**

Run: `cargo test runtime::tests --lib`

Expected: compilation fails because the runtime preference helpers do not exist.

- [ ] **Step 7: Wire commands, preferences, and signals**

At startup:

1. Create `SystemPreferenceStore`.
2. Load `Preferences`.
3. Create `MenuBar::new(preferences, menu_sender)`.
4. Install `HotkeyListener` with the same preferences.

During each drain:

1. Process menu commands before keyboard signals.
2. On threshold/reset, update in-memory preferences, render the menu, call
   `hotkey.set_preferences`, and persist.
3. On assignment command while `Ready`, render `● Нажмите клавишу…` and call
   `hotkey.begin_assignment`.
4. On `AssignmentSelected`, update/persist/render the key and restore the
   controller-derived status.
5. On `AssignmentCancelled`, restore the controller-derived status.
6. On `Pressed`, dispatch `TriggerPressed`.
7. On `Released { short }`, dispatch `TriggerReleased { short }`.
8. On `Cancelled`, dispatch `TriggerCancelled`.

Remove `press_started` from `Runtime`; duration classification now belongs to
the gate that snapshots the threshold at the physical press.

- [ ] **Step 8: Run runtime and integration tests**

Run: `cargo test runtime::tests --lib && cargo test --test ptt_flow`

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/menu.rs src/runtime.rs
git commit -m "feat: add trigger controls to menu"
```

---

### Task 5: Documentation and Full Verification

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: completed behavior from Tasks 1–4.
- Produces: user-facing workflow and a clean, validated macOS bundle.

- [ ] **Step 1: Update README behavior**

Document:

- default Fn/Globe plus 500 ms;
- `Клавиша активации` assignment/reset;
- exact 250/500/750 ms choices;
- immediate recording and short-press replay;
- combination preservation;
- excluded assignment keys.

Change the privacy statement from “does not save ... settings” to state that
only the selected trigger and threshold are stored in macOS user defaults; no
audio, transcripts, or recognized text are retained.

- [ ] **Step 2: Run formatting and all tests**

Run:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all commands exit zero with no warnings.

- [ ] **Step 3: Build and validate the app bundle**

Run:

```bash
scripts/build-app.sh
scripts/check-bundle.sh build/PTT2me.app
```

Expected: release build succeeds and bundle validation reports success.

- [ ] **Step 4: Perform focused manual macOS checks**

Launch the built application and verify:

1. Fn/Globe with 500 ms is the fresh-default projection.
2. Threshold and trigger persist after relaunch.
3. Short Fn/Globe still switches the configured input source.
4. Short ordinary-key presses reach the frontmost application once.
5. A long press includes speech begun immediately after key-down.
6. Long presses do not type or switch input source.
7. Command+C and Option-generated characters survive when their modifier is assigned.
8. Escape cancels assignment; excluded keys do not replace the current trigger.
9. Repeated short, long, and combination sequences leave no key stuck.

- [ ] **Step 5: Commit documentation**

```bash
git add README.md
git commit -m "docs: explain configurable push-to-talk trigger"
```

- [ ] **Step 6: Record final repository evidence**

Run:

```bash
git status --short
git log -6 --oneline
```

Expected: clean worktree and the feature commits in task order.
