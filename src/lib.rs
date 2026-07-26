#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("PTT2me supports only aarch64-apple-darwin");

pub mod constants;
pub mod state;
