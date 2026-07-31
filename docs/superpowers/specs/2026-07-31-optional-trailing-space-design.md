# Optional Trailing Space Design

## Goal

Let the user optionally append one space after each inserted dictation result so
successive dictated sentences remain separated. Preserve punctuation produced
by the ASR model.

## User Experience

The menu gains one checkable item titled `Пробел`.

- The item is unchecked by default on a new installation.
- Selecting it toggles whether one trailing space is appended to future
  insertions.
- The selected state persists across application restarts.

With the option disabled, recognized text is inserted using the existing
behavior. With the option enabled, exactly one ASCII space is appended after
the existing outer-whitespace normalization:

```text
Привет.  -> Привет. 
Привет!  -> Привет! 
Привет?  -> Привет? 
```

The application does not add, remove, or replace punctuation. The local
GigaAM RNNT call has no text prompt or punctuation instruction, so no ASR
configuration change is required.

## Design

### Preference Model and Persistence

A small output preference model owns the `append_space` boolean and its
default value of `false`.

The preference is stored in the standard macOS user-defaults store. A missing
or unreadable value falls back to `false`. A write failure does not block
dictation: the in-memory value remains active for the current run and the
failure is logged.

### Menu

`MenuBar` adds `Пробел` after the informational version row and before the
existing separator. It is a normal checkable menu command. Its checkmark is
updated immediately when selected.

The menu target reports the new boolean value to the runtime through a command
channel. It does not own persistence or text insertion.

### Insertion Flow

The runtime keeps the current output preference. When it handles an
`InsertText` effect, it passes the recognized text and current `append_space`
value to the insertion boundary.

The insertion boundary first retains the established behavior of trimming
outer whitespace and rejecting an empty result. It then appends exactly one
ASCII space when the option is enabled. This ordering is required because the
current insertion normalization would otherwise remove the requested trailing
space.

The resulting string is written temporarily to the pasteboard and pasted with
the existing Command-V path. Complete pasteboard snapshot, ownership checking,
and restoration behavior remain unchanged.

Changing the checkbox affects the next insertion that reaches the insertion
boundary, including recognition already in progress.

## Error Handling

- Missing or invalid persisted state uses the unchecked default.
- A persistence write failure is logged and keeps the selected in-memory state.
- Existing empty-text, pasteboard, keyboard-event, and restoration errors keep
  their current behavior.

## Testing

Automated tests cover:

- the unchecked default;
- loading and saving enabled and disabled values;
- fallback when the stored value is unavailable;
- the menu descriptor and checkbox projection;
- the menu command carrying the new selected value;
- disabled formatting preserving normalized model output;
- enabled formatting appending exactly one space after `.`, `!`, `?`, and
  unpunctuated text;
- whitespace-only recognition still being rejected;
- the temporary pasteboard text containing the requested trailing space;
- the existing full pasteboard restoration behavior remaining intact.

The final verification runs formatting, the full Rust test suite, Clippy, and
bundle validation.

Manual macOS verification confirms that the checkbox toggles immediately,
persists after relaunch, and separates two successive dictations in a
frontmost application.
