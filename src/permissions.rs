use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use block2::RcBlock;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

use crate::state::{PermissionKind, PermissionSnapshot};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
}

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {
    static AVMediaTypeAudio: *const AnyObject;
}

const IOHID_REQUEST_LISTEN_EVENT: u32 = 1;
const IOHID_ACCESS_GRANTED: u32 = 0;
const AV_NOT_DETERMINED: isize = 0;
const AV_RESTRICTED: isize = 1;
const AV_DENIED: isize = 2;
const AV_AUTHORIZED: isize = 3;
const MICROPHONE_PRIME_MS: u64 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneAuthorization {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    Unknown,
}

impl MicrophoneAuthorization {
    const fn from_raw(status: isize) -> Self {
        match status {
            AV_NOT_DETERMINED => Self::NotDetermined,
            AV_RESTRICTED => Self::Restricted,
            AV_DENIED => Self::Denied,
            AV_AUTHORIZED => Self::Authorized,
            _ => Self::Unknown,
        }
    }

    const fn is_authorized(self) -> bool {
        matches!(self, Self::Authorized)
    }
}

pub trait MicrophonePermissionBoundary {
    fn request_access(&mut self) -> bool;
    fn open_settings(&mut self) -> bool;
}

/// One-process coordinator for the asynchronous microphone consent prompt.
///
/// It never prompts more than once and opens System Settings at most once
/// while authorization remains missing.
#[derive(Default)]
pub struct MicrophonePermissionFlow {
    request_started: bool,
    settings_opened: bool,
}

impl MicrophonePermissionFlow {
    pub fn permission_needed(
        &mut self,
        authorization: MicrophoneAuthorization,
        boundary: &mut impl MicrophonePermissionBoundary,
    ) {
        match authorization {
            MicrophoneAuthorization::NotDetermined if !self.request_started => {
                self.request_started = true;
                if !boundary.request_access() {
                    self.open_settings_once(boundary);
                }
            }
            MicrophoneAuthorization::Denied
            | MicrophoneAuthorization::Restricted
            | MicrophoneAuthorization::Unknown => self.open_settings_once(boundary),
            MicrophoneAuthorization::Authorized => {
                self.settings_opened = false;
            }
            MicrophoneAuthorization::NotDetermined => {}
        }
    }

    pub fn request_completed(
        &mut self,
        authorization: MicrophoneAuthorization,
        boundary: &mut impl MicrophonePermissionBoundary,
    ) {
        if matches!(
            authorization,
            MicrophoneAuthorization::Denied
                | MicrophoneAuthorization::Restricted
                | MicrophoneAuthorization::Unknown
        ) {
            self.open_settings_once(boundary);
        }
    }

    fn open_settings_once(&mut self, boundary: &mut impl MicrophonePermissionBoundary) {
        if !self.settings_opened {
            self.settings_opened = true;
            let _ = boundary.open_settings();
        }
    }
}

/// Read-only probes for the three macOS permissions PTT2me requires.
pub struct SystemPermissionProbe;

impl SystemPermissionProbe {
    /// Reads cached TCC authorization state without requesting any permission.
    pub fn check() -> PermissionSnapshot {
        PermissionSnapshot {
            accessibility: unsafe { AXIsProcessTrusted() },
            input_monitoring: unsafe {
                IOHIDCheckAccess(IOHID_REQUEST_LISTEN_EVENT) == IOHID_ACCESS_GRANTED
            },
            microphone: Self::microphone_authorization().is_authorized(),
        }
    }

    pub fn microphone_authorization() -> MicrophoneAuthorization {
        MicrophoneAuthorization::from_raw(microphone_authorization_status())
    }
}

/// Opens the System Settings pane for one of PTT2me's required permissions.
pub fn open_settings(permission: PermissionKind) -> bool {
    let url_string = NSString::from_str(settings_url(permission));
    let Some(url) = (unsafe { NSURL::URLWithString(&url_string) }) else {
        return false;
    };

    unsafe { NSWorkspace::sharedWorkspace().openURL(&url) }
}

/// Primes macOS's microphone consent by briefly opening the default input.
///
/// Returns a shell-compatible exit status: `0` after a stream was played for
/// 150 ms, or `1` if no usable default input stream is available.
pub fn prime_microphone_and_exit() -> i32 {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return 1;
    };
    let Ok(supported_config) = device.default_input_config() else {
        return 1;
    };
    let sample_format = supported_config.sample_format();
    let config: cpal::StreamConfig = supported_config.into();
    let callback_failed = Arc::new(AtomicBool::new(false));
    let callback_failure_for_stream = Arc::clone(&callback_failed);
    let error_callback = move |_| {
        callback_failure_for_stream.store(true, Ordering::Release);
        tracing::warn!("microphone prime stream error");
    };

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            device.build_input_stream(&config, |_: &[f32], _| {}, error_callback, None)
        }
        cpal::SampleFormat::I16 => {
            device.build_input_stream(&config, |_: &[i16], _| {}, error_callback, None)
        }
        cpal::SampleFormat::U16 => {
            device.build_input_stream(&config, |_: &[u16], _| {}, error_callback, None)
        }
        cpal::SampleFormat::F64 => {
            device.build_input_stream(&config, |_: &[f64], _| {}, error_callback, None)
        }
        _ => return 1,
    };
    let Ok(stream) = stream else {
        return 1;
    };
    if stream.play().is_err() {
        return 1;
    }

    thread::sleep(Duration::from_millis(MICROPHONE_PRIME_MS));
    drop(stream);
    prime_exit_code(callback_failed.load(Ordering::Acquire))
}

const fn prime_exit_code(callback_failed: bool) -> i32 {
    if callback_failed {
        1
    } else {
        0
    }
}

pub const fn settings_url(permission: PermissionKind) -> &'static str {
    match permission {
        PermissionKind::Accessibility => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        }
        PermissionKind::InputMonitoring => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
        }
        PermissionKind::Microphone => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        }
    }
}

fn microphone_authorization_status() -> isize {
    let Some(device_class) = AnyClass::get("AVCaptureDevice") else {
        return -1;
    };

    unsafe { msg_send![device_class, authorizationStatusForMediaType: AVMediaTypeAudio] }
}

/// Starts AVFoundation's asynchronous, one-shot microphone authorization
/// prompt. The callback owns `completion` until macOS invokes the copied block.
pub fn request_microphone_access(completion: impl Fn() + Send + Sync + 'static) -> bool {
    let Some(device_class) = AnyClass::get("AVCaptureDevice") else {
        return false;
    };
    let completion: RcBlock<dyn Fn(Bool)> = RcBlock::new(move |_granted: Bool| completion());

    unsafe {
        let _: () = msg_send![
            device_class,
            requestAccessForMediaType: AVMediaTypeAudio,
            completionHandler: &*completion
        ];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{
        prime_exit_code, settings_url, MicrophoneAuthorization, MicrophonePermissionBoundary,
        MicrophonePermissionFlow,
    };
    use crate::state::{PermissionKind, PermissionSnapshot};

    #[derive(Default)]
    struct RecordingBoundary {
        effects: Vec<&'static str>,
        request_succeeds: bool,
    }

    impl MicrophonePermissionBoundary for RecordingBoundary {
        fn request_access(&mut self) -> bool {
            self.effects.push("request");
            self.request_succeeds
        }

        fn open_settings(&mut self) -> bool {
            self.effects.push("open");
            true
        }
    }

    #[test]
    fn missing_permissions_are_requested_in_required_order() {
        assert_eq!(
            PermissionSnapshot {
                accessibility: false,
                input_monitoring: false,
                microphone: false,
            }
            .next_missing(),
            Some(PermissionKind::Accessibility)
        );
        assert_eq!(
            PermissionSnapshot {
                accessibility: true,
                input_monitoring: false,
                microphone: false,
            }
            .next_missing(),
            Some(PermissionKind::InputMonitoring)
        );
        assert_eq!(
            PermissionSnapshot {
                accessibility: true,
                input_monitoring: true,
                microphone: false,
            }
            .next_missing(),
            Some(PermissionKind::Microphone)
        );
        assert_eq!(PermissionSnapshot::all().next_missing(), None);
    }

    #[test]
    fn settings_urls_are_limited_to_the_three_required_grants() {
        assert_eq!(
            settings_url(PermissionKind::Accessibility),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        );
        assert_eq!(
            settings_url(PermissionKind::InputMonitoring),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
        );
        assert_eq!(
            settings_url(PermissionKind::Microphone),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        );
    }

    #[test]
    fn microphone_prime_callback_failure_returns_error_exit_code() {
        assert_eq!(prime_exit_code(false), 0);
        assert_eq!(prime_exit_code(true), 1);
    }

    #[test]
    fn not_determined_microphone_requests_once_then_rechecks_before_opening_settings() {
        let mut flow = MicrophonePermissionFlow::default();
        let mut boundary = RecordingBoundary {
            request_succeeds: true,
            ..RecordingBoundary::default()
        };

        flow.permission_needed(MicrophoneAuthorization::NotDetermined, &mut boundary);
        flow.permission_needed(MicrophoneAuthorization::NotDetermined, &mut boundary);
        assert_eq!(boundary.effects, vec!["request"]);

        flow.request_completed(MicrophoneAuthorization::Denied, &mut boundary);
        flow.request_completed(MicrophoneAuthorization::Denied, &mut boundary);
        assert_eq!(boundary.effects, vec!["request", "open"]);
    }

    #[test]
    fn denied_or_restricted_microphone_never_triggers_an_authorization_request() {
        for authorization in [
            MicrophoneAuthorization::Denied,
            MicrophoneAuthorization::Restricted,
        ] {
            let mut flow = MicrophonePermissionFlow::default();
            let mut boundary = RecordingBoundary {
                request_succeeds: true,
                ..RecordingBoundary::default()
            };

            flow.permission_needed(authorization, &mut boundary);
            flow.permission_needed(authorization, &mut boundary);

            assert_eq!(boundary.effects, vec!["open"]);
        }
    }
}
