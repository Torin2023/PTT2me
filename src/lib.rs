#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("PTT2me supports only aarch64-apple-darwin");

pub mod asr;
mod asr_task;
pub mod audio;
mod browser_accessibility;
pub mod constants;
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
