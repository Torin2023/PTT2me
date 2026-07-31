# PTT2me Automatic Updates Design

## Status

Approved in conversation on 2026-07-31. This design covers update discovery,
download verification, user-guided installation, and permission migration for
unsigned builds. It does not add automatic application replacement, an Apple
Developer ID signature, notarization, telemetry, or a background agent.

## Product contract

- Git is the only source of truth for published versions.
- The signed `updates/channels/stable.json` committed to Git has priority over
  local bundle metadata, local `dist/` files, and previously built artifacts.
- GitHub Pages is a read-only delivery channel for committed update manifests.
- GitHub Releases stores the DMG referenced by a manifest.
- PTT2me checks automatically at most once per 24 hours, beginning 60 seconds
  after launch. `Проверить обновления…` bypasses that interval.
- A DMG is downloaded only after the user chooses the download menu action.
- The app verifies the manifest signature, architecture, version, size, and
  SHA-256 before opening a DMG.
- Installation remains manual: PTT2me opens the verified DMG and quits; the
  user replaces the app in `Applications`.
- No code removes quarantine metadata, disables Gatekeeper, or automates an
  `Open Anyway` confirmation.
- On the first launch of every distinct unsigned build, PTT2me resets only its
  Accessibility, Input Monitoring, and Microphone grants. The user grants all
  three again in System Settings before dictation becomes ready.

## Trust model and repository layout

The repository contains:

```text
updates/
  public-key.txt
  channels/stable.json
  releases/1.0.6.json
scripts/
  sign-update-manifest.sh
.github/workflows/pages.yml
```

The private Ed25519 key never enters Git, GitHub Pages, a GitHub Release, or a
normal CI environment. `public-key.txt` is embedded into the application at
compile time and is also published for auditability.

To avoid ambiguous JSON canonicalization, the signed envelope is:

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
  "download_url": "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-macos-arm64.dmg",
  "sha256": "64 lowercase hexadecimal characters",
  "size": 191287170,
  "published_at": "2026-08-01T12:00:00Z"
}
```

The client verifies the envelope before parsing or trusting payload fields.
It accepts only schema 1, channel `stable`, architecture `arm64`, HTTPS, the
exact GitHub release host/path shape, valid semantic versions, nonzero size,
and well-formed hashes and commits.

`updates/releases/<version>.json` is immutable. `stable.json` is a byte-for-byte
copy of the selected signed release envelope. CI rejects a stable entry without
a matching release record and rejects disagreement with `Cargo.toml`, the tag,
the source commit, or the published asset digest.

## Version precedence

The remote signed stable manifest defines the published stable version. The
installed bundle defines only which build is currently running.

- Remote version/build greater than local: update available.
- Remote version/build equal to local: current.
- Local greater than remote: `неопубликованная сборка`; do not downgrade.
- An automatic downgrade is never performed. A future recovery design requires
  an explicit signed recovery field and is outside this scope.

## Application components

### `update_manifest.rs`

Owns envelope decoding, Ed25519 verification, payload validation, semantic
version/build comparison, and download digest verification. It has no network,
filesystem, UI, or AppKit dependencies.

### `updater.rs`

Owns the updater state machine and boundary traits for HTTP, storage, clock,
current platform, and opening a file. Network and hashing run off the AppKit
main thread. A verified DMG is stored below
`~/Library/Caches/com.ptt2me.app/updates/`; partial files are never opened.

States are `Idle`, `Checking`, `Current`, `Available`, `Downloading`,
`ReadyToInstall`, `UnpublishedLocal`, and `Failed`. Automatic-check failures
are logged without interrupting dictation. A manual failure is visible in the
updater menu row and remains retryable.

### Menu and runtime integration

The menu always contains `Проверить обновления…`. When an update is available,
it also contains `Скачать PTT2me <version>…`. During work the informational row
shows checking/downloading progress. Selecting download is the user's explicit
confirmation. After verification PTT2me opens the DMG through `NSWorkspace`
and terminates, leaving bundle replacement to Finder.

The runtime schedules a one-shot 60-second startup timer and evaluates the
24-hour persisted last-check timestamp before making a request. A manual check
ignores the timestamp. No LaunchAgent or work is performed while PTT2me is not
running.

### `permission_migration.rs`

A build identity is the tuple of `CFBundleShortVersionString`,
`CFBundleVersion`, and `PTT2meSourceCommit`. `build-app.sh` writes all three to
`Info.plist`. Development binaries outside a `.app` do not run migration.

Before the event tap, microphone stream, or insertion runtime can become ready,
the app compares the identity with `PermissionsResetForBuild` in
`NSUserDefaults`. On a new identity it runs, without a shell:

```text
/usr/bin/tccutil reset Accessibility com.ptt2me.app
/usr/bin/tccutil reset ListenEvent com.ptt2me.app
/usr/bin/tccutil reset Microphone com.ptt2me.app
```

The reset marker is stored only after all commands succeed. A separate
`PermissionsSetupCompletedForBuild` marker is stored after system probes report
all three grants. Closing the app during setup does not repeat a successful
reset; the existing permission flow resumes at the first missing grant.

If reset fails, dictation remains blocked, the menu reports a permission-reset
error, and the user can retry. The app never treats old grants as valid for the
new build.

## Release flow

1. Commit release source and create tag `vX.Y.Z`.
2. Build the DMG from that exact tag on the controlled Apple Silicon Mac.
3. Run bundle, manual, signature, architecture, checksum, and model checks.
4. Publish the immutable DMG and checksum to GitHub Release.
5. Generate the payload with the tag commit and GitHub asset digest.
6. Sign payload bytes using the offline Ed25519 key.
7. Commit the immutable release envelope and update `stable.json` through PR.
8. CI validates the relationship and deploys the committed `updates/` directory
   to GitHub Pages.

The next release is the bootstrap release: v1.0.5 cannot check for updates, so
users install the first updater-enabled version manually. All later compatible
versions use the embedded public key.

## Verification

- Pure unit tests cover malformed envelopes, invalid signatures, field
  validation, version precedence, downgrade refusal, and digest mismatch.
- Updater state tests use deterministic in-memory boundaries for automatic and
  manual checks, download confirmation, network failure, and verified opening.
- Permission migration tests cover first launch, same-build relaunch, partial
  reset failure, setup continuation, and development-binary bypass.
- Menu/runtime tests cover action visibility, timer policy, failure display,
  and blocking until permissions are re-granted.
- Script tests generate a temporary key and artifact, sign a manifest, and
  verify it through the same Rust verifier.
- Release validation covers GitHub Pages payload bytes, GitHub asset SHA-256,
  source commit, bundle fields, and the full existing PTT2me check suite.
