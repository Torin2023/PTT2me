use std::collections::BTreeSet;

use crate::constants::{ERROR_VISIBLE_MS, MIN_HOLD_MS, RELEASE_GRACE_MS};

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
    Starting,
    PermissionBlocked(PermissionKind),
    Ready,
    Recording,
    Recognizing,
    Error {
        message: &'static str,
        recoverable: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    ModelLoaded(Result<(), String>),
    PermissionsChanged(PermissionSnapshot),
    FnPressed,
    FnReleased { held_ms: u64 },
    CaptureLimitReached,
    CaptureFailed,
    AudioReady(Option<Vec<f32>>),
    RecognitionFinished(Result<String, String>),
    PasteFinished(Result<(), String>),
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
            status: AppStatus::Starting,
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
            AppEvent::ModelLoaded(Ok(())) => {
                self.model_ready = true;
                self.enter_idle_state()
            }
            AppEvent::ModelLoaded(Err(_)) => {
                self.model_ready = false;
                self.status = AppStatus::Error {
                    message: "Не удалось загрузить модель",
                    recoverable: false,
                };
                Vec::new()
            }
            AppEvent::PermissionsChanged(permissions) => {
                self.permissions = permissions;
                self.forget_granted_permission_panes();
                if self.model_ready {
                    self.enter_idle_state()
                } else {
                    Vec::new()
                }
            }
            AppEvent::FnPressed if self.status == AppStatus::Ready => {
                self.status = AppStatus::Recording;
                vec![Effect::StartCapture]
            }
            AppEvent::FnReleased { held_ms } if self.status == AppStatus::Recording => {
                if held_ms < MIN_HOLD_MS {
                    self.status = AppStatus::Ready;
                    vec![Effect::AbortCapture]
                } else {
                    self.status = AppStatus::Recognizing;
                    vec![Effect::FinishCaptureAfter {
                        delay_ms: RELEASE_GRACE_MS,
                    }]
                }
            }
            AppEvent::CaptureLimitReached if self.status == AppStatus::Recording => {
                self.status = AppStatus::Recognizing;
                vec![Effect::FinishCaptureAfter { delay_ms: 0 }]
            }
            AppEvent::CaptureFailed => self.show_recoverable_error("Не удалось записать звук"),
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
            AppEvent::EventTapLost => self.show_recoverable_error("Глобальная клавиша недоступна"),
            AppEvent::EventTapRestored => self.enter_idle_state(),
            _ => Vec::new(),
        }
    }

    fn enter_idle_state(&mut self) -> Vec<Effect> {
        if !self.model_ready {
            self.status = AppStatus::Starting;
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
    fn ready_press_starts_capture() {
        let mut c = AppController::ready_for_test();
        assert_eq!(c.handle(AppEvent::FnPressed), vec![Effect::StartCapture]);
        assert_eq!(c.status(), &AppStatus::Recording);
    }

    #[test]
    fn short_release_aborts_without_recognition() {
        let mut c = AppController::recording_for_test();
        assert_eq!(
            c.handle(AppEvent::FnReleased { held_ms: 249 }),
            vec![Effect::AbortCapture]
        );
        assert_eq!(c.status(), &AppStatus::Ready);
    }

    #[test]
    fn valid_release_waits_for_release_grace() {
        let mut c = AppController::recording_for_test();
        assert_eq!(
            c.handle(AppEvent::FnReleased { held_ms: 250 }),
            vec![Effect::FinishCaptureAfter { delay_ms: 180 }]
        );
        assert_eq!(c.status(), &AppStatus::Recognizing);
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
        assert!(c.handle(AppEvent::FnPressed).is_empty());
    }

    #[test]
    fn model_load_failure_is_persistent() {
        let mut c = AppController::new();
        assert!(c
            .handle(AppEvent::ModelLoaded(Err("missing model".into())))
            .is_empty());
        assert_eq!(
            c.status(),
            &AppStatus::Error {
                message: "Не удалось загрузить модель",
                recoverable: false,
            }
        );
        assert!(c.handle(AppEvent::ErrorTimerFired).is_empty());
        assert_eq!(
            c.status(),
            &AppStatus::Error {
                message: "Не удалось загрузить модель",
                recoverable: false,
            }
        );
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
