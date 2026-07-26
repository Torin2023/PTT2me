use tracing_subscriber::prelude::*;

use crate::constants::BUNDLE_ID;

/// Routes technical lifecycle events to macOS Unified Logging.
///
/// Call sites must never attach recognized text or audio samples as fields.
pub fn init() {
    let _ = tracing_subscriber::registry()
        .with(tracing_oslog::OsLogger::new(BUNDLE_ID, "runtime"))
        .try_init();
}
