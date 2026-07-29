# Configurable Trigger and Hold Threshold Design

## Goal

Let the user choose both the keyboard key and the time it must be held before
PTT2me treats the gesture as dictation, while preserving the beginning of
speech, short key presses, and keyboard combinations.

## User Experience

The menu gains two adjacent preference submenus.

`Клавиша активации` contains:

- a disabled row showing the current key;
- `Назначить…`;
- `Сбросить на Fn / Globe`.

Choosing `Назначить…` closes the menu and changes the application status to
`● Нажмите клавишу…`. The next supported physical key becomes the trigger.
Escape cancels assignment and leaves the existing trigger unchanged.

Supported triggers include ordinary keyboard keys, function keys, and
left/right variants of Shift, Control, Option, and Command. Fn/Globe remains a
single default trigger that accepts either representation used by Mac
keyboards. Escape, Caps Lock, media keys, Power, and Touch ID cannot be
assigned.

`Порог удержания` contains exactly three choices:

- `250 мс`
- `500 мс`
- `750 мс`

The active threshold has a checkmark. A new installation starts with Fn/Globe
and `500 мс`. Changing either choice persists it for future launches and applies
it to the next trigger-key press. A press already in progress keeps the key and
threshold that were active when the press began.

Pressing the trigger starts audio capture immediately. Releasing it before the
selected threshold aborts and discards the capture, then passes an equivalent
short press to macOS. Releasing it at or after the selected threshold finishes
the capture and follows the existing recognition and insertion flow; the key
press is not passed to macOS.

If another key is pressed while the trigger is held, PTT2me treats the input as
a keyboard combination regardless of elapsed hold time. It aborts and discards
the capture, replays the buffered trigger press and the other key in their
original order, and passes the remainder of that physical key sequence through.
Auto-repeat events from the trigger itself do not count as another key.

The menu continues to show the existing status and version rows and the
`Выйти` command.

## Design

### Preference model and persistence

A small threshold type owns the supported values, their Russian menu labels,
the `500 мс` default, and validation of persisted data. A trigger-key type owns
the physical key identity, display label, supported-key validation, and the
Fn/Globe default.

The preference boundary uses the standard macOS defaults store. Missing or
unsupported persisted values fall back independently to Fn/Globe and `500 мс`.

The application runtime loads the preference before creating the menu. Menu
selection updates the preference, the visible selection, and the values used by
future presses.

### Menu

`MenuBar` adds adjacent parent items titled `Клавиша активации` and
`Порог удержания`. The key submenu exposes assignment, reset, and the current
display label. The threshold submenu contains the three fixed choices. The menu
target reports commands to the runtime through a callback or channel; it does
not own hotkey, recording, or persistence state.

The menu projection updates the trigger display and the three threshold
checkmarks when preferences change. Status rendering remains independent of
preference rendering.

### Assignment mode

Assignment mode temporarily disables normal trigger handling. The event tap
classifies the next keyboard event:

- Escape cancels assignment.
- A supported physical key confirms assignment and consumes that selection
  press so it has no unrelated system effect.
- An unsupported key leaves the existing assignment unchanged, exits
  assignment mode, and is passed through to macOS.

The runtime returns to its permission-derived idle state after any of these
outcomes. Assignment cannot begin while recording or recognizing.

### Hotkey and recording flow

The event tap acts as a small keyboard-event gate. It suppresses and buffers the
physical trigger press while PTT2me decides whether the gesture is a short
press, dictation, or the beginning of a keyboard combination. All other events
pass through normally when no trigger decision is pending.

On the trigger press edge, the runtime snapshots the current trigger and
threshold, records the start time, and starts capture immediately.

On release:

- Below the snapshotted threshold, the reducer aborts capture and requests that
  the same physical key be replayed as a short system press.
- At or above the threshold, the reducer finishes capture using the existing
  release grace and recognition flow.

If a different key-down event arrives before trigger release, the gate switches
to combination pass-through. The runtime aborts capture. The gate replays the
buffered trigger-down followed by the different key-down, both with their
original flags, then lets subsequent physical events through until the trigger
is released. This preserves combinations such as Command+C. Trigger auto-repeat
is ignored while the decision is pending.

Replayed events carry a private source marker. The event tap passes marked
events through without tracking or suppressing them, preventing a replay loop.
Replay preserves left/right modifier identity and the selected physical key.

Tap loss, capture failure, shutdown, and other cancellation paths clear any
pending press without replaying it unless a physical release was observed and
classified as short or another key explicitly classified the sequence as a
combination.

### Error handling

Preference read and write failures do not block dictation. A read failure uses
the Fn/Globe and `500 мс` defaults; a write failure keeps the in-memory choice
for the current run and is logged.

If replaying a short press fails, PTT2me still aborts the short capture and
returns to the ready state. The replay failure is logged without showing a
recording error, because no dictation was requested.

## Testing

Automated tests cover:

- the exact supported values and default;
- supported and excluded trigger-key validation;
- left/right modifier identity and Fn/Globe compatibility;
- persisted threshold and trigger validation and fallback;
- the menu descriptor, trigger label, and selected checkmark projection;
- entering, cancelling, completing, and rejecting assignment;
- threshold snapshot behavior;
- immediate capture on press;
- short release aborting capture and requesting system replay;
- long release following the existing recognition path;
- a second key cancelling capture and preserving its combination order;
- trigger auto-repeat not cancelling capture;
- preservation of physical key and event flags during replay;
- marked replay events bypassing tracking and suppression;
- preference selection affecting only future presses;
- capture failure and event-tap loss clearing pending input safely.

The full Rust checks and bundle validation run after implementation.

Manual macOS testing verifies that:

- the assigned key and each threshold choice persist after relaunch;
- assignment mode accepts supported keys, rejects excluded keys, and cancels
  with Escape;
- short Fn/Globe presses still switch the configured input source when it is
  the trigger;
- short presses of an ordinary assigned key still reach the frontmost app;
- short presses do not produce recognized text;
- speech begun immediately after pressing the key is present in long-press
  recognition;
- long presses do not switch the input source;
- combinations using an assigned modifier, including Command+C and
  Option-generated characters, still work;
- repeated assignment, short presses, combinations, and dictation do not leave
  any key logically stuck.
