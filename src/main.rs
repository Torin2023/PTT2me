use std::process::ExitCode;

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::MainThreadMarker;
use ptt2me::permissions::prime_microphone_and_exit;
use ptt2me::runtime::{smoke_bundled_model, Runtime};
use ptt2me::single_instance::InstanceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchMode {
    App,
    PrimeMicrophone,
    SmokeModel,
}

fn main() -> ExitCode {
    ptt2me::logging::init();

    let mode = match parse_launch_mode(std::env::args_os().skip(1)) {
        Ok(mode) => mode,
        Err(()) => return ExitCode::from(2),
    };
    match mode {
        LaunchMode::PrimeMicrophone => {
            return ExitCode::from(prime_microphone_and_exit() as u8);
        }
        LaunchMode::SmokeModel => {
            return ExitCode::from(smoke_bundled_model() as u8);
        }
        LaunchMode::App => {}
    }

    let Ok(_instance_lock) = InstanceLock::acquire() else {
        return ExitCode::FAILURE;
    };
    let Some(main_thread) = MainThreadMarker::new() else {
        return ExitCode::FAILURE;
    };
    let application = NSApplication::sharedApplication(main_thread);
    let policy_set = application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let current_policy = unsafe { application.activationPolicy() };
    if !activation_policy_allows_launch(current_policy, policy_set) {
        tracing::error!(error_category = "app_activation_policy");
        return ExitCode::FAILURE;
    }
    let _runtime = Runtime::start(main_thread);
    unsafe { application.run() };
    ExitCode::SUCCESS
}

fn parse_launch_mode(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<LaunchMode, ()> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(LaunchMode::App),
        [argument] if argument == "--prime-microphone-and-exit" => Ok(LaunchMode::PrimeMicrophone),
        [argument] if argument == "--smoke-model" => Ok(LaunchMode::SmokeModel),
        _ => Err(()),
    }
}

fn activation_policy_allows_launch(
    current_policy: NSApplicationActivationPolicy,
    set_succeeded: bool,
) -> bool {
    set_succeeded || current_policy == NSApplicationActivationPolicy::Accessory
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{activation_policy_allows_launch, parse_launch_mode, LaunchMode};
    use objc2_app_kit::NSApplicationActivationPolicy;

    #[test]
    fn recognizes_only_the_two_hidden_modes() {
        assert_eq!(parse_launch_mode(Vec::new()), Ok(LaunchMode::App));
        assert_eq!(
            parse_launch_mode([OsString::from("--prime-microphone-and-exit")]),
            Ok(LaunchMode::PrimeMicrophone)
        );
        assert_eq!(
            parse_launch_mode([OsString::from("--smoke-model")]),
            Ok(LaunchMode::SmokeModel)
        );
    }

    #[test]
    fn rejects_unknown_or_combined_arguments() {
        assert_eq!(parse_launch_mode([OsString::from("--unknown")]), Err(()));
        assert_eq!(
            parse_launch_mode([
                OsString::from("--smoke-model"),
                OsString::from("--prime-microphone-and-exit"),
            ]),
            Err(())
        );
    }

    #[test]
    fn already_accessory_launches_when_redundant_setter_returns_false() {
        assert!(activation_policy_allows_launch(
            NSApplicationActivationPolicy::Accessory,
            false,
        ));
    }
}
