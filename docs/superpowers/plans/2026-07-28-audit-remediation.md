# PTT2me Audit Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove realtime allocation and main-run-loop blocking, bound bundled-model smoke initialization, and add an executable CI/release verification path.

**Architecture:** Keep all AppKit pasteboard operations on the main thread and sequence them with existing `CFRunLoopTimer` infrastructure. Replace callback-owned temporary vectors and the shared mutex with a bounded preallocated SPSC buffer. Isolate model initialization in a child process so the parent can enforce a real deadline.

**Tech Stack:** Rust 2021, CPAL 0.15, Core Foundation run-loop timers, AppKit pasteboard, GitHub Actions arm64 macOS.

## Global Constraints

- The only model is the bundled GigaAM v3 RNNT model; no runtime download, selection, or fallback.
- Preserve every pasteboard item, representation, type name, and byte sequence.
- Never overwrite a pasteboard change that occurred after PTT2me's temporary write.
- Support Apple Silicon and macOS 13 or later.
- Keep the existing 250 ms minimum hold and 25-second maximum capture.
- Do not add settings, history, telemetry, or other product features.

---

### Task 1: Non-blocking paste transaction

**Files:**
- Modify: `src/inserter.rs`
- Modify: `src/runtime.rs`
- Modify: `src/hotkey.rs`
- Modify: `src/state.rs`
- Test: unit tests in the same modules and `tests/ptt_flow.rs`

**Interfaces:**
- Produces: `PendingInsertion::begin`, `PendingInsertion::paste`, and `PendingInsertion::restore`.
- Produces: timestamped `HotkeySignal` values.
- Consumes: existing `PasteboardAccess`, `PasteCommand`, `CFRunLoopTimer`, and `AppEvent::PasteFinished`.

- [ ] Add tests proving begin does not sleep, paste and restore are separate stages, paste failure restores immediately, and a newer pasteboard change is preserved.
- [ ] Run the focused tests and verify they fail because staged insertion does not exist.
- [ ] Implement `PendingInsertion` and remove `Sleeper`/`thread::sleep` from production insertion.
- [ ] Add runtime timer stages and pending insertion ownership, including guarded restore in `Drop`.
- [ ] Add timestamped hotkey observations and tests proving delayed drain retains the real hold duration.
- [ ] Run focused insertion, hotkey, state, and flow tests.

### Task 2: Bounded realtime audio buffer

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/audio.rs`

**Interfaces:**
- Produces: bounded capture storage sized from source sample rate and `MAX_CAPTURE_MS`.
- Produces: `AudioError::BufferOverflow`.
- Consumes: interleaved CPAL sample blocks and existing `prepare_capture`.

- [ ] Add tests proving interleaved conversion/downmix writes expected mono samples and overflow returns an error without partial transcription.
- [ ] Run the focused tests and verify the missing bounded writer fails.
- [ ] Add a no-allocation SPSC dependency or a minimal project-local bounded SPSC implementation.
- [ ] Build the producer before stream creation and move it into each CPAL callback.
- [ ] Drain the consumer after stream shutdown and map atomic overflow to `BufferOverflow`.
- [ ] Run all audio and flow tests.

### Task 3: Real timeout for bundled-model initialization

**Files:**
- Modify: `src/main.rs`
- Modify: `src/runtime.rs`

**Interfaces:**
- Produces: hidden `--smoke-model-child` mode.
- Produces: parent watchdog with a 180-second deadline and timeout exit code `124`.
- Consumes: existing bundled resource resolution and ASR worker.

- [ ] Add tests for launch-mode parsing and watchdog success, failure, and timeout using a short-lived test command.
- [ ] Run focused tests and verify timeout behavior is missing.
- [ ] Move current in-process initialization into the child entry point.
- [ ] Implement parent spawn, polling deadline, child termination, and exit mapping.
- [ ] Run focused main/runtime tests.

### Task 4: CI and main-thread AppKit test harness

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `tests/pasteboard_main.rs`
- Modify: `Cargo.toml`
- Modify: `src/inserter.rs`
- Modify: `README.md`

**Interfaces:**
- Produces: callable pasteboard round-trip fixture for a custom `harness = false` test binary.
- Produces: arm64 macOS quality job and documented manual release gate.

- [ ] Move system pasteboard round-trip assertions behind a test-only callable entry point and create a custom main-thread test binary.
- [ ] Verify the new binary fails before the entry point exists, then passes without the background-thread warning.
- [ ] Add CI steps for format, all tests, Clippy with warnings denied, and `cargo audit`.
- [ ] Document that bundle/smoke verification requires pre-provisioned frozen model files and list the manual TCC/Fn release checks.
- [ ] Run workflow syntax checks available locally.

### Task 5: Full verification

**Files:**
- Verify all files changed by Tasks 1–4.

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo test --all-targets -- --test-threads=1` in a normal macOS user session.
- [ ] Run `cargo clippy --all-targets -- -D warnings`.
- [ ] Run `cargo build --release --target aarch64-apple-darwin`.
- [ ] Inspect the full diff for accidental product-scope changes.
- [ ] Record anything requiring the frozen model assets or manual TCC/Fn interaction as an explicit remaining release gate.

