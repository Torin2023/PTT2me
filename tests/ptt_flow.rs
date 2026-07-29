use ptt2me::state::{AppController, AppEvent, AppStatus, Effect, PermissionSnapshot};

#[test]
fn push_to_talk_flow_reaches_ready_after_paste() {
    let mut controller = AppController::new();
    assert!(controller
        .handle(AppEvent::PermissionsChanged(PermissionSnapshot::all()))
        .is_empty());
    assert!(controller.handle(AppEvent::ModelLoaded(Ok(()))).is_empty());
    assert_eq!(controller.status(), &AppStatus::Ready);

    assert_eq!(
        controller.handle(AppEvent::TriggerPressed),
        vec![Effect::StartCapture]
    );
    assert_eq!(
        controller.handle(AppEvent::TriggerReleased { short: false }),
        vec![Effect::FinishCaptureAfter { delay_ms: 180 }]
    );

    let samples = vec![0.1; 16_000];
    assert_eq!(
        controller.handle(AppEvent::AudioReady(Some(samples.clone()))),
        vec![Effect::Recognize(samples)]
    );
    assert_eq!(
        controller.handle(AppEvent::RecognitionFinished(Ok(" привет ".into()))),
        vec![Effect::InsertText("привет".into())]
    );
    assert!(controller
        .handle(AppEvent::PasteFinished(Ok(())))
        .is_empty());
    assert_eq!(controller.status(), &AppStatus::Ready);
}
