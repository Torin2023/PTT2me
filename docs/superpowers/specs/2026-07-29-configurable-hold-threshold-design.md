# Configurable Hold Threshold Design

## Goal

Let the user choose how long Fn/Globe must be held before PTT2me treats the
gesture as dictation, while preserving the beginning of speech and allowing a
short press to retain its macOS input-source behavior.

## User Experience

The menu gains a `Порог удержания` submenu with exactly three choices:

- `250 мс`
- `500 мс`
- `750 мс`

The active choice has a checkmark. A new installation starts at `500 мс`.
Changing the choice persists it for future launches and applies it to the next
Fn/Globe press. A press already in progress keeps the threshold that was active
when the press began.

Pressing Fn/Globe starts audio capture immediately. Releasing it before the
selected threshold aborts and discards the capture, then passes an equivalent
short Fn/Globe press to macOS. Releasing it at or after the selected threshold
finishes the capture and follows the existing recognition and insertion flow;
the key press is not passed to macOS.

The menu continues to show the existing status and version rows and the
`Выйти` command.

## Design

### Threshold model and persistence

A small threshold type owns the supported values, their Russian menu labels,
the `500 мс` default, and validation of persisted data. The preference boundary
uses the standard macOS defaults store. Missing or unsupported persisted values
fall back to `500 мс`.

The application runtime loads the preference before creating the menu. Menu
selection updates the preference, the checkmarks, and the threshold used by
future presses.

### Menu

`MenuBar` adds a parent item titled `Порог удержания` with a submenu containing
the three fixed choices. The menu target reports a selected threshold to the
runtime through a callback or channel; it does not own recording state.

The menu projection updates only the three checkmarks when the choice changes.
Status rendering remains independent of preference rendering.

### Hotkey and recording flow

The event tap suppresses the physical Fn/Globe events while PTT2me decides
whether the gesture is short or long. On the press edge, the runtime snapshots
the current threshold, records the start time, and starts capture immediately.

On release:

- Below the snapshotted threshold, the reducer aborts capture and requests that
  the same Fn/Globe key be replayed as a short system press.
- At or above the threshold, the reducer finishes capture using the existing
  release grace and recognition flow.

The hotkey signal retains whether the physical key was Fn or Globe so the short
press can replay the corresponding key. Replayed events carry a private marker.
The event tap passes marked events through without tracking or suppressing
them, preventing a replay loop.

Tap loss, capture failure, shutdown, and other cancellation paths clear any
pending press without replaying it unless a physical release was observed and
classified as short.

### Error handling

Preference read and write failures do not block dictation. A read failure uses
the `500 мс` default; a write failure keeps the in-memory choice for the current
run and is logged.

If replaying a short press fails, PTT2me still aborts the short capture and
returns to the ready state. The replay failure is logged without showing a
recording error, because no dictation was requested.

## Testing

Automated tests cover:

- the exact supported values and default;
- persisted-value validation and fallback;
- the menu descriptor and selected checkmark projection;
- threshold snapshot behavior;
- immediate capture on press;
- short release aborting capture and requesting system replay;
- long release following the existing recognition path;
- preservation of Fn versus Globe identity;
- marked replay events bypassing tracking and suppression;
- preference selection affecting only future presses.

The full Rust checks and bundle validation run after implementation.

Manual macOS testing verifies that:

- each menu choice persists after relaunch;
- short Fn/Globe presses still switch the configured input source;
- short presses do not produce recognized text;
- speech begun immediately after pressing the key is present in long-press
  recognition;
- long presses do not switch the input source;
- repeated short and long presses do not leave Fn/Globe logically stuck.
