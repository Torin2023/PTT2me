# PTT2me

PTT2me is a minimal, fully local macOS menu-bar app: hold Fn/Globe, speak,
release the key, and the recognized Russian text is pasted into the frontmost
app. Recognition uses the bundled GigaAM v3 model; the resulting text remains
on the pasteboard.

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
PTT2me 1.0.0
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
transcripts, history, settings, or application data. Successful recognized
text remains only in the macOS pasteboard after insertion.

## Troubleshooting

- `Ошибка модели`: the bundled model or native runtime could not be loaded.
  Rebuild with `scripts/build-app.sh`; the app must be restarted after fixing
  the bundle.
- `Ошибка микрофона`: the input device could not start or stop. Confirm
  Microphone permission and try the next Fn hold.
- `Ошибка Fn`: Input Monitoring is missing or the Fn event tap could not be
  restored. Confirm Input Monitoring permission; PTT2me retries the tap
  periodically.
