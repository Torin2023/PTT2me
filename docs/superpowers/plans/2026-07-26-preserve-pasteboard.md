# Preserve Pasteboard During Text Insertion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paste recognized text with Command-V, then restore every item and binary representation from the pasteboard state that existed before PTT2me started the insertion.

**Architecture:** Split insertion orchestration from AppKit pasteboard access inside `src/inserter.rs`. A small private pasteboard interface makes ownership-token and failure behavior testable in memory; the production adapter materializes all `NSPasteboardItem` type/data pairs and reconstructs them after the paste event.

**Tech Stack:** Rust 2021, `objc2` 0.5, `objc2-app-kit` 0.2, `objc2-foundation` 0.2, AppKit `NSPasteboard`.

## Global Constraints

- Preserve an empty pasteboard and every item, item order, declared type, type order, and byte payload available through `NSPasteboardItem::dataForType`.
- Abort before clearing the pasteboard when any declared representation cannot be captured.
- Use the existing 30 millisecond pre-paste settling delay and a 100 millisecond post-paste restoration delay.
- Restore only when `NSPasteboard::changeCount` still equals the token recorded immediately after PTT2me's temporary write.
- Never overwrite pasteboard content written by the user or another process during insertion.
- Do not persist pasteboard data to disk or log its contents.
- Keep Command-V as the insertion mechanism and retain the existing visible runtime error behavior.
- Target Apple Silicon and macOS 13 Ventura or newer.

---

### Task 1: Make restoration ownership and failure behavior testable

**Files:**
- Modify: `src/inserter.rs`
- Test: `src/inserter.rs`

**Interfaces:**
- Consumes: normalized recognized text and the existing `InsertError::{EmptyText, PasteboardWrite, EventSource, KeyboardEvent}` variants.
- Produces: private `PasteboardSnapshot`, `PasteboardAccess`, `PasteCommand`, `Sleeper`, `TemporaryWrite`, `TemporaryWriteFailure`, and `insert_with`; public `insert_text` remains `fn insert_text(&str) -> Result<(), InsertError>`.

- [ ] **Step 1: Write failing orchestration tests**

Before the test body, name the protected production mutations:

- removing the restore call after successful paste must fail
  `restores_original_snapshot_after_paste`;
- restoring without checking the ownership token must fail
  `does_not_overwrite_a_newer_pasteboard_change`;
- returning success after a restore error must fail
  `reports_restore_failure_after_successful_paste`;
- returning the restore error instead of the keyboard error must fail
  `restores_after_keyboard_failure_and_keeps_primary_error`;
- leaving a cleared pasteboard after a temporary string write failure must fail
  `restores_after_temporary_write_failure_and_keeps_primary_error`;
- mutating the pasteboard after an incomplete snapshot must fail
  `snapshot_failure_aborts_before_temporary_write`.

Add the wished-for private model and fake-driven tests to the existing
`#[cfg(test)]` module:

```rust
#[test]
fn restores_original_snapshot_after_paste() {
    let original = snapshot(&[
        &[("public.utf8-plain-text", b"before")],
        &[("public.png", &[0x89, 0x50, 0x4e, 0x47])],
    ]);
    let mut pasteboard = FakePasteboard::with_snapshot(original.clone());
    let mut command = FakePasteCommand::succeed();
    let mut sleeper = FakeSleeper::default();

    assert_eq!(
        insert_with("recognized", &mut pasteboard, &mut command, &mut sleeper),
        Ok(())
    );
    assert_eq!(pasteboard.current, original);
    assert_eq!(
        sleeper.delays,
        vec![PASTEBOARD_SETTLE_DELAY, PASTEBOARD_RESTORE_DELAY]
    );
}

#[test]
fn does_not_overwrite_a_newer_pasteboard_change() {
    let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
    let newer = snapshot(&[&[("public.utf8-plain-text", b"newer")]]);
    let mut pasteboard = FakePasteboard::with_snapshot(original);
    pasteboard.replace_before_ownership_check = Some(newer.clone());

    assert_eq!(
        insert_with(
            "recognized",
            &mut pasteboard,
            &mut FakePasteCommand::succeed(),
            &mut FakeSleeper::default(),
        ),
        Ok(())
    );
    assert_eq!(pasteboard.current, newer);
    assert_eq!(pasteboard.restore_calls, 0);
}

#[test]
fn reports_restore_failure_after_successful_paste() {
    let mut pasteboard = FakePasteboard::with_snapshot(PasteboardSnapshot::default());
    pasteboard.restore_error = Some(InsertError::PasteboardRestore);

    assert_eq!(
        insert_with(
            "recognized",
            &mut pasteboard,
            &mut FakePasteCommand::succeed(),
            &mut FakeSleeper::default(),
        ),
        Err(InsertError::PasteboardRestore)
    );
}

#[test]
fn restores_after_keyboard_failure_and_keeps_primary_error() {
    let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
    let mut pasteboard = FakePasteboard::with_snapshot(original.clone());
    pasteboard.restore_error = Some(InsertError::PasteboardRestore);

    assert_eq!(
        insert_with(
            "recognized",
            &mut pasteboard,
            &mut FakePasteCommand::fail(InsertError::KeyboardEvent),
            &mut FakeSleeper::default(),
        ),
        Err(InsertError::KeyboardEvent)
    );
    assert_eq!(pasteboard.restore_calls, 1);
}

#[test]
fn restores_after_temporary_write_failure_and_keeps_primary_error() {
    let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
    let mut pasteboard = FakePasteboard::with_snapshot(original);
    pasteboard.temporary_write_error = Some(InsertError::PasteboardWrite);

    assert_eq!(
        insert_with(
            "recognized",
            &mut pasteboard,
            &mut FakePasteCommand::succeed(),
            &mut FakeSleeper::default(),
        ),
        Err(InsertError::PasteboardWrite)
    );
    assert_eq!(pasteboard.restore_calls, 1);
}

#[test]
fn snapshot_failure_aborts_before_temporary_write() {
    let mut pasteboard = FakePasteboard::with_snapshot(PasteboardSnapshot::default());
    pasteboard.snapshot_error = Some(InsertError::PasteboardSnapshot);

    assert_eq!(
        insert_with(
            "recognized",
            &mut pasteboard,
            &mut FakePasteCommand::succeed(),
            &mut FakeSleeper::default(),
        ),
        Err(InsertError::PasteboardSnapshot)
    );
    assert_eq!(pasteboard.temporary_write_calls, 0);
}
```

Keep `FakePasteboard`, `FakePasteCommand`, `FakeSleeper`, and the `snapshot`
literal builder inside the test module. The fakes assert observable state; do
not assert that a mock object merely exists.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin inserter::tests::restores_original_snapshot_after_paste
```

Expected: compilation fails because `PasteboardSnapshot`, `insert_with`, and
the restoration error variants do not exist yet. Add only enough private type
declarations and test fakes to compile, then rerun until the test fails on the
missing restoration behavior rather than a typo.

Run the other four new test names individually and confirm that each fails for
the behavior named in Step 1.

- [ ] **Step 3: Add the minimal orchestration model**

Add:

```rust
const PASTEBOARD_RESTORE_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Eq, PartialEq, Default)]
struct PasteboardSnapshot {
    items: Vec<PasteboardItemSnapshot>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PasteboardItemSnapshot {
    representations: Vec<PasteboardRepresentation>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PasteboardRepresentation {
    type_name: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWrite {
    change_count: isize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWriteFailure {
    error: InsertError,
    change_count: isize,
}

trait PasteboardAccess {
    fn snapshot(&mut self) -> Result<PasteboardSnapshot, InsertError>;
    fn write_temporary_text(&mut self, text: &str)
        -> Result<TemporaryWrite, TemporaryWriteFailure>;
    fn change_count(&mut self) -> isize;
    fn restore(&mut self, snapshot: &PasteboardSnapshot) -> Result<(), InsertError>;
}

trait PasteCommand {
    fn send_command_v(&mut self) -> Result<(), InsertError>;
}

trait Sleeper {
    fn sleep(&mut self, duration: Duration);
}
```

Extend `InsertError` and its display strings:

```rust
PasteboardSnapshot, // "could not snapshot the pasteboard"
PasteboardRestore,  // "could not restore the pasteboard"
```

Implement the coordinator:

```rust
fn insert_with(
    text: &str,
    pasteboard: &mut impl PasteboardAccess,
    command: &mut impl PasteCommand,
    sleeper: &mut impl Sleeper,
) -> Result<(), InsertError> {
    let text = normalize_text(text).ok_or(InsertError::EmptyText)?;
    let snapshot = pasteboard.snapshot()?;
    let temporary = match pasteboard.write_temporary_text(&text) {
        Ok(temporary) => temporary,
        Err(failure) => {
            if pasteboard.change_count() == failure.change_count
                && pasteboard.restore(&snapshot).is_err()
            {
                tracing::warn!(
                    error_category = "pasteboard_restore_after_temporary_write_failure"
                );
            }
            return Err(failure.error);
        }
    };

    sleeper.sleep(PASTEBOARD_SETTLE_DELAY);
    let paste_result = command.send_command_v();
    if paste_result.is_ok() {
        sleeper.sleep(PASTEBOARD_RESTORE_DELAY);
    }

    let restore_result = if pasteboard.change_count() == temporary.change_count {
        pasteboard.restore(&snapshot)
    } else {
        Ok(())
    };

    match (paste_result, restore_result) {
        (Err(primary), Err(_restore)) => {
            tracing::warn!(error_category = "pasteboard_restore_after_insert_failure");
            Err(primary)
        }
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), restore) => restore,
    }
}
```

Keep the current concrete `insert_text` body unchanged during this task. Task
1 introduces and tests the private coordinator alongside it; Task 2 replaces
the old body with concrete adapters and makes `insert_with` the production
path. This keeps every task independently usable.

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run:

```bash
cargo test --target aarch64-apple-darwin inserter::tests
```

Expected: the six new coordinator tests and the three existing normalization
tests pass.

- [ ] **Step 5: Refactor while green**

Extract a private `restore_if_owned` helper only if it removes duplicated
ownership checks between the temporary-write failure cleanup and normal
cleanup. Keep all test doubles in `#[cfg(test)]`; do not add production methods
used only by tests.

Run:

```bash
cargo fmt --check
cargo test --target aarch64-apple-darwin inserter::tests
```

Expected: formatting and focused tests pass.

- [ ] **Step 6: Commit the tested coordinator**

Run:

```bash
git add src/inserter.rs
git commit -m "refactor: isolate pasteboard insertion flow"
```

Expected: the commit contains the orchestration boundary, fakes, and passing
behavioral tests, without changing the existing production `insert_text` path
or adding README/Cargo feature changes.

### Task 2: Materialize and restore complete AppKit pasteboard snapshots

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/inserter.rs`
- Test: `src/inserter.rs`

**Interfaces:**
- Consumes: Task 1's `PasteboardSnapshot` and `PasteboardAccess`.
- Produces: private `SystemPasteboard` backed by `NSPasteboardItem` type/data enumeration; production `insert_text` uses it.

- [ ] **Step 1: Enable the required generated AppKit/Foundation APIs**

Add `NSPasteboardItem` to `objc2-app-kit` features and `NSArray`, `NSData` to
`objc2-foundation` features:

```toml
objc2-foundation = { version = "0.2", features = [
  "NSArray", "NSData", "NSString", "NSURL", "NSTimer", "NSRunLoop", "NSThread",
] }
objc2-app-kit = { version = "0.2", features = [
  "NSApplication", "NSStatusBar", "NSStatusItem", "NSStatusBarButton",
  "NSMenu", "NSMenuItem", "NSPasteboard", "NSPasteboardItem", "NSWorkspace",
  "NSImage", "NSButton", "NSButtonCell", "NSControl", "NSResponder", "NSView",
  "NSCell", "NSColor", "NSRunningApplication",
] }
```

Run:

```bash
cargo check --target aarch64-apple-darwin
```

Expected: dependency feature resolution succeeds without changing package
versions in `Cargo.lock`.

- [ ] **Step 2: Write real AppKit round-trip tests on isolated pasteboards**

Name the protected mutations:

- dropping an item, type, or byte payload during capture/restore must fail
  `round_trips_multiple_items_and_representations`;
- failing to clear the temporary value for an originally empty pasteboard must
  fail `round_trips_an_empty_pasteboard`.

Use `NSPasteboard::pasteboardWithUniqueName()` so tests never touch the user's
general pasteboard:

```rust
#[test]
fn round_trips_multiple_items_and_representations() {
    let pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
    let expected = snapshot(&[
        &[
            ("public.utf8-plain-text", b"plain"),
            ("public.rtf", b"{\\rtf1 rich}"),
        ],
        &[("public.png", &[0x89, 0x50, 0x4e, 0x47])],
    ]);
    write_snapshot_fixture(&pasteboard, &expected);
    let mut system = SystemPasteboard::new(pasteboard);

    let captured = system.snapshot().unwrap();
    system.write_temporary_text("recognized").unwrap();
    system.restore(&captured).unwrap();

    assert_eq!(system.snapshot().unwrap(), expected);
}

#[test]
fn round_trips_an_empty_pasteboard() {
    let pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
    unsafe { pasteboard.clearContents() };
    let mut system = SystemPasteboard::new(pasteboard);

    let captured = system.snapshot().unwrap();
    system.write_temporary_text("recognized").unwrap();
    system.restore(&captured).unwrap();

    assert_eq!(system.snapshot().unwrap(), PasteboardSnapshot::default());
}
```

`write_snapshot_fixture` must call the same private reconstruction helper used
by production restoration, but the expected snapshot values remain literal and
independent of the capture code.

- [ ] **Step 3: Run the AppKit tests and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin inserter::tests::round_trips_multiple_items_and_representations
cargo test --target aarch64-apple-darwin inserter::tests::round_trips_an_empty_pasteboard
```

Expected: FAIL because `SystemPasteboard::snapshot` and `restore` do not yet
materialize and reconstruct AppKit items.

- [ ] **Step 4: Implement snapshot capture**

Import:

```rust
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString};
```

Implement `SystemPasteboard { pasteboard: Retained<NSPasteboard> }` and its
snapshot method with this data flow:

```rust
let items = unsafe { self.pasteboard.pasteboardItems() };
let Some(items) = items else {
    return Ok(PasteboardSnapshot::default());
};

let mut captured_items = Vec::with_capacity(items.len());
for item in items.iter() {
    let types = unsafe { item.types() };
    let mut representations = Vec::with_capacity(types.len());
    for pasteboard_type in types.iter() {
        let data = unsafe { item.dataForType(pasteboard_type) }
            .ok_or(InsertError::PasteboardSnapshot)?;
        representations.push(PasteboardRepresentation {
            type_name: pasteboard_type.to_string(),
            data: data.bytes().to_vec(),
        });
    }
    captured_items.push(PasteboardItemSnapshot { representations });
}
Ok(PasteboardSnapshot {
    items: captured_items,
})
```

Do not fall back to `stringForType`: all representations must remain binary
exact.

- [ ] **Step 5: Implement reconstruction and conditional production restore**

Build all new items before clearing:

```rust
fn reconstruct_items(
    snapshot: &PasteboardSnapshot,
) -> Result<Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>>, InsertError> {
    let mut objects = Vec::with_capacity(snapshot.items.len());
    for saved_item in &snapshot.items {
        let item = unsafe { NSPasteboardItem::new() };
        for saved in &saved_item.representations {
            let pasteboard_type = NSString::from_str(&saved.type_name);
            let data = NSData::with_bytes(&saved.data);
            if !unsafe { item.setData_forType(&data, &pasteboard_type) } {
                return Err(InsertError::PasteboardRestore);
            }
        }
        objects.push(ProtocolObject::from_retained(item));
    }
    Ok(NSArray::from_vec(objects))
}
```

Then:

```rust
let objects = reconstruct_items(snapshot)?;
unsafe { self.pasteboard.clearContents() };
if snapshot.items.is_empty() || unsafe { self.pasteboard.writeObjects(&objects) } {
    Ok(())
} else {
    Err(InsertError::PasteboardRestore)
}
```

`write_temporary_text` clears the pasteboard, writes
`NSPasteboardTypeString`, and returns `TemporaryWrite` with `changeCount`
sampled immediately after the write. If the string write fails after clearing,
return `TemporaryWriteFailure { error: InsertError::PasteboardWrite,
change_count }`, with `change_count` sampled immediately after the failed
write. `insert_with` conditionally restores the snapshot and retains
`PasteboardWrite` as the primary error.

`SystemPasteCommand` retains the existing Core Graphics key-down/key-up
implementation. `ThreadSleeper` delegates to `thread::sleep`.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
cargo test --target aarch64-apple-darwin inserter::tests
```

Expected: coordinator tests, real isolated-pasteboard round-trip tests, and
normalization tests all pass without changing the user's general pasteboard.

- [ ] **Step 7: Run a manual general-pasteboard smoke test**

Build the app with:

```bash
scripts/build-app.sh
```

Manually copy each of the following before one PTT insertion, then press
Command-V after the recognized text appears:

1. plain text;
2. formatted text copied from TextEdit;
3. an image copied from Preview;
4. two files copied together in Finder.

Expected: recognized text is inserted at the cursor, and the subsequent manual
Command-V reproduces the original content for all four cases. During one extra
insertion, copy new text immediately after releasing Fn; expected: the new text
remains in the pasteboard and is not replaced by the older snapshot.

- [ ] **Step 8: Commit the AppKit implementation**

Run:

```bash
git add Cargo.toml src/inserter.rs
git commit -m "feat: preserve pasteboard during insertion"
```

Expected: the commit contains the dependency feature flags, AppKit adapter, and
passing tests.

### Task 3: Update user-facing behavior documentation and verify the repository

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the completed insertion behavior from Tasks 1 and 2.
- Produces: README claims that match the shipped pasteboard behavior.

- [ ] **Step 1: Update the overview and privacy statements**

Replace the overview claim:

```text
Recognition uses the bundled GigaAM v3 model; the resulting text remains
on the pasteboard.
```

with:

```text
Recognition uses the bundled GigaAM v3 model; insertion preserves the previous
pasteboard contents.
```

Replace the Privacy sentence:

```text
Successful recognized text remains only in the macOS pasteboard after insertion.
```

with:

```text
Recognized text is used temporarily for insertion and is not retained by
PTT2me; the previous macOS pasteboard contents are restored unless newer
contents were copied during insertion.
```

- [ ] **Step 2: Run formatting, tests, and lint**

Run:

```bash
cargo fmt --check
cargo test --target aarch64-apple-darwin
cargo clippy --all-targets --target aarch64-apple-darwin -- -D warnings
git diff --check
```

Expected: all commands pass with no warnings or whitespace errors.

- [ ] **Step 3: Review the final diff against the spec**

Run:

```bash
git diff HEAD~2 -- Cargo.toml src/inserter.rs README.md
git status --short
```

Confirm:

- no pasteboard data is logged;
- the temporary text is not documented or implemented as persistent;
- `changeCount` gates every restoration after a temporary write;
- tests use unique named pasteboards rather than the general pasteboard;
- no unrelated files or menu behavior changed.

- [ ] **Step 4: Commit the documentation**

Run:

```bash
git add README.md
git commit -m "docs: describe pasteboard preservation"
```

Expected: the final task commit contains only `README.md`.
