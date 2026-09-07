//! C ABI for the controlled AppKit/WebKit fixture. Compile the exact production
//! insertion modules without exporting test APIs from the application library.
#![allow(dead_code)]

#[path = "../../src/inserter.rs"]
mod inserter;
#[path = "../../src/performance_diagnostics.rs"]
mod performance_diagnostics;
#[path = "../../src/text_inserter.rs"]
mod text_inserter;

use std::ffi::{c_char, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};

use inserter::InsertError;
use text_inserter::PendingTextInsertion;

fn status(action: impl FnOnce() -> Result<(), InsertError>) -> i32 {
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return 3;
    }
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(Ok(())) => 0,
        Ok(Err(InsertError::SecureField)) => 1,
        Ok(Err(_)) => 2,
        Err(_) => 3,
    }
}

/// # Safety
/// `text` is a valid NUL-terminated UTF-8 string; `output` is writable. All
/// handles must be used on the main thread and finished exactly once.
#[no_mangle]
pub unsafe extern "C" fn ptt_test_begin(
    text: *const c_char,
    append_space: bool,
    output: *mut *mut c_void,
) -> i32 {
    status(|| {
        unsafe { *output = std::ptr::null_mut() };
        let text = unsafe { CStr::from_ptr(text) }
            .to_str()
            .map_err(|_| InsertError::EmptyText)?;
        let pending = text_inserter::begin(text, append_space)?;
        unsafe { *output = Box::into_raw(Box::new(pending)).cast() };
        Ok(())
    })
}

/// # Safety
/// `handle` is a live handle returned by `ptt_test_begin`, on the main thread.
#[no_mangle]
pub unsafe extern "C" fn ptt_test_paste(handle: *mut c_void) -> i32 {
    status(|| unsafe { &mut *handle.cast::<PendingTextInsertion>() }.paste())
}

/// # Safety
/// `handle` is live and owned by the caller. This consumes it exactly once.
#[no_mangle]
pub unsafe extern "C" fn ptt_test_finish(handle: *mut c_void) -> i32 {
    status(|| unsafe { Box::from_raw(handle.cast::<PendingTextInsertion>()) }.restore())
}

#[no_mangle]
pub extern "C" fn ptt_test_settle_ms() -> u64 {
    inserter::PASTEBOARD_SETTLE_DELAY_MS
}

#[no_mangle]
pub extern "C" fn ptt_test_restore_ms() -> u64 {
    inserter::PASTEBOARD_RESTORE_DELAY_MS
}
