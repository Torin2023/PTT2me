use std::collections::BTreeSet;

use crate::constants::{ERROR_VISIBLE_MS, RELEASE_GRACE_MS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionKind {
    Accessibility,
    InputMonitoring,
    Microphone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PermissionSnapshot {
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub microphone: bool,
}

impl PermissionSnapshot {
    pub const fn all() -> Self {
        Self {
            accessibility: true,
            input_monitoring: true,
            microphone: true,
        }
    }

    pub const fn next_missing(self) -> Option<PermissionKind> {
        if !self.accessibility {
            Some(PermissionKind::Accessibility)
        } else if !self.input_monitoring {
            Some(PermissionKind::InputMonitoring)
        } else if !self.microphone {
            Some(PermissionKind::Microphone)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppStatus {
    PreparingModel,
    AsrUnavailable,
    AsrCleanupPending,
    ModelRepairRequired,
    ModelPreparationFailed,
    ResettingPermissions,
    PermissionResetFailed,
    PermissionBlocked(PermissionKind),
    Ready,
    Recording,
    Recognizing,
    Error {
        message: &'static str,
        recoverable: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPreparationFailure {
    RepairRequired,
    Storage,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    ModelPreparationStarted,
    ModelPreparationFailed(ModelPreparationFailure),
    PermissionMigrationStarted,
    PermissionMigrationCompleted,
    PermissionMigrationFailed,
    ModelLoaded(Result<(), String>),
    AsrTimedOut,
    AsrRecoveryStarted,
    AsrUnavailable,
    AsrCleanupPending,
    PermissionsChanged(PermissionSnapshot),
    TriggerPressed,
    TriggerReleased { short: bool },
    TriggerCancelled,
    CaptureLimitReached,
    CaptureFailed,
    AudioReady(Option<Vec<f32>>),
    RecognitionFinished(Result<String, String>),
    // Command-V completed; delayed clipboard restoration has its own event.
    PasteFinished(Result<(), String>),
    ClipboardRestoreFailed,
    ErrorTimerFired,
    EventTapLost,
    EventTapRestored,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    OpenPermission(PermissionKind),
    StartCapture,
    AbortCapture,
    FinishCaptureAfter { delay_ms: u64 },
    Recognize(Vec<f32>),
    InsertText(String),
    ScheduleErrorReset { delay_ms: u64 },
}

pub struct AppController {
    status: AppStatus,
    model_ready: bool,
    permissions: PermissionSnapshot,
    opened_permission_panes: BTreeSet<PermissionKind>,
}

impl AppController {
    pub fn new() -> Self {
        Self {
            status: AppStatus::PreparingModel,
            model_ready: false,
            permissions: PermissionSnapshot::default(),
            opened_permission_panes: BTreeSet::new(),
        }
    }

    pub fn status(&self) -> &AppStatus {
        &self.status
    }

    pub fn handle(&mut self, event: AppEvent) -> Vec<Effect> {
        match event {
            AppEvent::ModelPreparationStarted | AppEvent::AsrRecoveryStarted => {
                self.model_ready = false;
                self.status = AppStatus::PreparingModel;
                Vec::new()
            }
            AppEvent::ModelPreparationFailed(ModelPreparationFailure::RepairRequired) => {
                self.model_ready = false;
                self.status = AppStatus::ModelRepairRequired;
                Vec::new()
            }
            AppEvent::ModelPreparationFailed(ModelPreparationFailure::Storage) => {
                self.model_ready = false;
                self.status = AppStatus::ModelPreparationFailed;
                Vec::new()
            }
            AppEvent::PermissionMigrationStarted => {
                self.model_ready = false;
                self.status = AppStatus::ResettingPermissions;
                Vec::new()
            }
            AppEvent::PermissionMigrationCompleted => {
                self.status = AppStatus::PreparingModel;
                Vec::new()
            }
            AppEvent::PermissionMigrationFailed => {
                self.model_ready = false;
                self.status = AppStatus::PermissionResetFailed;
                Vec::new()
            }
            AppEvent::ModelLoaded(Ok(())) if self.status == AppStatus::PreparingModel => {
                self.model_ready = true;
                self.enter_idle_state()
            }
            AppEvent::AsrCleanupPending => {
                self.model_ready = false;
                self.status = AppStatus::AsrCleanupPending;
                Vec::new()
            }
            AppEvent::AsrTimedOut | AppEvent::AsrUnavailable | AppEvent::ModelLoaded(Err(_)) => {
                self.model_ready = false;
                self.status = AppStatus::AsrUnavailable;
                Vec::new()
            }
            AppEvent::PermissionsChanged(permissions) => {
                self.permissions = permissions;
                self.forget_granted_permission_panes();
                if self.model_ready
                    && !matches!(self.status, AppStatus::Recording | AppStatus::Recognizing)
                {
                    self.enter_idle_state()
                } else {
                    Vec::new()
                }
            }
            AppEvent::TriggerPressed if self.status == AppStatus::Ready => {
                self.status = AppStatus::Recording;
                vec![Effect::StartCapture]
            }
            AppEvent::TriggerReleased { short } if self.status == AppStatus::Recording => {
                if short {
                    self.status = AppStatus::Ready;
                    vec![Effect::AbortCapture]
                } else {
                    self.status = AppStatus::Recognizing;
                    vec![Effect::FinishCaptureAfter {
                        delay_ms: RELEASE_GRACE_MS,
                    }]
                }
            }
            AppEvent::TriggerCancelled if self.status == AppStatus::Recording => {
                self.status = AppStatus::Ready;
                vec![Effect::AbortCapture]
            }
            AppEvent::CaptureLimitReached if self.status == AppStatus::Recording => {
                self.status = AppStatus::Recognizing;
                vec![Effect::FinishCaptureAfter { delay_ms: 0 }]
            }
            AppEvent::CaptureFailed
                if matches!(self.status, AppStatus::Recording | AppStatus::Recognizing) =>
            {
                self.show_recoverable_error("Не удалось записать звук")
            }
            AppEvent::AudioReady(None) if self.status == AppStatus::Recognizing => {
                self.enter_idle_state()
            }
            AppEvent::AudioReady(Some(samples)) if self.status == AppStatus::Recognizing => {
                vec![Effect::Recognize(samples)]
            }
            AppEvent::RecognitionFinished(Ok(text)) if self.status == AppStatus::Recognizing => {
                let text = text.trim();
                if text.is_empty() {
                    self.enter_idle_state()
                } else {
                    vec![Effect::InsertText(text.to_owned())]
                }
            }
            AppEvent::RecognitionFinished(Err(_)) if self.status == AppStatus::Recognizing => {
                self.show_recoverable_error("Не удалось распознать речь")
            }
            AppEvent::PasteFinished(Ok(())) if self.status == AppStatus::Recognizing => {
                self.enter_idle_state()
            }
            AppEvent::PasteFinished(Err(_)) if self.status == AppStatus::Recognizing => {
                self.show_recoverable_error("Не удалось вставить текст")
            }
            AppEvent::ClipboardRestoreFailed if self.status == AppStatus::Ready => self
                .show_recoverable_error("Текст вставлен, но не удалось восстановить буфер обмена"),
            AppEvent::ErrorTimerFired
                if matches!(
                    self.status,
                    AppStatus::Error {
                        recoverable: true,
                        ..
                    }
                ) =>
            {
                self.enter_idle_state()
            }
            AppEvent::EventTapLost if self.model_ready => {
                self.show_recoverable_error("Глобальная клавиша недоступна")
            }
            AppEvent::EventTapRestored if self.is_event_tap_error() => self.enter_idle_state(),
            _ => Vec::new(),
        }
    }

    fn enter_idle_state(&mut self) -> Vec<Effect> {
        if !self.model_ready {
            self.status = AppStatus::PreparingModel;
            return Vec::new();
        }

        if let Some(permission) = self.permissions.next_missing() {
            self.status = AppStatus::PermissionBlocked(permission);
            if self.opened_permission_panes.insert(permission) {
                vec![Effect::OpenPermission(permission)]
            } else {
                Vec::new()
            }
        } else {
            self.status = AppStatus::Ready;
            Vec::new()
        }
    }

    fn forget_granted_permission_panes(&mut self) {
        if self.permissions.accessibility {
            self.opened_permission_panes
                .remove(&PermissionKind::Accessibility);
        }
        if self.permissions.input_monitoring {
            self.opened_permission_panes
                .remove(&PermissionKind::InputMonitoring);
        }
        if self.permissions.microphone {
            self.opened_permission_panes
                .remove(&PermissionKind::Microphone);
        }
    }

    fn show_recoverable_error(&mut self, message: &'static str) -> Vec<Effect> {
        self.status = AppStatus::Error {
            message,
            recoverable: true,
        };
        vec![Effect::ScheduleErrorReset {
            delay_ms: ERROR_VISIBLE_MS,
        }]
    }

    fn is_event_tap_error(&self) -> bool {
        self.status
            == (AppStatus::Error {
                message: "Глобальная клавиша недоступна",
                recoverable: true,
            })
    }

    #[cfg(test)]
    fn ready_for_test() -> Self {
        Self {
            status: AppStatus::Ready,
            model_ready: true,
            permissions: PermissionSnapshot::all(),
            opened_permission_panes: BTreeSet::new(),
        }
    }

    #[cfg(test)]
    fn recording_for_test() -> Self {
        Self {
            status: AppStatus::Recording,
            ..Self::ready_for_test()
        }
    }

    #[cfg(test)]
    fn recognizing_for_test() -> Self {
        Self {
            status: AppStatus::Recognizing,
            ..Self::ready_for_test()
        }
    }
}

impl Default for AppController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_readiness_requires_loaded_and_current_permissions() {
        let mut controller = AppController::recognizing_for_test();
        controller.handle(AppEvent::AsrRecoveryStarted);
        for event in [
            AppEvent::PermissionsChanged(PermissionSnapshot::all()),
            AppEvent::ErrorTimerFired,
            AppEvent::EventTapLost,
            AppEvent::EventTapRestored,
            AppEvent::CaptureFailed,
            AppEvent::RecognitionFinished(Ok("synthetic late text".into())),
            AppEvent::TriggerPressed,
        ] {
            assert!(controller.handle(event).is_empty());
            assert_eq!(controller.status(), &AppStatus::PreparingModel);
        }
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot {
            microphone: false,
            ..PermissionSnapshot::all()
        }));
        assert_eq!(
            controller.handle(AppEvent::ModelLoaded(Ok(()))),
            vec![Effect::OpenPermission(PermissionKind::Microphone)]
        );
        assert_eq!(
            controller.status(),
            &AppStatus::PermissionBlocked(PermissionKind::Microphone)
        );
        controller.handle(AppEvent::AsrUnavailable);
        assert!(
            controller.handle(AppEvent::ModelLoaded(Ok(()))).is_empty(),
            "unsolicited old loaded cannot restore readiness"
        );
        assert_eq!(controller.status(), &AppStatus::AsrUnavailable);
        controller.handle(AppEvent::AsrRecoveryStarted);
        controller.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        controller.handle(AppEvent::ModelLoaded(Ok(())));
        assert_eq!(controller.status(), &AppStatus::Ready);
    }

    #[test]
    fn permission_notifications_do_not_abandon_active_dictation() {
        for mut controller in [
            AppController::recording_for_test(),
            AppController::recognizing_for_test(),
        ] {
            let before = controller.status().clone();
            assert!(controller
                .handle(AppEvent::PermissionsChanged(PermissionSnapshot::default()))
                .is_empty());
            assert_eq!(controller.status(), &before);
        }
    }

    #[test]
    fn model_preparation_failure_is_targeted_and_retryable() {
        let mut controller = AppController::new();
        assert_eq!(controller.status(), &AppStatus::PreparingModel);

        controller.handle(AppEvent::ModelPreparationFailed(
            ModelPreparationFailure::RepairRequired,
        ));
        assert_eq!(controller.status(), &AppStatus::ModelRepairRequired);

        controller.handle(AppEvent::ModelPreparationStarted);
        assert_eq!(controller.status(), &AppStatus::PreparingModel);

        controller.handle(AppEvent::ModelPreparationFailed(
            ModelPreparationFailure::Storage,
        ));
        assert_eq!(controller.status(), &AppStatus::ModelPreparationFailed);
    }

    #[test]
    fn permission_reset_failure_stays_blocked_until_targeted_retry() {
        let mut controller = AppController::new();

        controller.handle(AppEvent::PermissionMigrationStarted);
        assert_eq!(controller.status(), &AppStatus::ResettingPermissions);
        controller.handle(AppEvent::PermissionMigrationFailed);
        assert_eq!(controller.status(), &AppStatus::PermissionResetFailed);

        assert!(controller
            .handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()))
            .is_empty());
        assert!(controller.handle(AppEvent::TriggerPressed).is_empty());
        assert_eq!(controller.status(), &AppStatus::PermissionResetFailed);

        controller.handle(AppEvent::PermissionMigrationStarted);
        assert_eq!(controller.status(), &AppStatus::ResettingPermissions);
        controller.handle(AppEvent::PermissionMigrationCompleted);
        assert_eq!(controller.status(), &AppStatus::PreparingModel);
    }

    #[test]
    fn trigger_press_starts_capture_immediately() {
        let mut controller = AppController::ready_for_test();
        assert_eq!(
            controller.handle(AppEvent::TriggerPressed),
            vec![Effect::StartCapture]
        );
    }

    #[test]
    fn short_release_and_combination_cancel_capture() {
        for event in [
            AppEvent::TriggerReleased { short: true },
            AppEvent::TriggerCancelled,
        ] {
            let mut controller = AppController::recording_for_test();
            assert_eq!(controller.handle(event), vec![Effect::AbortCapture]);
            assert_eq!(controller.status(), &AppStatus::Ready);
        }
    }

    #[test]
    fn long_release_finishes_capture() {
        let mut controller = AppController::recording_for_test();
        assert_eq!(
            controller.handle(AppEvent::TriggerReleased { short: false }),
            vec![Effect::FinishCaptureAfter { delay_ms: 180 }]
        );
    }

    #[test]
    fn recognition_is_trimmed_then_inserted() {
        let mut c = AppController::recognizing_for_test();
        assert_eq!(
            c.handle(AppEvent::RecognitionFinished(Ok("  привет.  ".into()))),
            vec![Effect::InsertText("привет.".into())]
        );
    }

    #[test]
    fn busy_press_is_ignored() {
        let mut c = AppController::recognizing_for_test();
        assert!(c.handle(AppEvent::TriggerPressed).is_empty());
    }

    #[test]
    fn model_load_failure_is_persistent() {
        let mut c = AppController::new();
        assert!(c
            .handle(AppEvent::ModelLoaded(Err("missing model".into())))
            .is_empty());
        assert_eq!(c.status(), &AppStatus::AsrUnavailable);
        assert!(c.handle(AppEvent::ErrorTimerFired).is_empty());
        assert_eq!(c.status(), &AppStatus::AsrUnavailable);
    }

    #[test]
    fn event_tap_loss_does_not_replace_persistent_model_failure() {
        let mut c = AppController::new();
        c.handle(AppEvent::ModelLoaded(Err("missing model".into())));

        assert!(c.handle(AppEvent::EventTapLost).is_empty());
        assert_eq!(c.status(), &AppStatus::AsrUnavailable);
    }

    #[test]
    fn event_tap_restoration_does_not_clear_persistent_model_failure() {
        let mut c = AppController::new();
        c.handle(AppEvent::ModelLoaded(Err("missing model".into())));

        assert!(c.handle(AppEvent::EventTapRestored).is_empty());
        assert_eq!(c.status(), &AppStatus::AsrUnavailable);
    }

    #[test]
    fn missing_permissions_open_in_accessibility_input_microphone_order() {
        let mut c = AppController::new();
        assert_eq!(
            c.handle(AppEvent::ModelLoaded(Ok(()))),
            vec![Effect::OpenPermission(PermissionKind::Accessibility)]
        );
        assert_eq!(
            c.handle(AppEvent::PermissionsChanged(PermissionSnapshot {
                accessibility: true,
                input_monitoring: false,
                microphone: false,
            })),
            vec![Effect::OpenPermission(PermissionKind::InputMonitoring)]
        );
        assert_eq!(
            c.handle(AppEvent::PermissionsChanged(PermissionSnapshot {
                accessibility: true,
                input_monitoring: true,
                microphone: false,
            })),
            vec![Effect::OpenPermission(PermissionKind::Microphone)]
        );
    }

    #[test]
    fn granted_permission_can_be_opened_again_after_revocation() {
        let mut c = AppController::new();
        c.handle(AppEvent::ModelLoaded(Ok(())));
        c.handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()));
        assert_eq!(
            c.handle(AppEvent::PermissionsChanged(PermissionSnapshot {
                accessibility: false,
                ..PermissionSnapshot::all()
            })),
            vec![Effect::OpenPermission(PermissionKind::Accessibility)]
        );
    }

    #[test]
    fn empty_recognition_returns_to_ready_without_pasting() {
        let mut c = AppController::recognizing_for_test();
        assert!(c
            .handle(AppEvent::RecognitionFinished(Ok(" \n\t ".into())))
            .is_empty());
        assert_eq!(c.status(), &AppStatus::Ready);
    }

    #[test]
    fn asr_timeout_blocks_new_dictation_and_cannot_be_reset_by_permissions() {
        let mut c = AppController::recognizing_for_test();
        assert!(c.handle(AppEvent::AsrTimedOut).is_empty());
        let timed_out = c.status().clone();
        assert!(matches!(timed_out, AppStatus::AsrUnavailable));
        for event in [
            AppEvent::ErrorTimerFired,
            AppEvent::TriggerPressed,
            AppEvent::PermissionsChanged(PermissionSnapshot::all()),
            AppEvent::RecognitionFinished(Ok("late result".into())),
            AppEvent::EventTapRestored,
        ] {
            assert!(c.handle(event).is_empty());
            assert_eq!(c.status(), &timed_out);
        }
    }

    #[test]
    fn paste_failure_is_visible_then_resets() {
        let mut c = AppController::recognizing_for_test();
        assert_eq!(
            c.handle(AppEvent::PasteFinished(Err("clipboard unavailable".into()))),
            vec![Effect::ScheduleErrorReset { delay_ms: 3_000 }]
        );
        assert_eq!(
            c.status(),
            &AppStatus::Error {
                message: "Не удалось вставить текст",
                recoverable: true,
            }
        );
        assert!(c.handle(AppEvent::ErrorTimerFired).is_empty());
        assert_eq!(c.status(), &AppStatus::Ready);
    }

    #[test]
    fn capture_failure_is_transient() {
        let mut c = AppController::recording_for_test();
        assert_eq!(
            c.handle(AppEvent::CaptureFailed),
            vec![Effect::ScheduleErrorReset { delay_ms: 3_000 }]
        );
        assert_eq!(
            c.status(),
            &AppStatus::Error {
                message: "Не удалось записать звук",
                recoverable: true,
            }
        );
    }

    #[test]
    fn event_tap_loss_is_transient_and_restoration_returns_to_ready() {
        let mut c = AppController::ready_for_test();
        assert_eq!(
            c.handle(AppEvent::EventTapLost),
            vec![Effect::ScheduleErrorReset { delay_ms: 3_000 }]
        );
        assert_eq!(
            c.status(),
            &AppStatus::Error {
                message: "Глобальная клавиша недоступна",
                recoverable: true,
            }
        );
        assert!(c.handle(AppEvent::EventTapRestored).is_empty());
        assert_eq!(c.status(), &AppStatus::Ready);
    }

    #[test]
    fn capture_limit_finishes_immediately() {
        let mut c = AppController::recording_for_test();
        assert_eq!(
            c.handle(AppEvent::CaptureLimitReached),
            vec![Effect::FinishCaptureAfter { delay_ms: 0 }]
        );
        assert_eq!(c.status(), &AppStatus::Recognizing);
    }

    #[test]
    fn audio_ready_some_starts_recognition() {
        let mut c = AppController::recognizing_for_test();
        assert_eq!(
            c.handle(AppEvent::AudioReady(Some(vec![0.25, -0.25]))),
            vec![Effect::Recognize(vec![0.25, -0.25])]
        );
    }
}
