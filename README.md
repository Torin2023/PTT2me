# PTT2me

PTT2me is a minimal, fully local macOS menu-bar app: hold Fn/Globe, speak,
release the key, and the recognized Russian text is pasted into the frontmost
app. Recognition uses the bundled GigaAM v3 model; insertion preserves the
previous pasteboard contents.

## Requirements and workflow

- Apple Silicon (`arm64`) Mac
- macOS 13 Ventura or newer

PTT2me loads its fixed model at startup. When the status becomes `Готово`,
hold Fn/Globe for at least 250 ms while speaking and release it. The app keeps
recording for another 180 ms, recognizes the captured phrase, and pastes a
non-empty result with Cmd+V. A capture ends automatically after 25 seconds.

The menu contains exactly:

```text
<status>
PTT2me 1.0.2
────────────
Выйти
```

The status and version rows are informational; `Выйти` is the only command.

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

The command creates `dist/PTT2me-1.0.2-macos-arm64.dmg` and its
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
3. With Microphone, Input Monitoring, and Accessibility granted, hold and
   release Fn/Globe in a text editor and verify recognition, Cmd+V insertion,
   and restoration of a multi-item pasteboard.
4. Revoke each permission in turn and verify the corresponding status and
   recovery path.

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

Audio and recognized text are processed locally. PTT2me does not save audio,
transcripts, history, settings, or application data. Recognized text is used
temporarily for insertion and is not retained by PTT2me; the previous macOS
pasteboard contents are restored unless newer contents were copied during
insertion.

## Troubleshooting

- `Ошибка модели`: the bundled model or native runtime could not be loaded.
  Rebuild with `scripts/build-app.sh`; the app must be restarted after fixing
  the bundle.
- `Ошибка микрофона`: the input device could not start or stop. Confirm
  Microphone permission and try the next Fn hold.
- `Ошибка Fn`: Input Monitoring is missing or the Fn event tap could not be
  restored. Confirm Input Monitoring permission; PTT2me retries the tap
  periodically.
