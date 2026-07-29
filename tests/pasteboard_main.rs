use objc2_foundation::MainThreadMarker;

fn main() {
    assert!(
        MainThreadMarker::new().is_some(),
        "pasteboard system tests must run on the macOS main thread"
    );
    ptt2me::inserter::run_pasteboard_main_thread_tests()
        .expect("pasteboard system tests must pass");
}
