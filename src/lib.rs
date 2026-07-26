#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("PTT2me supports only aarch64-apple-darwin");

pub mod asr;
pub mod audio;
pub mod constants;
pub mod hotkey;
pub mod inserter;
pub mod menu;
pub mod model;
pub mod permissions;
pub mod state;
