# PTT2me optimization and stabilization execution

Spec: docs/superpowers/specs/2026-09-06-optimization-stability-design.md
Base: ca2a465540aa2377aeba0937d476e5a9bafdc0ae
Goal: implement separately committed improvements, investigate/fix ChatGPT insertion, combined verification and 1.3.0 preview release.

## Global Constraints

Only arm64/macOS 13+. Preserve immutable model/release records, signatures, licenses, TCC, short-press/combination semantics, 25-second capture limit, 180-ms release tail, secure-field checks and complete clipboard preservation. No text/audio in logs or committed fixtures. Main checkout and other worktrees untouched. No model downloads or key generation. User has authorized implementation, GUI testing, merge and release, and requested no interruptions. One implementation worker at a time; independent task reviews and final branch review. Each task has its own commit(s).

### Task 1: Ship arm64-only native libraries

Files: scripts/build-app.sh, scripts/check-bundle.sh, tests/model_bundle_variants.sh and directly related shell fixtures as necessary.
Extract arm64 from both native libraries when copying into bundle, before install_name_tool/codesign. Accept thin arm64 inputs unchanged. Fail if arm64 is absent. Bundle verification requires exactly arm64, including executable. Add regression with a real universal Mach-O fixture showing the former check accepts x86_64+arm64 and the new one rejects it. Keep Full/Update construction and licensing unchanged. Run affected shell contracts, shell syntax and git diff --check; commit independently.

### Task 2: Fix ChatGPT insertion regression

Files: src/text_inserter.rs, src/browser_accessibility.rs, tests/gui and focused tests if justified by observed cause.
Start from actual installed PTT2me/ChatGPT metadata and technical logs. Reproduce in a safe unsent ChatGPT draft without overwriting user drafts or submitting messages. Identify exact failing boundary. Implement the smallest verified fix and regression test. No fallback that bypasses unknown focus or password checks. Preserve Chrome behavior and clipboard ownership. Keep diagnosis/evidence in task report and commit independently.

### Task 3: Prepare completed audio off the AppKit thread

Files: src/audio.rs, src/runtime.rs, focused audio/runtime tests; small dedicated src/audio_task.rs module allowed.
Runtime must stop/drop the CPAL stream on main then transfer completed consumer/native samples and source rate to a single background preparation task. Ring draining and resampling run off main. Carry operation generation; ignore cancelled/late output; propagate overflow/callback/preparation panic/disconnect as recoverable capture errors. App exit never joins a blocked worker. Preserve stop helper needed by existing tests if appropriate. Test late output, callback/overflow, abort and exact existing signal contracts. Include technical duration/sample-count events only. Commit independently.

### Task 4: Supervise isolated ASR process

Files: src/asr.rs, src/asr_task.rs, src/main.rs, new focused src/asr_process.rs/protocol module, state/runtime integration and tests.
Keep serial Load/Transcribe semantics but run native recognizer in hidden --asr-worker child of the same executable. Use bounded binary frames over stdin/stdout (no JSON arrays of PCM), versioned handshake and session/request IDs; validate lengths, sample count, finite audio, paths and response limits. Child performs no GUI/TCC/instance locking. Parent supervisor keeps process ownership, kills/reaps timed-out/crashed child, discards stale responses, and supports bounded reload/recovery from verified model paths. Preserve original 180s load and 60s transcription deadlines, no automatic replay/paste of failed requests. Queue at most one operation. Test normal load/transcription with fake child, timeout, malformed/oversize/truncated data, crash, shutdown and recovery without process accumulation. Commit independently.

### Task 5: Bound AX and clipboard resource use

Files: src/text_inserter.rs, src/inserter.rs, focused tests/gui fixture.
Set explicit AX messaging timeout and total probe budget (short enough for menu responsiveness; allow legitimate target latency), recheck sensitive target immediately before paste. Introduce checked clipboard item/representation/total-byte budgets, refusing before mutation on overflow; preserve all formats or none. Retain immutable NSData to avoid extra copy only if lifetime contract is validated. Test huge data, overflow arithmetic, user ownership changes, nonresponsive target and protected field. Record byte counts/durations only, never contents. Commit independently.

### Task 6: Accept next dictation during clipboard restoration

Files: src/runtime.rs, src/state.rs, menu/hotkey integration and behavioral tests.
Separate dictation state from pending clipboard transaction. After successful Command-V, permit the next capture while previous restore timer remains 1000ms. Only one active capture/ASR, and at most one queued recognized insertion; never start next clipboard write before old restoration. Tag results/errors to originating operation so old restore completion cannot reset a newer recording/ASR state. Preserve copies made by user and avoid stale hotkey replay. Test rapid phrases at 0/100/500/1000ms gaps, old restore failure during new recording, empty/new failed recognition, exit and update-open deferral. Commit independently.

### Task 7: Wake run loop on events and bound disk cache

Files: new small runtime event/wake module, runtime/asr/audio/updater/menu/hotkey send boundaries, updater cache and tests.
Replace unconditional 50ms global drain polling with a signalled CFRunLoopSource or equivalent safe main-queue wake. Coalesce wakeups and drain bounded batches without lost wakes. Keep explicit watchdog/deadline timers and adapt permission polling to setup/recovery plus fresh checks before sensitive actions; preserve revoked permission behavior. Bound cache pruning to owned verified artifact names, no symlink traversal, keep current/previous needed release and active downloads; retention scoped and tested. Separate commits for event wake and cache retention. Measure idle wake behavior and regressions. Existing ad-hoc TCC reset policy stays because no Developer ID is available.

### Task 8: Add reproducible performance measurement and choose supported defaults

Files: examples/ or src/bin benchmark target, docs performance guide and tests for input validation.
Build benchmark with current lockfile/toolchain, accepting explicit existing model and consented mono PCM/WAV corpus. Measure model load, warm ASR, preparation, p50/p95, RTF, memory where feasible; compare CPU 1/2/4 threads with same samples. Bound input and never auto-download. Add stage timings to runtime without text/audio logging. Use locally generated non-user synthetic speech only with explicit label; no claim of corpus WER from silence. Keep CPU/2 defaults if evidence does not support change. Evaluate new runtime/CoreML only in an experiment; do not alter engine/model without quality evidence. Commit measurement tooling/results independently.

### Task 9: Integrate, review, test and release

Files: focused fixes from final review; Cargo.toml/Cargo.lock/versioned docs/site and immutable new release record.
Run cargo fmt --all -- --check; cargo test --all-targets --features test-support -- --test-threads=1; cargo clippy --all-targets --features test-support -- -D warnings; cargo audit --no-fetch --deny warnings; scripts/test-shell-contracts.sh; GUI fixture build/run where permissions allow. Exercise crash/recovery, repetitive captures, runtime responsiveness and ChatGPT draft insertion. Review full diff independently. Bump once to 1.3.0 and prepare accurate changes/limitations. Create PR preserving separate implementation commits and pass required GitHub CI before authorized merge. Follow README Gate B/C/D with existing external model/key and exact clean source commit; build Full/Update through sole builder, independently verify them. Publish only after applicable gates pass; never invent Manual P0. Publish signed record/channel/site and verify public assets/hashes. Keep local app/DMG/checksum links for final response.
