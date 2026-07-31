# PTT2me Automatic Updates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add signed GitHub-Pages update discovery, verified user-confirmed DMG downloads, and first-launch TCC migration for each unsigned PTT2me build.

**Architecture:** A pure manifest verifier and updater state machine sit behind network, storage, clock, and platform boundaries. AppKit runtime integration schedules checks and projects state into the existing menu. A separate build-identity migration resets only PTT2me's three required TCC grants before a new unsigned build becomes usable.

**Tech Stack:** Rust 2021, AppKit/Foundation through objc2, Ed25519, SHA-256, ureq HTTPS, GitHub Pages, GitHub Releases, shell release tooling.

## Global Constraints

- Git is the only source of truth; signed Git manifests have priority over local bundle and `dist/` metadata.
- GitHub Pages only serves committed static manifests.
- Apple Developer ID signing and notarization remain out of scope.
- Installation is user-confirmed and manual; never replace `.app`, remove quarantine, or disable Gatekeeper.
- Automatic network checks occur at most once per 24 hours, beginning 60 seconds after launch.
- PTT2me remains Apple Silicon-only and requires macOS 13 or newer.
- Reset exactly `Accessibility`, `ListenEvent`, and `Microphone` for bundle ID `com.ptt2me.app` on each new build identity.
- Every behavior change follows a witnessed RED-GREEN cycle.

---

### Task 1: Signed update-manifest contract

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `src/update_manifest.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `VerifiedRelease`, `InstalledBuild`, `ReleaseDisposition`, `verify_envelope(bytes, public_key)`, and `verify_artifact(reader, release)`.

- [ ] Write unit tests with literal signed fixtures for a valid envelope, altered payload, altered signature, invalid URL/architecture/hash/commit, newer/equal/local-newer builds, and wrong artifact bytes.
- [ ] Run `cargo test update_manifest::tests --features test-support` and confirm failure because the module/API does not exist.
- [ ] Add direct dependencies for `base64`, `ed25519-dalek`, `semver`, `serde`, `serde_json`, and `sha2`; implement exact-byte signature verification and strict validation.
- [ ] Re-run the focused tests and confirm all pass.
- [ ] Run `cargo fmt --all -- --check` and commit the task.

### Task 2: Deterministic updater state machine

**Files:**
- Create: `src/updater.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `VerifiedRelease`, `InstalledBuild`, `ReleaseDisposition`.
- Produces: `Updater`, `UpdaterState`, `UpdaterCommand`, `UpdaterEvent`, and boundary traits for fetch, storage, clock, and file opening.

- [ ] Write tests proving a due automatic check, a suppressed check within 24 hours, an unconditional manual check, silent automatic failure, visible manual failure, explicit download confirmation, digest rejection, and open-then-quit only after verification.
- [ ] Run `cargo test updater::tests --features test-support` and confirm the missing API failure.
- [ ] Implement the minimal pure transition reducer and side-effect commands.
- [ ] Re-run the focused tests and confirm all pass.
- [ ] Refactor only duplicated transition setup, keep tests green, and commit.

### Task 3: System updater worker and cache

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/updater.rs`
- Modify: `src/constants.rs`

**Interfaces:**
- Produces: production HTTPS fetch with bounded timeout, cache writer using `.part` plus atomic rename, SHA-256 streaming verification, and `NSWorkspace` DMG opening.

- [ ] Write filesystem integration tests using a local temporary directory for partial-file cleanup, digest mismatch, successful atomic promotion, and reuse of an already verified artifact.
- [ ] Run the focused tests and witness expected failures.
- [ ] Add direct `ureq` dependency and implement production boundaries without executing network work on the main thread.
- [ ] Re-run focused tests and the entire library test target; commit.

### Task 4: Updater menu and runtime scheduling

**Files:**
- Modify: `src/menu.rs`
- Modify: `src/runtime.rs`
- Modify: `src/state.rs`
- Modify: `src/preferences.rs`

**Interfaces:**
- Adds menu actions `CheckForUpdates` and `DownloadUpdate` and projects updater states into informational/action rows.
- Persists only the last automatic-check timestamp; it stores no user, device, or telemetry identifier.

- [ ] Add menu projection tests for idle, checking, available, downloading, current, unpublished-local, and failure states.
- [ ] Run focused menu tests and witness missing-row/action failures.
- [ ] Implement menu projection and action delivery.
- [ ] Add runtime timer/reducer tests for 60-second startup scheduling, 24-hour policy, manual bypass, and worker-result delivery.
- [ ] Run focused runtime tests and witness failures.
- [ ] Implement the worker channel, timers, persistence, open-DMG then orderly termination, and keep dictation responsive.
- [ ] Re-run menu/runtime tests and commit.

### Task 5: New-build permission migration

**Files:**
- Create: `src/permission_migration.rs`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Modify: `src/runtime.rs`
- Modify: `src/state.rs`
- Modify: `src/menu.rs`
- Modify: `scripts/build-app.sh`
- Modify: `scripts/check-bundle.sh`

**Interfaces:**
- Produces: `BuildIdentity`, `PermissionMigration`, `ResetBoundary`, and startup events for success/failure/retry.
- Adds `PTT2meSourceCommit` to `Info.plist`.

- [ ] Write tests for first launch, same-build relaunch, a changed build, second-command failure, no marker on failure, setup continuation, setup completion, and non-bundle development bypass.
- [ ] Run focused tests and witness missing API failures.
- [ ] Implement build identity loading and direct `/usr/bin/tccutil` execution for the exact three services.
- [ ] Add state/menu tests proving dictation remains blocked on reset failure and retry is available.
- [ ] Run and witness failures, then implement runtime/state/menu integration.
- [ ] Add bundle-script assertions for a 40-character lowercase commit and create a controlled failing script check before changing `build-app.sh`.
- [ ] Write `PTT2meSourceCommit`, rerun script checks, focused tests, and commit.

### Task 6: Git source of truth, signing tool, and Pages

**Files:**
- Create: `updates/public-key.txt`
- Create: `updates/channels/stable.json`
- Create: `updates/releases/1.0.6.json` during release only
- Create: `scripts/sign-update-manifest.sh`
- Create: `scripts/validate-update-manifest.sh`
- Create: `.github/workflows/pages.yml`
- Create: `tests/update_manifest_cli.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Adds hidden `--verify-update-manifest <public-key> <manifest> <artifact>` mode used by scripts and tests.
- Signing script consumes a private key file path and never reads a key from Git.

- [ ] Add an integration test that creates a temporary Ed25519 key/artifact, signs an envelope through the script, and verifies it through the binary CLI.
- [ ] Run the integration test and witness failure because scripts/CLI are absent.
- [ ] Implement signing and validation scripts plus CLI; rerun the test.
- [ ] Generate the production key outside Git with restrictive permissions, commit only its public key, and embed it with `include_str!`.
- [ ] Add a Pages workflow that validates then publishes only `updates/`; do not publish a stable manifest until a corresponding real release asset exists.
- [ ] Run script integration and workflow syntax checks; commit.

### Task 7: User instructions and release documentation

**Files:**
- Modify: `README.md`
- Modify in product-site worktree: `site/app/page.tsx`
- Modify in product-site worktree: `site/app/globals.css`
- Modify in product-site worktree: `site/tests/rendered-html.test.mjs`

**Interfaces:**
- Documents automatic daily checks, user-confirmed download, manual replacement, narrow Gatekeeper recovery, automatic permission reset, and manual re-granting.

- [ ] Replace the temporary manual-`tccutil` instructions with factual behavior from the implemented app.
- [ ] Update the rendered-site contract first and witness failure.
- [ ] Update site content and README; rerun `npm test` and `npm run lint`.
- [ ] Preserve the existing social card because product name, headline, palette, and visual motif are unchanged.
- [ ] Commit application README and site changes in their appropriate branches.

### Task 8: Full verification and release-readiness gate

**Files:**
- Modify only files required by failures found in this task, always with a new failing regression test first.

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo test --all-targets --features test-support -- --test-threads=1` outside the non-GUI sandbox.
- [ ] Run `cargo clippy --all-targets --features test-support -- -D warnings`.
- [ ] Run `cargo audit --deny warnings`.
- [ ] Run update-manifest script integration tests and validate Pages input.
- [ ] Build the `.app` with provisioned frozen models, run `scripts/check-bundle.sh`, and confirm the embedded source commit/public key.
- [ ] Review `git diff --check`, the final diff, and every design requirement; report any release-only step that remains blocked on a real v1.0.6 artifact.
