# Preserve Pasteboard During Text Insertion

## Goal

PTT2me must continue to paste recognized text into the frontmost application
with Command-V while preserving the complete pasteboard contents that existed
before the insertion.

After a successful insertion, a normal Command-V performed by the user must
paste the content that was present before PTT2me ran. This includes multiple
pasteboard items and every available representation of each item, such as plain
text, rich text, images, and file URLs.

## Current Behavior

`src/inserter.rs::insert_text` clears the general `NSPasteboard`, writes the
recognized plain text, sends Command-V, and intentionally leaves the recognized
text on the pasteboard. This destroys the user's previous pasteboard contents.

## Chosen Approach

Before modifying the general pasteboard, PTT2me will materialize an in-memory
snapshot of all current pasteboard items. Each saved item contains every type
reported by `NSPasteboardItem::types` and the binary data returned for that
type.

The insertion sequence is:

1. Normalize and validate the recognized text.
2. Read a complete snapshot of the general pasteboard.
3. Clear the general pasteboard and write the recognized text.
4. Record the pasteboard change count after the temporary text is installed.
5. Wait for the existing pre-paste settling delay.
6. Send the Command-V key-down and key-up events.
7. Wait 100 milliseconds so the frontmost application can consume the paste.
8. If the pasteboard change count still matches the recorded value, replace the
   temporary text with the saved snapshot.
9. If the change count differs, leave the pasteboard untouched because the user
   or another application has written newer content.

The same conditional restoration is attempted when keyboard-event creation or
posting fails after the temporary text has been installed.

## Components

### Pasteboard snapshot

A private snapshot model in `src/inserter.rs` will own copied bytes rather than
retaining references to source `NSPasteboardItem` objects. This avoids relying
on source item providers after the pasteboard has been cleared.

The snapshot preserves:

- an empty pasteboard;
- item order;
- type order within each item;
- the exact bytes for each available type.

Snapshot capture is all-or-nothing. If a reported type cannot be materialized,
insertion stops before clearing the pasteboard.

### Pasteboard restoration

Restoration creates new `NSPasteboardItem` objects, assigns every saved
type/data pair, clears the general pasteboard, and writes all reconstructed
items in their original order.

Construction happens before clearing the pasteboard. If an item cannot be
reconstructed, restoration returns an error without first destroying the
temporary or newly copied content.

An originally empty pasteboard is restored by clearing the temporary text and
leaving the pasteboard empty.

### Concurrent clipboard changes

PTT2me uses `NSPasteboard::changeCount` as an ownership token. Restoration is
allowed only while the temporary recognized text is still the newest
pasteboard write. A change made during either insertion delay wins and is never
overwritten by PTT2me.

Skipping restoration because the change count changed is not an insertion
error: the recognized text may already have been pasted, and the newer
pasteboard content is more important than the older snapshot.

## Error Handling

`InsertError` gains distinct snapshot and restoration failures.

- Empty recognized text fails before reading or writing the pasteboard.
- Snapshot failure fails before modifying the pasteboard.
- Temporary text write failure triggers a conditional attempt to restore the
  snapshot.
- Keyboard-event failure triggers a conditional attempt to restore the
  snapshot.
- Restoration failure is returned when insertion otherwise succeeded.
- If both insertion and restoration fail, the original insertion failure is
  retained for the caller and the restoration failure is logged without
  exposing pasteboard data.

The existing runtime continues to map insertion failures to its generic visible
insertion error and must not log pasteboard contents.

## Testing

The platform-independent orchestration decision will be separated from AppKit
access behind small private interfaces so unit tests can exercise real control
flow with an in-memory pasteboard implementation.

Tests will cover:

- capture and restoration of multiple items with multiple binary types;
- restoration of an originally empty pasteboard;
- aborting before mutation when snapshot capture is incomplete;
- restoring after a successful Command-V sequence;
- restoring after keyboard-event failure;
- preserving newer pasteboard content when the change count changes;
- reporting restoration failure after an otherwise successful insertion;
- the existing whitespace normalization behavior.

The full Rust test suite and formatting/lint checks used by the repository will
run after the focused tests pass.

## Documentation

`README.md` will state that recognized text is pasted without replacing the
user's existing pasteboard contents. The current claims that recognized text
remains on the pasteboard will be removed.

## Non-goals

- Maintaining a clipboard history.
- Adding menu items or settings.
- Replacing Command-V with Accessibility-based text editing.
- Restoring pasteboard contents after another process has written newer data.
- Persisting clipboard data to disk.
