# PTT2me

PTT2me is a minimal, fully local macOS menu-bar app: hold Fn/Globe, speak,
release the key, and the recognized Russian text is inserted at the cursor's
current location. Recognition uses one fixed GigaAM v3 model. A Full build
contains that model for an offline first launch, then keeps the verified copy
outside the application bundle so later model-free Update builds can reuse it.
Insertion uses Accessibility to validate the focused field and reject secure
input, then sends ordinary text through the macOS pasteboard and Command-V.
The previous pasteboard is restored unless newer contents appeared meanwhile.

## Requirements and workflow

- Apple Silicon (`arm64`) Mac
- macOS 13 Ventura or newer

The repository source is prepared as the 1.3.0 release candidate. It is not a
published release: the public stable download, signed update channel, and menu
snapshot documented below remain Preview 1.2.1 until the 1.3.0 release gates
and publication complete.

## Cloud development without Codespaces

Repository work can be delegated to Codex Cloud from a browser or mobile
device without a running local computer. Connect `Torin2023/PTT2me` in
[Codex Cloud](https://chatgpt.com/codex), create an environment for the
repository, and set its setup command to:

```bash
bash scripts/cloud-setup.sh
```

The setup uses the exact Rust version in `rust-toolchain.toml`, installs the
repository's formatting, lint, audit, and cross-target tools, and fetches only
dependencies locked by `Cargo.lock`. It also refreshes the RustSec advisory
database and audits `Cargo.lock` while setup internet is available; later
checks use that snapshot with `cargo audit --no-fetch --deny warnings`. The
environment needs no repository or application secrets. Agent internet access
can remain disabled after setup.

PTT2me intentionally does not use GitHub Codespaces. Codex Cloud is a Linux
editing environment, while PTT2me is a macOS-only application. The development
workflow therefore separates responsibilities:

| Environment | Supported work |
| --- | --- |
| Codex Cloud | Inspect and edit files, format, cross-target check, audit, create a branch and Pull Request |
| GitHub Actions on macOS | Compile, run all unit and integration tests, run Clippy, audit dependencies |
| Controlled Apple Silicon Mac | Launch PTT2me, verify TCC and PTT behavior, build and validate release DMGs |

Every task follows one Issue or prompt → one `codex/<task>` branch → one Pull
Request. Direct changes to `main` are prohibited. A Pull Request is merged only
after the required `Format, test, lint, and audit` check succeeds. Repository
rules and agent-specific constraints are documented in `AGENTS.md`.

PTT2me verifies and prepares its fixed model at startup. When the status
becomes `Готово`, hold Fn/Globe while speaking and release it. Fn/Globe and a
500 ms hold threshold are the default settings. Recording begins immediately
when the trigger is pressed; a hold that reaches the selected threshold is
recognized on release. The app keeps recording for another 180 ms, recognizes
the captured phrase, and inserts a
non-empty result into whichever editable field owns the cursor at that moment.
A capture ends automatically after 25 seconds.

Loading the recognition engine has a 180-second deadline; each transcription
has a 60-second deadline. Recognition runs in one supervised child process. If
a deadline expires, PTT2me rejects late results, blocks new dictation, and
kills and reaps that child before a replacement can start. It attempts one
bounded automatic reload; if recovery fails, the menu offers
`Повторить запуск распознавания` after cleanup ownership is resolved. Quitting
uses the same cleanup path. If cleanup exceeds three seconds, the app stays
responsive and retains ownership until the child is actually reaped instead
of abandoning it.

Completed microphone captures are converted to mono 16 kHz for GigaAM. When
the device rate differs, a windowed-sinc filter prevents high-frequency noise
from folding into the speech band during conversion. Native 16 kHz audio is
passed through unchanged. See [audio quality checks](docs/audio-quality.md)
for the signal tests and the listening/recognition comparison still required
on a Mac; signal fidelity alone does not establish a reduction in word errors.

Insertion uses Accessibility to inspect the focused field and rejects secure
text fields. For ordinary text, PTT2me temporarily uses the full macOS
pasteboard and Command-V so browser and web-based editors receive a normal
paste event, then performs a guarded restore after one second. A newer
pasteboard change is never overwritten.

Immediately before Command-V, PTT2me also checks that the temporary pasteboard
still belongs to the current insertion. A copy made during the settle delay
cancels insertion and keeps the newer clipboard contents.

A short press is preserved: PTT2me replays it once to macOS, so the normal
Fn/Globe input-source action still works. The same applies to a short press of
any assigned ordinary key. If the assigned key is used in a combination, the
combination is passed through in its original order instead of starting
dictation.

The menu contains the trigger controls:

```text
<status>
PTT2me 1.2.1
Проверить обновления…
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

This menu snapshot describes the currently published Preview 1.2.1. The
updater action changes when a signed release is available or downloaded.

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

The current public download is the Preview 1.2.1 Full DMG:
[PTT2me-1.2.1-full-macos-arm64.dmg](https://github.com/Torin2023/PTT2me/releases/download/v1.2.1/PTT2me-1.2.1-full-macos-arm64.dmg)
(193,485,229 bytes / 184.5 MiB, SHA-256
`53076f0253a4f710cc0fc3fb3151802a43514110f2035d46b90c8d8f3c914fba`).
See the [v1.2.1 release page](https://github.com/Torin2023/PTT2me/releases/tag/v1.2.1)
for the model-free Update DMG and checksum files.

This preview fixes Chrome page-field insertion by preparing the browser's
Accessibility tree during capture. Automated macOS/AppKit, Rust, security,
signed-manifest, model, bundle, and DMG checks passed.
All manual checks for 1.2.1, including Manual P0, were skipped by explicit
owner instruction. They are not reported as passed. This unsigned preview is
published by owner decision with that limitation disclosed in the release notes.

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
bash scripts/test-shell-contracts.sh
cargo fmt --all -- --check
cargo test --all-targets --features test-support -- --test-threads=1
cargo clippy --all-targets --features test-support -- -D warnings
cargo audit --no-fetch --deny warnings
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
Git. The currently published stable package version is exactly 1.2.1.

### Reproducible release gates

On a controlled Apple Silicon Mac, run the fail-closed preflight before the
production builder. The model directory and private key are external release
inputs; the scripts never download, generate, or replace them:

```bash
scripts/release-preflight.sh \
  --version X.Y.Z \
  --build YYYYMMDDHHMM \
  --source-commit COMMIT \
  --model-manifest models/manifests/gigaam-v3-rnnt-v1.json \
  --model-source /absolute/path/to/gigaam-v3-rnnt \
  --public-key updates/public-key.txt \
  --private-key /absolute/path/outside/git/private-key.txt \
  --published-at YYYY-MM-DDTHH:MM:SSZ \
  --output-dir /absolute/path/to/empty-release-set

scripts/build-release-artifacts.sh \
  --version X.Y.Z \
  --build YYYYMMDDHHMM \
  --source-commit COMMIT \
  --model-manifest models/manifests/gigaam-v3-rnnt-v1.json \
  --model-source /absolute/path/to/gigaam-v3-rnnt \
  --public-key updates/public-key.txt \
  --private-key /absolute/path/outside/git/private-key.txt \
  --published-at YYYY-MM-DDTHH:MM:SSZ \
  --output-dir /absolute/path/to/empty-release-set
```

The builder publishes exactly five no-overwrite outputs into the selected
directory. Verify that closed set independently, without the private key or
builder workspace:

```bash
scripts/verify-release-artifacts.sh \
  --version X.Y.Z \
  --source-commit COMMIT \
  --full-dmg /absolute/path/to/release-set/PTT2me-X.Y.Z-full-macos-arm64.dmg \
  --full-checksum /absolute/path/to/release-set/PTT2me-X.Y.Z-full-macos-arm64.dmg.sha256 \
  --update-dmg /absolute/path/to/release-set/PTT2me-X.Y.Z-update-macos-arm64.dmg \
  --update-checksum /absolute/path/to/release-set/PTT2me-X.Y.Z-update-macos-arm64.dmg.sha256 \
  --manifest /absolute/path/to/release-set/PTT2me-X.Y.Z-signed-update-manifest.json \
  --public-key updates/public-key.txt \
  --model-manifest models/manifests/gigaam-v3-rnnt-v1.json
```

This is the rehearsal form before the tag exists. Immediately before
publication, repeat the same verifier command with `--expected-tag vX.Y.Z`; the
tag must resolve to the signed source commit. Then complete a separate copy of
[`docs/release/MANUAL_P0_CHECKLIST.md`](docs/release/MANUAL_P0_CHECKLIST.md)
from the installed Full DMG. Filled checklists remain beside local release
outputs and are not committed.

The preflight, builder, and verifier do not publish GitHub Release or the Pages stable channel.
Publication is a separate owner-authorized workflow after Gate A, Gate B,
Gate C, and the manual P0 gate all pass.

The controlled [AppKit/WebKit insertion fixture](docs/testing/INSERTION_GUI.md)
checks final field contents, focus changes, password rejection, and clipboard
preservation through the production insertion modules. CI builds the fixture;
execution requires a macOS GUI session with Accessibility and event-posting
access. The manual P0 checklist also includes a short ChatGPT draft check.

## Updating (PTT2me 1.2.1)

Preview 1.0.5 cannot discover this release. Versions 1.1.0 and 1.1.1 can check
the signed stable channel, but their updater cannot download the build that
contains its fix. Replace any of these versions once with the published 1.2.1
Full DMG. Starting with 1.1.2, PTT2me can download subsequent signed updates
through the flow described below.

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

Recognized text is used temporarily for insertion. Accessibility validates the
focused field and rejects secure input. Ordinary text is placed temporarily on
the full pasteboard and inserted with Command-V so browsers, web editors,
Codex, and native fields receive a normal paste event. Every previous item and
representation is restored unless newer contents were copied during insertion.

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
