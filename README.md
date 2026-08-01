# PTT2me

PTT2me is a minimal, fully local macOS menu-bar app: hold Fn/Globe, speak,
release the key, and the recognized Russian text is inserted at the cursor's
current location. Recognition uses one fixed GigaAM v3 model. A Full build
contains that model for an offline first launch, then keeps the verified copy
outside the application bundle so later model-free Update builds can reuse it.
Insertion prefers the focused Accessibility text field and preserves the
previous pasteboard whenever compatibility requires Command-V fallback.

## Requirements and workflow

- Apple Silicon (`arm64`) Mac
- macOS 13 Ventura or newer

PTT2me verifies and prepares its fixed model at startup. When the status
becomes `Готово`, hold Fn/Globe while speaking and release it. Fn/Globe and a
500 ms hold threshold are the default settings. Recording begins immediately
when the trigger is pressed; a hold that reaches the selected threshold is
recognized on release. The app keeps recording for another 180 ms, recognizes
the captured phrase, and inserts a
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
PTT2me 1.0.5
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

This menu snapshot describes the currently published Preview 1.0.5. The
updater rows described below ship only with PTT2me 1.1.0 after that release is
published.

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

## Published release

The public download remains Preview 1.0.5:
[PTT2me-1.0.5-macos-arm64.dmg](https://github.com/Torin2023/PTT2me/releases/download/v1.0.5/PTT2me-1.0.5-macos-arm64.dmg)
(182 MB, SHA-256
`d89a1767edfb2c010ba98ffc59f6c35f8e346958c492b3ed33b4596f303a7c8c`).
Preview 1.0.5 does not check for updates. No 1.1.0 download URL, size,
checksum, or release page is published yet.

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
scripts/build-app.sh \
  --variant full \
  --model-manifest models/manifests/gigaam-v3-rnnt-v1.json \
  --model-source vendor/models/gigaam-v3-rnnt
```

The script builds only `aarch64-apple-darwin`, obtains the two native runtime
libraries from the Cargo release output, verifies the exact committed model
manifest, and creates the self-contained Full app at `dist/PTT2me.app`.
The release coordinator creates the Update variant from the same compiled app
and removes only `Contents/Resources/models`.

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
an ASR model. Release builds consume the fixed GigaAM v3 RNNT files supplied
locally at the release gate. Full contains them for bootstrap; Update does not.

## Release artifacts

The controlled release command `scripts/build-release-artifacts.sh` creates
both DMGs and their checksums from one clean Git commit:

```text
PTT2me-X.Y.Z-full-macos-arm64.dmg
PTT2me-X.Y.Z-update-macos-arm64.dmg
PTT2me-X.Y.Z-signed-update-manifest.json
```

Full and Update contain the same executable, frameworks, version, build,
source commit, and update-verification public key. Full also contains the fixed
model; Update contains no model. The product site and every fresh installation
use Full only. Update is selected only inside an already installed PTT2me.

The release coordinator requires an explicit stable version, 12-digit UTC
build, exact clean `HEAD`, model source, committed model manifest, publication
timestamp, and matching key pair. The private signing key must remain outside
Git. The package version stays 1.0.5 during development and is changed once to
exactly 1.1.0 at the final release gate.

## Updating (PTT2me 1.1.0 after publication)

This behavior starts only after PTT2me 1.1.0 is published. Preview 1.0.5
cannot discover that release and must be replaced once with the published
1.1.0 Full DMG.

- The first automatic manifest request starts no earlier than 60 seconds after
  launch. Later automatic requests run no more than once per 24 hours.
  `Проверить обновления…` bypasses the interval.
- The signed release record is requested from GitHub Pages without a user,
  device, or telemetry identifier. The endpoint is
  `https://torin2023.github.io/PTT2me/channels/stable.json`. A previously
  verified record may be shown from cache after restart.
- A check never downloads or installs a DMG. Download starts only when the user
  chooses `Скачать обновление <version>…`; bytes come from GitHub Release and
  are verified before use.
- A fresh installation always uses Full. The running app selects the
  model-free Update only after verifying the exact external model at
  `~/Library/Application Support/PTT2me/models/gigaam-v3-rnnt-v1/`.
  A missing, changed, or invalid model selects Full instead.
- Replacing `PTT2me.app` does not remove the external model. Full provisions it
  when needed; Update reuses it only after verification.

To install an offered update:

1. Choose `Скачать обновление <version>…`.
2. When verification finishes, choose `Открыть DMG и выйти…`.
3. Replace `PTT2me.app` in `/Applications` through Finder.
4. If macOS blocks the ad-hoc-signed build, allow only PTT2me with
   `Открыть всё равно` in Privacy & Security. Do not disable Gatekeeper.
5. Launch the new build and grant Accessibility, Input Monitoring, and
   Microphone again.

## Permissions and launch

PTT2me requires exactly Microphone, Input Monitoring, and Accessibility
permission. Starting with the published 1.1.0 build, launch ordering is fixed:

1. PTT2me verifies or provisions the external model.
2. On the first usable launch of a new version/build/source-commit identity,
   it automatically resets only its Accessibility, Input Monitoring, and
   Microphone decisions.
3. The menu shows `Сброс разрешений…` while this runs. A failure blocks
   dictation and exposes the single targeted action
   `Повторить сброс разрешений`.
4. After a successful reset, grant the same three permissions again in System
   Settings. `Открыть настройки…` opens the pane for the first missing
   permission and can be used repeatedly.

There are no Terminal commands in the normal install or update flow. Closing
PTT2me during manual permission setup does not repeat a completed reset; the
next launch resumes with the first missing permission.

## Stored data and full uninstall

Starting with the published 1.1.0 build, PTT2me keeps:

- the verified model below
  `~/Library/Application Support/PTT2me/models/gigaam-v3-rnnt-v1/`;
- the cached signed release record and verified downloaded DMGs below
  `~/Library/Caches/com.ptt2me.app/`;
- trigger, threshold, `Пробел в конце`, last network-check timestamp, and
  `PermissionsResetForBuild` / `PermissionsSetupCompletedForBuild`
  markers in macOS user defaults, normally represented by
  `~/Library/Preferences/com.ptt2me.app.plist`.

To remove PTT2me completely, quit it, remove
`/Applications/PTT2me.app`, `~/Library/Application Support/PTT2me/`,
`~/Library/Caches/com.ptt2me.app/`, and
`~/Library/Preferences/com.ptt2me.app.plist`. Then remove PTT2me from
Accessibility, Input Monitoring, and Microphone in System Settings. Replacing
or deleting the app bundle alone intentionally leaves the external model in
place.

## Privacy

Audio and recognized text are processed locally. PTT2me does not retain audio,
transcripts, recognized text, or dictation history. Update discovery requests
only the signed release record from GitHub Pages and carries no product user,
device, or telemetry identifier. A DMG request to GitHub Release occurs only
after the user's download action.

Recognized text is used temporarily for insertion. Direct Accessibility and
Unicode insertion do not modify the pasteboard. The compatibility fallback
restores every previous pasteboard item and representation unless newer
contents were copied during insertion.

## Troubleshooting

- `Требуется восстановление модели`: the external model is missing or
  invalid and this app contains no verified bundled copy. Install the offered
  Full DMG.
- `Ошибка подготовки модели`: provisioning or verification failed. Fix the
  storage problem and choose `Повторить подготовку модели`.
- `Не удалось сбросить разрешения`: choose
  `Повторить сброс разрешений`. Model preparation remains complete and is
  not repeated.
- `Ошибка микрофона`: the input device could not start or stop. Confirm
  Microphone permission and try the next Fn hold.
- `Ошибка Fn`: Input Monitoring is missing or the Fn event tap could not be
  restored. Confirm Input Monitoring permission; PTT2me retries the tap
  periodically.
