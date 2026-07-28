# PTT2me Audit Remediation Design

## Goal

Remove the two runtime risks and two delivery gaps confirmed by the 2026-07-27
audit without changing the product contract: one bundled GigaAM v3 model,
offline recognition, Fn/Globe push-to-talk, paste into the active application,
and guarded restoration of the complete pasteboard.

## Non-blocking insertion

Pasteboard work remains on the AppKit main thread. `inserter` exposes a
`PendingInsertion` transaction with three explicit stages:

1. snapshot the complete pasteboard and write temporary recognized text;
2. send Command-V after the 30 ms settle timer;
3. restore the snapshot after the 100 ms restore timer when `changeCount` still
   proves ownership.

`Runtime` owns the pending transaction and one non-repeating insertion timer.
No stage sleeps. A failed paste restores immediately. Runtime shutdown also
attempts a guarded restore.

Fn/Globe observations carry callback timestamps. Runtime queues hotkey signals
while insertion is pending and drains them after `PasteFinished`, so a rapid
press/release is neither lost nor measured as zero merely because main-thread
processing was delayed.

## Realtime-safe capture

The CPAL data callback performs conversion and downmix in one pass into a
bounded, preallocated single-producer/single-consumer ring buffer. Capacity is
derived from the configured source rate and the 25-second capture limit.
Callback code performs no heap allocation, mutex acquisition, or logging.

Overflow and stream callback failure are stored atomically. `stop` drops the
stream, drains captured mono samples, and returns a distinct error instead of
transcribing partial audio.

## Bounded model smoke

“Load” means initializing the one model already embedded in the application,
not downloading or selecting a model. The smoke path is split into:

- a hidden child mode that initializes the bundled model;
- the public `--smoke-model` parent, which waits up to 180 seconds and
  terminates the child on timeout.

Process isolation is required because a Rust thread stuck inside ONNX Runtime
cannot be safely interrupted and an unconditional `join` would defeat a channel
timeout.

## CI and release verification

The normal GitHub Actions job runs on an arm64 macOS runner and requires format,
tests, Clippy, and dependency audit. It does not download a runtime model.

The AppKit pasteboard round-trip runs through a custom test binary whose main
function executes on the process main thread. Bundle verification remains a
separate release job because the fixed model files are ignored source-tree
inputs. That job may run only where the exact frozen model assets have already
been provisioned and verified.

Hardware/TCC/Fn validation remains a documented manual release gate.

## Acceptance

- No `thread::sleep` remains in the insertion path.
- The audio data callback contains no `Vec` construction, mutex lock, or log.
- Overflow returns an explicit capture error.
- Model smoke exits with a distinct timeout status.
- PR CI blocks formatting, test, Clippy, or dependency-audit failures.
- All automated tests pass without AppKit background-thread warnings.
