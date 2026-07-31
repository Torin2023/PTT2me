# PTT2me

PTT2me is a minimal, fully local macOS menu-bar app: hold Fn/Globe, speak,
release the key, and the recognized Russian text is inserted at the cursor's
current location. Recognition uses the bundled GigaAM v3 model. Insertion
prefers the focused Accessibility text field and preserves the previous
pasteboard whenever compatibility requires Command-V fallback.

## Requirements and workflow

- Apple Silicon (`arm64`) Mac
- macOS 13 Ventura or newer

PTT2me loads its fixed model at startup. When the status becomes `Готово`,
hold Fn/Globe while speaking and release it. Fn/Globe and a 500 ms hold
threshold are the default settings. Recording begins immediately when the
trigger is pressed; a hold that reaches the selected threshold is recognized
on release. The app keeps recording for another 180 ms, recognizes the
captured phrase, and inserts a
non-empty result into whichever editable field owns the cursor at that moment.
A capture ends automatically after 25 seconds.

Insertion first tries the focused field's Accessibility selected-text
attribute, then direct Unicode keyboard events. If neither method is
available, PTT2me temporarily uses the full macOS pasteboard and Command-V,
then performs a guarded restore after one second. A newer pasteboard change is
never overwritten.

A short press is preserved: PTT2me replays it once to macOS, so the normal
Fn/Globe input-source action still works. The same applies to a short press of
any assigned ordinary key. If the assigned key is used in a combination, the
combination is passed through in its original order instead of starting
dictation.

The menu contains the trigger controls:

```text
<status>
PTT2me 1.0.4
Открыть настройки…   (only while a required permission is missing)
Клавиша активации
  <selected key>
  Назначить…
  Сбросить на Fn / Globe
Порог удержания
  250 мс
  500 мс
  750 мс
Пробел в конце
────────────
Выйти
```

The status and version rows are informational. While a permission is missing,
`Открыть настройки…` opens its exact Privacy & Security pane and can be used
repeatedly.

Choose `Клавиша активации` → `Назначить…`, then press the key to use for
dictation. Press Escape to cancel assignment. `Сбросить на Fn / Globe` restores
the default trigger. Escape, Caps Lock, media keys, Power, and Touch ID are
not accepted as assigned triggers and leave the current trigger unchanged.

Choose one of the exact `Порог удержания` values: 250, 500, or 750 ms. A hold
shorter than the selected value is replayed normally; a longer hold records
and is consumed so it does not type or invoke its usual system action.

`Пробел в конце` is disabled by default. When enabled, PTT2me appends one
ASCII space to each non-empty recognized phrase after trimming outer
whitespace. The option persists across application restarts. Recognition
punctuation is never added, removed, or rewritten by PTT2me.

`Выйти` terminates the app.

## Build

Place these four non-empty build assets in
`vendor/models/gigaam-v3-rnnt/`:

```text
encoder.int8.onnx
decoder.onnx
joiner.onnx
tokens.txt
```

Then run:

```bash
scripts/build-app.sh
```

The script builds only `aarch64-apple-darwin`, obtains the two native runtime
libraries from the Cargo release output, and creates the self-contained
`dist/PTT2me.app`.

## Automated checks

Pull requests and pushes to `main` run on an Apple Silicon macOS runner. The
workflow checks formatting, runs all unit and integration tests (including the
main-thread NSPasteboard round trip), denies Clippy warnings, and audits locked
Rust dependencies:

```bash
cargo fmt --all -- --check
cargo test --all-targets --features test-support -- --test-threads=1
cargo clippy --all-targets --features test-support -- -D warnings
cargo audit --deny warnings
```

These checks compile and test the program; they do not download or substitute
an ASR model. PTT2me has one fixed GigaAM v3 RNNT model, supplied as frozen
build assets and embedded in the application bundle.

## Local DMG release

To rebuild the app and create a local Apple Silicon DMG, run:

```bash
scripts/build-dmg.sh
```

The command creates `dist/PTT2me-1.0.4-macos-arm64.dmg` and its
`.sha256` checksum. The image contains `PTT2me.app` and an `Applications`
link for drag-and-drop installation. It uses ad-hoc signing and is not
notarized for public distribution.

Before each new release, explicitly bump the package version in `Cargo.toml`
and synchronize `Cargo.lock` with Cargo. Rebuilding an existing release does
not change its version automatically.

The release gate runs on a controlled Apple Silicon Mac where the four frozen
model files have already been provisioned in `vendor/models/gigaam-v3-rnnt/`.
No model is fetched at build or runtime. Before publishing a DMG:

1. Run `scripts/build-dmg.sh`; its bundle check initializes the model embedded
   in the generated `PTT2me.app` and fails after 180 seconds instead of waiting
   forever.
2. Launch the built app in a normal macOS user session and verify the fixed
   model reaches `Готово`.
3. Put `CLIPBOARD-НЕ-ВСТАВЛЯТЬ` in the pasteboard and verify dictation in
   ChatGPT, a native text view, HTML `input`, `textarea`, contenteditable,
   Telegram, and Discord. The marker must never be inserted; the next manual
   Command-V must still produce it.
4. Verify rich text, an image, and a Finder file URL survive a fallback
   insertion.
5. Perform 20 short presses and 20 long holds with Fn/Globe and an assigned
   ordinary trigger. Short presses must perform the configured macOS action
   without ASR; long holds must run PTT without the short system action.
6. Revoke each permission in turn and verify the corresponding status,
   repeatable `Открыть настройки…` action, and return to `Готово`.
7. Move the cursor to another editable field during recognition and verify the
   result is inserted at its final location.

## Permissions and launch

PTT2me requires exactly Microphone, Input Monitoring, and Accessibility
permission. Build the app first, then use:

```bash
scripts/grant-and-run.sh
scripts/grant-and-run.sh --open-panes
scripts/grant-and-run.sh --reset
```

The default command preserves existing grants. `--open-panes` opens the three
relevant System Settings panes. `--reset` resets only PTT2me's three grants
and guides their interactive setup.

## Privacy

Audio and recognized text are processed locally. PTT2me stores only the
selected trigger, hold threshold, and `Пробел в конце` boolean preference in
macOS user defaults. It does not retain audio, transcripts, recognized text,
history, or other application data.
Recognized text is used temporarily for insertion. Direct Accessibility
and Unicode insertion do not modify the pasteboard. The compatibility fallback
restores every previous pasteboard item and representation unless newer
contents were copied during insertion.

## Troubleshooting

- `Ошибка модели`: the bundled model or native runtime could not be loaded.
  Rebuild with `scripts/build-app.sh`; the app must be restarted after fixing
  the bundle.
- `Ошибка микрофона`: the input device could not start or stop. Confirm
  Microphone permission and try the next Fn hold.
- `Ошибка Fn`: Input Monitoring is missing or the Fn event tap could not be
  restored. Confirm Input Monitoring permission; PTT2me retries the tap
  periodically.
