# PTT2me Automatic Update Discovery Design

## Status

Revised after the 2026-07-31 implementation-plan audit. The original design
was approved in conversation; this revision incorporates the audit findings
and is pending written-spec review before production-code implementation.

This feature provides signed update discovery, a verified download, and a
user-guided manual installation. It does not replace the application bundle,
remove quarantine metadata, disable Gatekeeper, add Apple Developer ID
signing/notarization, send telemetry, or run a background agent.

## Product contract

- Git is the sole source of truth for published versions.
- The exact signed `updates/channels/stable.json` committed to Git has priority
  over a local bundle, local `dist/` files, and previously built artifacts.
- GitHub Pages is a read-only transport for committed update records. The
  client URL is exactly
  `https://torin2023.github.io/PTT2me/channels/stable.json`.
- Every release publishes a self-contained Full DMG and a model-free Update
  DMG. Both contain the same executable, frameworks, version, build, source
  commit, and embedded public key.
- A fresh installation always uses Full. Update is only selected by the
  running application after the required external model has been verified.
- No model is downloaded as an independent runtime asset. A Full DMG contains
  the complete model needed for an offline first launch.
- Update checks start no earlier than 60 seconds after launch and recur no more
  than once per 24 hours while the application remains running. A manual check
  bypasses the interval.
- The manifest request is automatic. A DMG is downloaded only after an
  explicit menu action by the user.
- After verification, the application remains running in `ReadyToInstall`.
  The user separately chooses `Открыть DMG и выйти…`; that action is enabled
  only while dictation and pasteboard restoration are idle.
- The user replaces `PTT2me.app` in `/Applications` through Finder, launches
  the new version, and grants Accessibility, Input Monitoring, and Microphone
  again.
- The updater never removes quarantine, automates Finder replacement, or
  bypasses the narrow macOS `Open Anyway` flow.
- On the first usable launch of every new release build, PTT2me resets exactly
  its Accessibility, ListenEvent, and Microphone decisions for bundle ID
  `com.ptt2me.app`. A failed model bootstrap does not consume this reset.

The resulting user intervention for an ordinary update is:

1. Choose `Скачать обновление <version>…`.
2. After the verified download completes, choose `Открыть DMG и выйти…`.
3. Replace the application in Finder and launch it.
4. If Gatekeeper blocks the unsigned build, use `Open Anyway` for this app.
5. Grant the same three permissions again in System Settings.

## Trust model

The initial Full DMG is an unsigned bootstrap trust decision by the user. The
embedded Ed25519 public key authenticates subsequent update records, but it
does not turn the app into an Apple-identified or notarized application.

The repository contains:

```text
updates/
  public-key.txt
  channels/stable.json
  releases/<version>.json
models/manifests/
  gigaam-v3-rnnt-v1.json
scripts/
  sign-update-manifest.sh
  validate-update-manifest.sh
.github/workflows/pages.yml
```

The private Ed25519 key never enters Git, GitHub Pages, GitHub Releases, or a
normal CI environment. Before the first production record is signed, the
release runbook must name its owner, require two encrypted offline backups,
and document recovery and bridge-release rotation. A bridge release signed by
the old key can ship a new embedded key before later records use it. Loss of
all copies of the current key requires a manual reinstall and cannot be hidden
by the updater.

To avoid ambiguous JSON canonicalization, the envelope remains:

```json
{
  "schema": 1,
  "payload": "BASE64_OF_EXACT_UTF8_JSON_BYTES",
  "signature": "BASE64_ED25519_SIGNATURE_OF_PAYLOAD_BYTES"
}
```

The decoded payload contains exactly:

```json
{
  "channel": "stable",
  "version": "1.0.6",
  "build": 202608011200,
  "source_commit": "40 lowercase hexadecimal characters",
  "minimum_macos": "13.0",
  "architecture": "arm64",
  "required_model": {
    "id": "gigaam-v3-rnnt-v1",
    "manifest_sha256": "64 lowercase hexadecimal characters"
  },
  "fresh_install": {
    "url": "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-full-macos-arm64.dmg",
    "sha256": "64 lowercase hexadecimal characters",
    "size": 191287170
  },
  "application_update": {
    "url": "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-update-macos-arm64.dmg",
    "sha256": "64 lowercase hexadecimal characters",
    "size": 12000000
  },
  "published_at": "2026-08-01T12:00:00Z"
}
```

The client limits the envelope body to 64 KiB, verifies the signature over the
decoded payload bytes before trusting fields, and rejects unknown fields. It
accepts only schema 1, channel `stable`, architecture `arm64`, stable semantic
versions, a nonzero build, exact expected HTTPS GitHub Release URL shapes,
nonzero artifact sizes no greater than 1 GiB, well-formed hashes/commits, and a
real UTC timestamp.

## Canonical release and installed-build precedence

The signed stable record defines the canonical GitHub release. The installed
bundle contributes `CFBundleShortVersionString`, `CFBundleVersion`, and
`PTT2meSourceCommit`.

Comparison is deterministic:

1. Compare semantic version.
2. For equal versions, compare numeric build.
3. For equal version/build, compare source commit.

- Remote version/build greater: `Available`.
- Equal version/build and equal source commit: `Current`.
- Equal version/build but different source commit: `DivergedLocal`; the
  canonical GitHub artifact is offered because Git has explicit priority.
- Local version/build greater: `UnpublishedLocal`; do not downgrade.
- A malformed or untrusted record never influences local state.

`minimum_macos` is compared numerically with the running system before a
download action is exposed. An incompatible release produces
`Incompatible { required_macos }`; automatic checks stay quiet and a manual
check shows the reason. No DMG action is offered.

Model repair is evaluated after release precedence:

- For `Available` or `DivergedLocal`, a verified exact model selects Update;
  a missing, wrong, or invalid model selects Full.
- For `Current`, a valid model needs no action. If neither a valid external
  model nor a bundled model exists, the same signed Full artifact becomes
  `RepairRequired` even though version/build are equal.
- For `UnpublishedLocal`, the stable Full artifact is not offered because it
  would be a downgrade. The UI reports that the local build needs its matching
  Full package.

## Model manifest and external store

The model manifest is committed at
`models/manifests/gigaam-v3-rnnt-v1.json` and embedded into both application
variants with `include_bytes!`. Its exact-byte SHA-256 must equal
`required_model.manifest_sha256`. A manifest file is immutable once its model
ID has shipped; changed bytes require a new model ID and a new manifest file.

The external model directory contains exactly four data files and no trusted
metadata copied from the user-writable directory:

```text
~/Library/Application Support/PTT2me/models/gigaam-v3-rnnt-v1/
  encoder.int8.onnx
  decoder.onnx
  joiner.onnx
  tokens.txt
```

The fixed manifest schema is:

```json
{
  "schema": 1,
  "id": "gigaam-v3-rnnt-v1",
  "files": [
    {"name": "encoder.int8.onnx", "size": 224570820, "sha256": "369f35a71bf288d3b8e0391fabd8dba5f2314088d440bca474056b7b4b6e66bf"},
    {"name": "decoder.onnx", "size": 4600132, "sha256": "38fc7475443ea2a26f63211ca350f73ac50fff824ab7a3876ee2bd610c53bbc4"},
    {"name": "joiner.onnx", "size": 2712896, "sha256": "602ff7017a93311aad34df1437c8d7f49911353c13d6eae7a6ee7b041339465c"},
    {"name": "tokens.txt", "size": 13354, "sha256": "39abae20e692998290c574e606f11a9edef2902a1995463fcff63d1490cf22b7"}
  ]
}
```

Runtime verification rejects duplicate or extra entries, extra filesystem
names, missing files, symlinks, non-regular files, executable bits, wrong
sizes, and digest mismatches. Verification and provisioning run on a worker,
not the AppKit thread.

### Bootstrap and repair transaction

The first updater-enabled release is installed from a Full DMG. Startup first
acquires the existing single-instance lock and then runs model preparation:

1. Verify the existing external directory against the embedded manifest.
2. If it is valid, use it without copying the bundled model.
3. If it is absent or invalid and the bundle has no matching model, enter
   `ModelRepairRequired` without resetting TCC.
4. If the bundle has the model, require free space equal to the manifest's
   total data size plus 64 MiB.
5. Create user-only `<model-id>.incoming`, copy only the four expected regular
   files, fsync each file, and verify the completed staging directory.
6. If the final target is an invalid non-empty directory, rename it to
   `<model-id>.invalid-<uuid>` and fsync the parent.
7. Rename `.incoming` to the final model ID, fsync the parent, and verify the
   final directory again.
8. Only after final verification may the invalid backup be removed. A valid
   model, including a valid older model ID, is never deleted automatically.

On restart, a valid `.incoming` with no final target is promoted; an invalid
`.incoming` is removed only after its path and contents have been validated as
belonging to this exact model transaction. A crash after the invalid-target
rename therefore remains recoverable.

The menu shows `Подготовка модели…` during work and a targeted storage/model
error with retry on failure. ASR loading begins only after verified model paths
have been resolved.

## Updater state machine and scheduling

The pure reducer states are:

```text
Idle
Checking(reason)
Current
Available(release, selected_artifact)
DivergedLocal(release, selected_artifact)
RepairRequired(release, full_artifact)
Incompatible(release, required_macos)
UnpublishedLocal
RecheckingModel(release)
Downloading(release, artifact)
ReadyToInstall(release, artifact, path)
Failed(reason, retry_action)
```

The reducer owns no HTTP, filesystem, AppKit, clock, platform, or process
implementation. Commands carry operation IDs; late results from an older
operation are ignored. At most one check, model recheck, or download is active.

On launch, the runtime first verifies a cached signed envelope, if present, so
a known update does not disappear after a restart. The cached bytes are never
trusted without the normal signature and field validation.

Automatic scheduling uses one-shot timers that are rescheduled after every
attempt:

- With no previous attempt, schedule for launch + 60 seconds.
- With a previous attempt less than 24 hours old, schedule its 24-hour due
  time, but never earlier than launch + 60 seconds.
- Persist `last_network_check_attempt` immediately before either an automatic
  or manual request. Manual checks bypass the due test but move the next
  automatic attempt 24 hours forward, avoiding a duplicate request.
- A future timestamp caused by wall-clock rollback is treated as an attempt at
  the current time and schedules the next check 24 hours later.

Automatic failures are logged and do not interrupt dictation. Manual failures
remain visible and retryable.

## Download, cache, and quarantine

The manifest worker uses HTTPS and bounded reads off the main thread:

- manifest response: at most 64 KiB;
- connect timeout: 10 seconds;
- read inactivity timeout: 30 seconds;
- overall request timeout: 15 minutes;
- redirects: at most five and never from HTTPS to HTTP;
- non-success HTTP status: failure;
- artifact stream: stop at signed size + 1 and reject both short and long
  bodies; compare `Content-Length` when present;
- global signed artifact size ceiling: 1 GiB.

Full and Update cache names include version, build, and artifact kind. A
download is written to a sibling `.part`, flushed and fsynced, verified by
size/SHA-256, atomically renamed, and re-opened for verification before use.
A verified cached file may be reused only after the same checks. Stale `.part`
files and verified DMGs from superseded releases are removed; the currently
offered verified DMG is retained until superseded or full uninstall.

Because a custom writer does not receive browser quarantine automatically, the
bundle enables file quarantine and the production boundary verifies that the
completed DMG has `com.apple.quarantine` before `ReadyToInstall`. Missing
quarantine is a hard failure, never a reason to open the file. A release gate
must exercise download → mount → Finder copy → expected Gatekeeper/Open Anyway
behavior on a clean macOS user.

The menu action `Открыть DMG и выйти…` is enabled only when the application is
`Ready`, no capture/recognition is active, and no pasteboard insertion or
restore is pending. `NSWorkspace` open failure leaves the app and verified DMG
available for retry. Only a successful open requests orderly termination.

Immediately before download confirmation, the worker verifies the required
model again. If an Update selection became invalid, the UI changes to Full and
requires the user to choose the Full download action; it never silently
downloads the larger artifact under an earlier choice.

## Permission migration

A release build identity is the exact tuple of:

```text
CFBundleShortVersionString
CFBundleVersion
PTT2meSourceCommit
```

`build-release-artifacts.sh` fixes these values once and both variants use the
same executable and identity. Dirty release builds are rejected. Development
binaries outside a `.app` bypass migration.

After model preparation succeeds and before event-tap, microphone, or insertion
runtime setup, the app compares this identity with `PermissionsResetForBuild`.
For a new identity it executes without a shell, with a 10-second timeout per
command:

```text
/usr/bin/tccutil reset Accessibility com.ptt2me.app
/usr/bin/tccutil reset ListenEvent com.ptt2me.app
/usr/bin/tccutil reset Microphone com.ptt2me.app
```

The reset marker is stored only after all three commands succeed. A separate
`PermissionsSetupCompletedForBuild` marker is stored after system probes report
all three grants. Closing during interactive setup does not repeat a successful
reset; the next launch resumes at the first missing permission.

Reset failure blocks dictation and exposes retry plus narrow manual Terminal
fallback instructions. The release gate must prove the real build-to-build
flow on supported macOS versions; mocked process-boundary tests do not satisfy
that gate.

## Deterministic packaging

The release coordinator receives explicit version, numeric build, and clean
source commit. It compiles the executable once, then creates two app bundles:

- Full: executable, frameworks, licenses, embedded public/model manifests, and
  the four model data files.
- Update: the byte-identical executable/frameworks/licenses/manifests and no
  `Resources/models` directory.

Both `Info.plist` files contain the same version, build, source commit, bundle
identifier, minimum system version, and file-quarantine policy. A diagnostic
`PTT2meDistributionVariant` differs, but it is not part of release precedence
or the version-level permission-reset marker.

Validation is explicitly variant-aware:

- `check-bundle.sh --variant full` verifies exact model contents and performs a
  bundled-model smoke test.
- `check-bundle.sh --variant update` rejects bundled model data and never reads
  production Application Support.
- The release gate compares executable and framework hashes between variants.
- `build-dmg.sh` packages a supplied app/variant/output and never rebuilds it.

## GitHub release and Pages flow

Feature implementation and production release are separate commits. The
release-only flow is:

1. Ensure GitHub Pages custom-workflow publishing and GitHub immutable releases
   are enabled. Immutability must be enabled before this release.
2. Bump `Cargo.toml` and `Cargo.lock` once, commit, and create tag `vX.Y.Z`.
3. Create a clean detached worktree at the exact tag and reject local changes.
4. Choose one build value, build both variants from the same compiled output,
   and complete bundle/manual/architecture/model/quarantine checks.
5. Create a draft GitHub Release and upload both DMGs and checksums.
6. Publish the release only after all assets are present; verify the release
   and each asset are immutable.
7. Generate and sign the payload using the offline key.
8. Through a separate PR, add immutable `updates/releases/X.Y.Z.json` and make
   `updates/channels/stable.json` its byte-for-byte copy.
9. CI verifies each release record against its historical source with
   `git show <source_commit>:Cargo.toml`, verifies tag → source commit, rejects
   modification of previously committed release records/model manifests, and
   verifies the real immutable asset digests.
10. Publish `updates/` as the GitHub Pages artifact root. After deployment,
    canary the exact public stable URL, require HTTP 200, and compare public
    bytes with Git before updating README/site or announcing the release.

If canary fails, the previous Pages deployment remains authoritative. A bad
stable pointer may be restored to a previous signed record, but already
installed newer clients are never downgraded; they require a signed hotfix.

The first updater-enabled version is a manually installed bootstrap Full DMG,
because v1.0.5 cannot discover it. Subsequent compatible releases exercise the
Update path.

## Documentation contract

README, release notes, and the product site must state:

- the feature automatically checks for updates; it does not automatically
  install them;
- exact 60-second/24-hour behavior and the manual bypass;
- manifest requests go to GitHub Pages without a product telemetry identifier;
- GitHub Release download happens only after the user's action;
- speech, audio, and recognition remain local;
- Full is the only fresh-install/manual website download; Update is for the
  in-app updater;
- Finder replacement, narrow Gatekeeper recovery, automatic TCC reset, and
  manual re-grant steps;
- model, update cache, timestamps, and permission markers stored by the app;
- full uninstall paths for the app, Application Support, cache, preferences,
  and remaining System Settings grants.

The site release version, Full URL, Full size, checksum, and release URL must be
validated against the committed signed release record. The primary CTA must
never point to `application_update`.

## Verification gates

Automated coverage includes:

- signed dual-artifact payloads, strict fields, source-commit divergence,
  numeric macOS comparison, repair precedence, and downgrade refusal;
- deterministic scheduler behavior across relaunch, long-running processes,
  manual checks, future clocks, and cached signed envelopes;
- model-manifest immutability, exact filesystem contents, symlink rejection,
  free-space failure, valid reuse, invalid-target swap, and crash recovery;
- re-verification between update discovery, user download action, and open;
- manifest/artifact size caps, short/long/chunked bodies, stalled reads,
  redirects, non-success responses, cache collisions, and `.part` cleanup;
- active recording/recognition/pasteboard states never terminate on download;
- deterministic Full/Update bundle identities and variant-specific checks;
- permission reset first launch, same-build relaunch, partial failure, marker
  persistence, and model-failure-before-reset ordering.

Manual release coverage includes:

```text
v1.0.5 → updater-enabled Full bootstrap → model provision → TCC reset/re-grant
current release + corrupt model → same-version Full repair
valid model + next release → model-free Update path
fresh user choosing Full from site → offline first recognition
minimum macOS mismatch → no download action
downloaded DMG → quarantine → Finder copy → Open Anyway → launch
offline/timeout/interrupted/ENOSPC cases → retry without data loss
```
