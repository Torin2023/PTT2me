# PTT2me

PTT2me is a minimal, fully local macOS menu-bar app: hold Fn/Globe, speak,
release the key, and the recognized Russian text is pasted into the frontmost
app. Recognition uses the bundled GigaAM v3 model; insertion preserves the
previous pasteboard contents.

## Requirements and workflow

- Apple Silicon (`arm64`) Mac
- macOS 13 Ventura or newer

PTT2me loads its fixed model at startup. When the status becomes `Готово`,
hold Fn/Globe while speaking and release it. Fn/Globe and a 500 ms hold
threshold are the default settings. Recording begins immediately when the
trigger is pressed; a hold that reaches the selected threshold is recognized
on release. The app keeps recording for another 180 ms, recognizes the
captured phrase, and pastes a non-empty result with Cmd+V. A capture ends
automatically after 25 seconds.

A short press is preserved: PTT2me replays it once to macOS, so the normal
Fn/Globe input-source action still works. The same applies to a short press of
any assigned ordinary key. If the assigned key is used in a combination, the
combination is passed through in its original order instead of starting
dictation.

The menu contains the trigger controls:

```text
<status>
PTT2me 1.0.0
────────────
Клавиша активации
  <selected key>
  Назначить…
  Сбросить на Fn / Globe
Порог удержания
  250 мс
  500 мс
  750 мс
Выйти
```

Choose `Клавиша активации` → `Назначить…`, then press the key to use for
dictation. Press Escape to cancel assignment. `Сбросить на Fn / Globe` restores
the default trigger. Escape, Caps Lock, and system volume/mute keys are not
accepted as assigned triggers and leave the current trigger unchanged.

Choose one of the exact `Порог удержания` values: 250, 500, or 750 ms. A hold
shorter than the selected value is replayed normally; a longer hold records
and is consumed so it does not type or invoke its usual system action.

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
selected trigger and hold threshold in macOS user defaults. It does not retain
audio, transcripts, recognized text, history, or other application data.
Recognized text is used temporarily for insertion; the previous macOS
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
