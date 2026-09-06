use std::path::PathBuf;
use std::process::ExitCode;

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::MainThreadMarker;
use ptt2me::permissions::prime_microphone_and_exit;
use ptt2me::release_manifest::verify_release_files;
use ptt2me::runtime::{finish_after_run, smoke_bundled_model, smoke_bundled_model_child, Runtime};
use ptt2me::single_instance::InstanceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchMode {
    App,
    AsrWorker,
    PrimeMicrophone,
    SmokeModel,
    SmokeModelChild,
    VerifyUpdateManifest {
        public_key: PathBuf,
        manifest: PathBuf,
        full_dmg: PathBuf,
        update_dmg: PathBuf,
        model_manifest: PathBuf,
    },
}

fn main() -> ExitCode {
    let mode = match parse_launch_mode(std::env::args_os().skip(1)) {
        Ok(mode) => mode,
        Err(()) => return ExitCode::from(2),
    };
    if let LaunchMode::VerifyUpdateManifest {
        public_key,
        manifest,
        full_dmg,
        update_dmg,
        model_manifest,
    } = &mode
    {
        return match verify_release_files(
            public_key,
            manifest,
            full_dmg,
            update_dmg,
            model_manifest,
        ) {
            Ok(release) => {
                println!("version={}", release.version);
                println!("source_commit={}", release.source_commit);
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("PTT2me update manifest verification failed: {error}");
                ExitCode::FAILURE
            }
        };
    }

    if mode == LaunchMode::AsrWorker {
        return ExitCode::from(ptt2me::run_asr_worker_process() as u8);
    }

    ptt2me::logging::init();
    match mode {
        LaunchMode::PrimeMicrophone => {
            return ExitCode::from(prime_microphone_and_exit() as u8);
        }
        LaunchMode::SmokeModel => {
            return ExitCode::from(smoke_bundled_model() as u8);
        }
        LaunchMode::SmokeModelChild => {
            return ExitCode::from(smoke_bundled_model_child() as u8);
        }
        LaunchMode::App => {}
        LaunchMode::VerifyUpdateManifest { .. } | LaunchMode::AsrWorker => unreachable!(),
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
    let mut runtime = Runtime::start(main_thread);
    unsafe { application.run() };
    finish_after_run(&mut runtime);
    ExitCode::SUCCESS
}

fn parse_launch_mode(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<LaunchMode, ()> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(LaunchMode::App),
        [argument] if argument == "--asr-worker" => Ok(LaunchMode::AsrWorker),
        [argument] if argument == "--prime-microphone-and-exit" => Ok(LaunchMode::PrimeMicrophone),
        [argument] if argument == "--smoke-model" => Ok(LaunchMode::SmokeModel),
        [argument] if argument == "--smoke-model-child" => Ok(LaunchMode::SmokeModelChild),
        [mode, public_key, manifest, full_dmg, update_dmg, model_manifest]
            if mode == "--verify-update-manifest" =>
        {
            Ok(LaunchMode::VerifyUpdateManifest {
                public_key: PathBuf::from(public_key),
                manifest: PathBuf::from(manifest),
                full_dmg: PathBuf::from(full_dmg),
                update_dmg: PathBuf::from(update_dmg),
                model_manifest: PathBuf::from(model_manifest),
            })
        }
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
    fn recognizes_only_the_hidden_modes() {
        assert_eq!(parse_launch_mode(Vec::new()), Ok(LaunchMode::App));
        assert_eq!(
            parse_launch_mode([OsString::from("--prime-microphone-and-exit")]),
            Ok(LaunchMode::PrimeMicrophone)
        );
        assert_eq!(
            parse_launch_mode([OsString::from("--smoke-model")]),
            Ok(LaunchMode::SmokeModel)
        );
        assert_eq!(
            parse_launch_mode([OsString::from("--smoke-model-child")]),
            Ok(LaunchMode::SmokeModelChild)
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
