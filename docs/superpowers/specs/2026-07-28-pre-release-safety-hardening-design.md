# PTT2me Pre-release Safety Hardening Design

**Date:** 2026-07-28  
**Target release:** 1.0.3  
**Status:** Approved product direction; written specification pending final review

## Goal

Close the release-blocking QA findings without changing PTT2me's local,
single-model product contract. The result must insert into the field that owns
the cursor when recognition finishes, preserve short Fn/Globe as a system
action, and prevent pasteboard restoration from being lost on failures.

## Product decisions

1. PTT2me does not pin the application or input field at recording start.
   Immediately before insertion it uses the currently focused editable field.
2. A short Fn/Globe press, below 250 ms, remains available to macOS, including
   the user's configured input-source switch action.
3. Holding Fn/Globe for at least 250 ms starts the PTT interaction without also
   sending Fn/Globe to the system.
4. Recognition remains fully local and uses only the frozen model bundled in
   the application. There is no model selector, download, or network fallback.
5. Text insertion tries Accessibility first, Unicode keyboard events second,
   and clipboard plus Command-V last. The clipboard fallback is retained for
   compatibility and must pass the explicit ChatGPT release regression.
6. When a required permission is missing, the menu provides a repeatable
   "Открыть настройки…" action for the exact missing permission.

## Architecture

### Current-focus insertion

Create a focused `text_inserter` module that owns insertion-method selection.
At the time `Effect::InsertText` is executed it queries the current system-wide
focused Accessibility element. It does not retain an application, process, or
element captured at Fn press or release.

The method order is:

1. **AX selected-text insertion.** Reject secure text fields as a terminal
   insertion error with no fallback. For other fields, require the focused
   element's selected-text attribute to be settable, then set `AXSelectedText`
   to the recognized string. This replaces the current selection or inserts at
   the current cursor without rewriting the complete field value.
2. **Unicode event insertion.** If the focused element cannot accept
   `AXSelectedText`, post chunked Unicode keyboard events through CoreGraphics.
   This preserves selection semantics while avoiding the pasteboard. Event
   source or event construction failure proceeds to the clipboard fallback.
   CoreGraphics provides no target-consumption acknowledgement; once all
   events are posted, the method reports completion and manual target testing
   is the acceptance evidence.
3. **Clipboard fallback.** If Unicode event construction is unavailable, use
   the existing full-pasteboard snapshot and Command-V transaction. Retain the
   recognized temporary clipboard value for 1,000 ms before guarded restore,
   rather than the current 100 ms. Restore only when `changeCount` still proves
   ownership; never overwrite a clipboard change made by the user or target
   application.

The module returns one of:

- `InsertOutcome::Complete(InsertMethod)` for AX or Unicode success;
- `InsertOutcome::PendingClipboard(PendingInsertion)` for the asynchronous
  clipboard fallback;
- `InsertError` when no method can begin.

`Runtime` immediately dispatches `PasteFinished(Ok(()))` for a complete direct
insertion. Only `PendingClipboard` enters the timer-based paste flow.

The 1,000 ms clipboard delay is a compatibility budget, not proof that every
target consumes a paste synchronously. Therefore the release gate must verify
ChatGPT with a non-empty sensitive-data marker in the original clipboard.

### Panic-safe clipboard ownership

Make the clipboard transaction an RAII owner:

- `InsertionTransaction` records whether restoration is still required.
- A successful guarded restore marks the transaction complete.
- `Drop` performs one best-effort guarded restore while restoration remains
  required.
- Explicit paste failure restores immediately and preserves the primary error.
- Runtime shutdown explicitly restores pending clipboard state, while `Drop`
  remains the final unwind safeguard.

Timer dispatch must not permanently remove the only restoration owner from
`Runtime`. If processing temporarily moves the transaction into a local guard,
that guard restores on unwind before it can be dropped.

### Short Fn and long Fn

Introduce a pure `FnPressPolicy` state machine with these states:

- `Idle`
- `Pressed { pressed_at, keycode }`

On physical Fn/Globe down:

1. suppress the physical event;
2. record its exact keycode and callback timestamp;
3. start provisional audio capture immediately so the first 250 ms of speech
   are not clipped.

On physical release, calculate the hold duration from callback timestamps. If
the duration is below 250 ms:

1. abort and discard provisional audio;
2. return the controller to Ready;
3. replay a marked synthetic short Fn/Globe press and release using the
   original keycode;
4. let those marked synthetic events pass through the event tap without
   generating PTT signals.

If the duration is at least 250 ms:

1. keep the already captured audio;
2. finish capture and recognize normally;
3. do not replay Fn/Globe to macOS.

The existing Recording status may be visible during a short provisional press.
No ASR or insertion is allowed for that short path. This avoids a new timer and
keeps the current no-clipping capture behavior.

Synthetic events carry a private CoreGraphics source-user-data marker. The
event-tap callback checks this marker before Fn tracking and suppression, which
prevents recursion and duplicate PTT signals.

Synthetic Fn behavior is an OS integration assumption and cannot be accepted
from unit tests alone. The release requires a physical-key test on macOS 26.
If macOS does not honor marked replay as the configured short-Fn system action,
release 1.0.3 must use a separate PTT chord and leave plain Fn/Globe entirely
untouched.

### Permission recovery action

Keep a stable menu layout by adding one permanent permission-action row between
the version and separator rows. It is:

- titled `Открыть настройки…`;
- enabled and visible only while status is
  `PermissionBlocked(PermissionKind)`;
- bound to the exact permission currently reported by the controller;
- reusable after each revoke/grant cycle.

The AppKit target sends a `MenuAction::OpenPermission(PermissionKind)` through
a main-thread channel. Runtime drains menu actions with its normal event loop
and calls the existing stateless `permissions::open_settings` function
directly, so a manual click is never suppressed by the automatic one-shot
prompt ledger. Permission polling remains responsible for returning the UI to
`Готово`.

## Error handling and safety invariants

1. No insertion method may log recognized text or existing field content.
2. AX insertion never uses whole-value replacement and never writes secure
   fields.
3. Unicode insertion is chunked; construction failure selects the clipboard
   fallback, while posted events remain bound to the focus current at posting.
4. Clipboard restore never overwrites a newer `changeCount`.
5. Every clipboard transaction has exactly one live restoration owner.
6. A panic, paste error, timer cancellation, or application shutdown leaves
   the original clipboard restored when ownership is still held.
7. Replayed Fn events cannot re-enter PTT tracking.
8. A short Fn press produces no ASR command and no text insertion.
9. The insertion target is intentionally the field focused at insertion time.

## Automated verification

Add deterministic tests for:

- AX selected-text success bypasses Unicode and clipboard;
- secure fields reject AX insertion;
- unsupported AX falls through to Unicode;
- failed Unicode falls through to clipboard;
- direct insertion completes without paste timers;
- clipboard fallback uses a 1,000 ms restore timer;
- clipboard restoration occurs when timer handling unwinds;
- explicit restore and `Drop` do not restore twice;
- premature and duplicate paste timers have no side effects;
- short Fn aborts provisional capture and emits exactly one marked replay;
- long Fn retains provisional audio and emits no system replay;
- replay-marked events bypass suppression and PTT tracking;
- the permission menu action tracks Accessibility, Input Monitoring, and
  Microphone across revoke/grant cycles.

All existing tests, formatting, clippy with denied warnings, release build,
bundle validation, model smoke, and dependency audit remain mandatory.

## Manual P0 release gate

Run on Apple Silicon with macOS 26 and all three TCC permissions:

1. With `CLIPBOARD-НЕ-ВСТАВЛЯТЬ` in the clipboard, dictate into ChatGPT.
   The dictated text appears and the marker does not. The following manual
   Command-V restores the marker.
2. Repeat insertion in a native `NSTextView`, HTML `input`, HTML `textarea`,
   contenteditable, Telegram, and Discord.
3. Verify rich text, image, and Finder file-URL clipboard snapshots survive a
   clipboard-fallback insertion byte-for-byte.
4. Perform 20 short Fn/Globe presses. Every press performs the configured
   macOS system action; none starts ASR or insertion.
5. Perform 20 long Fn/Globe holds. Every hold records and recognizes; none
   performs the short system action.
6. Revoke Accessibility, Input Monitoring, and Microphone one at a time. The
   menu action opens the correct pane each time and the app returns to
   `Готово` after the grant is restored.
7. Change the focused field during recognition and verify insertion occurs at
   the cursor's final location.

Failure of any item blocks the public release.

## Distribution gate

The public artifact remains the fixed-model arm64 application. Developer ID
signing and notarization are required for a normal public release. If those
credentials are unavailable, the GitHub release must be explicitly marked as
an unsigned preview and retain the documented Gatekeeper installation warning.
