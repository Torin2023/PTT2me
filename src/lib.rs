#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("PTT2me supports only aarch64-apple-darwin");

pub mod asr;
mod asr_process;
mod asr_protocol;
mod asr_task;

/// Hidden worker entry point; dispatched before all application initialization.
pub fn run_asr_worker_process() -> i32 {
    asr_protocol::run_native_worker()
}
pub mod audio;
mod audio_task;
mod browser_accessibility;
pub mod constants;
mod event_wake;
pub mod hotkey;
pub mod inserter;
pub mod logging;
pub mod menu;
pub mod model;
pub mod model_store;
pub mod output_preferences;
pub mod permission_migration;
pub mod permissions;
pub mod preferences;
pub mod release_manifest;
pub mod runtime;
pub mod single_instance;
pub mod state;
pub(crate) mod text_inserter;
pub mod update_manifest;
pub mod updater;
pub(crate) mod updater_runtime;

/// Process fixtures are compiled only for explicit test-support builds.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod asr_test_support {
    pub use crate::asr_protocol::{decode_samples, isolate_stdout, Frame, Kind, MAX_SAMPLES};
    pub use crate::asr_task::{
        AsrOperation, AsrTask, AsrTaskError, MODEL_LOAD_TIMEOUT, TRANSCRIPTION_TIMEOUT,
    };
}
