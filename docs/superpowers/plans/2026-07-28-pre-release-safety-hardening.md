# PTT2me Pre-release Safety Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the QA release blockers by inserting through the current focused field, restoring clipboard state safely on every exit path, preserving short Fn/Globe for macOS, and exposing repeatable permission recovery.

**Architecture:** Add a policy-driven text insertion layer that selects AX, Unicode events, or the existing asynchronous clipboard transaction. Keep macOS APIs behind narrow boundaries and test the selection/state machines with deterministic doubles. Extend the existing reducer/runtime rather than adding preferences or network behavior.

**Tech Stack:** Rust 2021, ApplicationServices Accessibility C API, CoreGraphics keyboard events, AppKit menu items, Core Foundation run-loop timers.

## Global Constraints

- The insertion target is the field focused when `Effect::InsertText` executes; never pin an earlier application or element.
- A short Fn/Globe press is replayed to macOS; a hold of at least 250 ms is PTT and is not replayed.
- Recognition uses only the frozen bundled model; no download, selection, or network fallback.
- AX insertion never writes a secure field or replaces the complete `AXValue`.
- Insertion order is AX selected text, Unicode keyboard events, then clipboard plus Command-V.
- Clipboard fallback restores after 1,000 ms only while `changeCount` proves ownership.
- Preserve every pasteboard item, type, and byte; never overwrite a newer user/application change.
- Support Apple Silicon and macOS 13 or later.
- No settings, history, telemetry, or target pinning.

---

### Task 1: Panic-safe clipboard transaction and timer coordinator

**Files:**
- Modify: `src/inserter.rs`
- Modify: `src/runtime.rs`

**Interfaces:**
- Consumes: `PasteboardAccess::restore`, `PendingInsertion`, and the existing `PasteFlowBoundary`.
- Produces: idempotent `PendingInsertion::restore`; best-effort RAII restoration; a coordinator that retains a restoration owner across unwind.

- [ ] **Step 1: Write failing idempotence and unwind tests**

Add an `active` restore ledger to the test pasteboard fixture and tests equivalent to:

```rust
#[test]
fn explicit_restore_then_drop_restores_once() {
    let restores = Rc::new(Cell::new(0));
    {
        let mut insertion = transaction_with_restore_counter(Rc::clone(&restores));
        insertion.restore().unwrap();
    }
    assert_eq!(restores.get(), 1);
}

#[test]
fn dropping_active_transaction_restores_once() {
    let restores = Rc::new(Cell::new(0));
    drop(transaction_with_restore_counter(Rc::clone(&restores)));
    assert_eq!(restores.get(), 1);
}
```

Extend the runtime paste-flow test with a boundary that panics during timer scheduling and assert through `catch_unwind` that dropping the local flow restores its insertion.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test --features test-support inserter::tests::explicit_restore_then_drop_restores_once
cargo test --features test-support inserter::tests::dropping_active_transaction_restores_once
cargo test --features test-support runtime::tests::paste_flow_restores_if_boundary_panics
```

Expected: tests fail because restore is not idempotent and the current test insertion has no RAII restoration.

- [ ] **Step 3: Implement an idempotent RAII transaction**

Constrain the struct as
`InsertionTransaction<P: PasteboardAccess, C>` and add
`restore_required: bool`. `restore()` returns
`Ok(())` when already complete, calls guarded pasteboard restore otherwise,
and clears the flag only after `Ok(())`. Implement:

```rust
impl<P: PasteboardAccess, C> Drop for InsertionTransaction<P, C> {
    fn drop(&mut self) {
        if self.restore_required
            && self
                .pasteboard
                .restore(&self.snapshot, self.temporary.change_count)
                .is_err()
        {
            tracing::warn!(error_category = "pasteboard_restore_on_drop");
        }
        self.restore_required = false;
    }
}
```

Keep explicit paste-failure restoration and the primary `InsertError`.

- [ ] **Step 4: Make runtime flow unwind-safe**

Keep `PasteFlow<PendingInsertion>` as the owner while a timer is processed.
When it must be temporarily moved out of `Runtime`, rely on the RAII transaction
to restore during unwind. The normal path puts unfinished flow back; finished
flow has already completed restoration.

- [ ] **Step 5: Verify GREEN and commit**

Run the three focused tests plus:

```bash
cargo test --features test-support inserter::tests runtime::tests::paste_flow
```

Commit:

```bash
git add src/inserter.rs src/runtime.rs
git commit -m "fix: make paste flow unwind safe"
```

Keep the existing timer-sequence commit unchanged because later specification
and plan commits already follow it.

### Task 2: AX-first insertion policy with Unicode and clipboard fallback

**Files:**
- Create: `src/text_inserter.rs`
- Modify: `src/lib.rs`
- Modify: `src/inserter.rs`

**Interfaces:**
- Consumes: `PendingInsertion::begin(&str)`.
- Produces:

```rust
pub(crate) enum InsertMethod {
    Accessibility,
    UnicodeEvents,
}

pub(crate) enum InsertOutcome {
    Complete(InsertMethod),
    PendingClipboard(PendingInsertion),
}

pub(crate) trait AccessibilityInsertion {
    fn insert_selected_text(&mut self, text: &str) -> Result<bool, InsertError>;
}

pub(crate) trait UnicodeInsertion {
    fn insert_unicode(&mut self, text: &str) -> Result<bool, InsertError>;
}
```

`Ok(true)` means complete, `Ok(false)` means unsupported and selects the next
method, and `Err(InsertError::SecureField)` is terminal.

Extend `InsertError` with `SecureField`, `Accessibility`, and `UnicodeEvent`.
Map missing focus, unsupported AX attributes, and non-settable selected text to
`Ok(false)` so Unicode can run; reserve `Accessibility` for malformed owned
Core Foundation values or an AX set operation that was declared settable but
failed.

- [ ] **Step 1: Write failing insertion-order tests**

Use recording doubles and add tests proving:

```rust
assert_eq!(
    begin_with("текст", ax_success(), unicode_unreachable(), clipboard_unreachable()),
    Ok(InsertOutcomeKind::Complete(InsertMethod::Accessibility))
);
assert_eq!(calls(), ["ax"]);
```

Add separate tests for AX unsupported to Unicode, Unicode unsupported to
clipboard, and secure AX field to terminal error without later calls.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --features test-support text_inserter::tests
```

Expected: compile failure because `text_inserter` and its policy do not exist.

- [ ] **Step 3: Implement the pure policy**

Implement `begin_with` first using only the traits and a clipboard factory:

```rust
match ax.insert_selected_text(text) {
    Ok(true) => Ok(InsertOutcome::Complete(InsertMethod::Accessibility)),
    Err(error) => Err(error),
    Ok(false) => match unicode.insert_unicode(text)? {
        true => Ok(InsertOutcome::Complete(InsertMethod::UnicodeEvents)),
        false => PendingInsertion::begin(text).map(InsertOutcome::PendingClipboard),
    },
}
```

Do not expose recognized text through debug output.

- [ ] **Step 4: Implement the system AX boundary**

Link `ApplicationServices` and wrap:

```rust
AXUIElementCreateSystemWide
AXUIElementCopyAttributeValue
AXUIElementIsAttributeSettable
AXUIElementSetAttributeValue
```

Read `AXFocusedUIElement` at call time. Read `AXRole`; return
`InsertError::SecureField` for `AXSecureTextField`. For other roles, require
`AXSelectedText` to be settable and set it from a retained `CFString`.
Release every Create/Copy-owned Core Foundation object.

- [ ] **Step 5: Implement the Unicode boundary**

Create a private HID event source and split the Rust string on `char`
boundaries into bounded chunks. Construct every key-down/key-up event and call
`CGEvent::set_string` before posting any of them. If construction fails, drop
the complete unposted vector and return `Ok(false)` so clipboard fallback
cannot duplicate a partially typed prefix. Once construction succeeds, post
the complete vector to `CGEventTapLocation::HID`.

- [ ] **Step 6: Verify GREEN and commit**

Run:

```bash
cargo test --features test-support text_inserter::tests
cargo clippy --lib --features test-support -- -D warnings
```

Commit:

```bash
git add src/text_inserter.rs src/lib.rs src/inserter.rs
git commit -m "feat: insert through focused accessibility field"
```

### Task 3: Runtime integration and 1,000 ms compatibility fallback

**Files:**
- Modify: `src/inserter.rs`
- Modify: `src/runtime.rs`
- Modify: `src/state.rs`
- Test: `tests/ptt_flow.rs`

**Interfaces:**
- Consumes: `text_inserter::begin`, `InsertOutcome`.
- Produces: immediate `PasteFinished(Ok(()))` for direct methods and the existing timer flow only for `PendingClipboard`.

- [ ] **Step 1: Write failing runtime outcome tests**

Extract an insertion outcome handler and test:

```rust
assert_eq!(
    effects_for(InsertOutcomeKind::Complete),
    RuntimeInsertAction::FinishNow
);
assert_eq!(
    effects_for(InsertOutcomeKind::PendingClipboard),
    RuntimeInsertAction::SchedulePaste { delay_ms: 30 }
);
```

Change the paste-flow timing contract test to require a 1,000 ms restore timer.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --features test-support runtime::tests::direct_insertion_finishes_without_timer
cargo test --features test-support runtime::tests::paste_flow_orders_command_restore_finish_and_hotkey_drain
```

Expected: missing direct-outcome handler and restore delay mismatch (`100` vs
`1_000`).

- [ ] **Step 3: Integrate the outcome**

Replace direct `PendingInsertion::begin` in `Effect::InsertText` with
`text_inserter::begin`. Dispatch success immediately for direct insertion;
start `PasteFlow` only for clipboard fallback; map all errors to the existing
recoverable insertion error without logging text.

- [ ] **Step 4: Change and document fallback timing**

Set `PASTEBOARD_RESTORE_DELAY_MS` to `1_000`. Keep the 30 ms pre-paste settle,
the `changeCount` guard, hotkey drain after completion, and full multi-item
pasteboard reconstruction.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
cargo test --all-targets --features test-support
```

Commit:

```bash
git add src/inserter.rs src/runtime.rs src/state.rs tests/ptt_flow.rs
git commit -m "fix: prefer direct focused text insertion"
```

### Task 4: Replay short Fn/Globe without re-entering PTT

**Files:**
- Modify: `src/hotkey.rs`
- Modify: `src/runtime.rs`

**Interfaces:**
- Consumes: callback timestamps and `MIN_HOLD_MS`.
- Produces:

```rust
enum FnReleaseAction {
    FinishPtt { observed_at: Instant },
    AbortAndReplay { observed_at: Instant, keycode: u16 },
}
```

Synthetic events carry `EventField::EVENT_SOURCE_USER_DATA =
0x5054_5432_4D45` (`"PTT2ME"`).

- [ ] **Step 1: Write failing policy tests**

Add tests proving a 249 ms release returns exactly one `AbortAndReplay`, a
250 ms release returns `FinishPtt`, duplicate edges do nothing, and marked
events return unchanged without tracker observation or suppression.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --features test-support hotkey::tests::short_fn_requests_one_system_replay
cargo test --features test-support hotkey::tests::long_fn_is_ptt_without_system_replay
cargo test --features test-support hotkey::tests::marked_replay_bypasses_ptt_tracking
```

Expected: missing release policy, marker, and replay action.

- [ ] **Step 3: Implement pure release classification**

Store the pressed timestamp and keycode in `FnTracker`. On release compute
elapsed milliseconds with checked subtraction and classify against
`MIN_HOLD_MS`. Continue sending the existing timestamped release signal to
Runtime so its reducer aborts short capture and recognizes long capture.

- [ ] **Step 4: Implement marked event replay**

After a short release, create Fn/Globe down and up events with the original
keycode, set the private source-user-data marker and Fn flag sequence, and post
them at the HID tap. At callback entry, pass marked events through before
calling the tracker or `should_suppress`.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
cargo test --features test-support hotkey::tests runtime::tests::delayed_hotkey
cargo clippy --lib --features test-support -- -D warnings
```

Commit:

```bash
git add src/hotkey.rs src/runtime.rs
git commit -m "fix: preserve short Fn system action"
```

### Task 5: Repeatable permission-settings menu action

**Files:**
- Modify: `src/menu.rs`
- Modify: `src/runtime.rs`
- Modify: `src/state.rs`

**Interfaces:**
- Produces:

```rust
pub(crate) enum MenuAction {
    OpenPermission(PermissionKind),
}

pub(crate) fn take_action(&self) -> Option<MenuAction>;
```

- [ ] **Step 1: Write failing menu projection and cycle tests**

Require a permanent `PermissionSettings` descriptor row. Test that it is
hidden/disabled for Ready, visible/enabled for each
`PermissionBlocked(PermissionKind)`, and tracks a revoke/grant/revoke cycle.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --features test-support menu::tests::permission_action_tracks_missing_permission
cargo test --features test-support state::tests::granted_permission_can_be_opened_again_after_revocation
```

Expected: menu descriptor lacks the row and action projection.

- [ ] **Step 3: Implement stable AppKit row and channel**

Add `MenuEntry::PermissionSettings`. Keep the item allocated for the lifetime
of `MenuBar`; update title, hidden state, enabled state, and current
`PermissionKind` during `render`. `MenuTarget` sends the exact current
permission through an `mpsc::Sender<MenuAction>`.

- [ ] **Step 4: Drain actions in Runtime**

Drain menu actions with the existing event drain timer. Route
`MenuAction::OpenPermission(permission)` directly through the stateless
`permissions::open_settings(permission)` function. Do not route manual clicks
through `MicrophonePermissionFlow`, whose automatic settings opening is
intentionally one-shot while permission remains denied.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
cargo test --all-targets --features test-support
```

Commit:

```bash
git add src/menu.rs src/runtime.rs src/state.rs
git commit -m "feat: reopen required privacy settings"
```

### Task 6: Documentation and release-gate verification

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/plans/2026-07-28-audit-remediation.md`
- Verify: all release branch files

**Interfaces:**
- Produces: explicit current-focus behavior, short/long Fn behavior, insertion
  fallback disclosure, and a recorded manual P0 checklist.

- [ ] **Step 1: Update product documentation**

Document:

- insertion targets the cursor position current when recognition finishes;
- short Fn/Globe remains a macOS action and a hold of at least 250 ms is PTT;
- insertion prefers Accessibility and Unicode events, with guarded clipboard
  fallback;
- the bundled model is fixed and never downloaded;
- the permission menu action is available while access is missing.

- [ ] **Step 2: Run the full automated release gate**

Run:

```bash
cargo fmt --all -- --check
cargo test --all-targets --features test-support
cargo clippy --all-targets --features test-support -- -D warnings
cargo build --release --target aarch64-apple-darwin
cargo audit
git diff --check origin/main...HEAD
```

- [ ] **Step 3: Build and verify the fixed-model bundle**

In a clean release worktree containing the four pre-provisioned model files:

```bash
scripts/build-dmg.sh
shasum -a 256 -c dist/PTT2me-1.0.3-macos-arm64.dmg.sha256
```

The bundle checker must initialize the embedded model under the existing
180-second watchdog. No model download is allowed.

- [ ] **Step 4: Run the manual P0 gate**

Record results for ChatGPT with `CLIPBOARD-НЕ-ВСТАВЛЯТЬ`, native text view,
HTML input/textarea/contenteditable, Telegram, Discord, rich text, image, file
URL, 20 short Fn presses, 20 long Fn holds, every permission revoke/grant
cycle, and focus change during recognition.

If synthetic short Fn does not invoke the configured macOS action reliably,
stop release and replace plain Fn PTT with a separate fixed chord before
repeating the gate.

- [ ] **Step 5: Review, commit, and continue the public release**

Request independent review of the complete diff. Resolve every blocker, rerun
the affected tests, then:

```bash
git add README.md docs/superpowers/plans/2026-07-28-audit-remediation.md
git commit -m "docs: update pre-release safety contract"
```

Push, open the public PR, wait for green CI, merge, and publish the arm64 DMG
only after the manual P0 record is complete. Use a normal public release only
with Developer ID signing and notarization; otherwise mark it explicitly as an
unsigned preview with the Gatekeeper warning.
