//! Finder launches have no visible stderr. Keep startup failures native and actionable.
pub fn error() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSAlert;
    use objc2_foundation::NSString;
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let alert = NSAlert::new(main);
    let language = crate::appearance::system_language();
    alert.setMessageText(&NSString::from_str(&crate::appearance::text(
        language,
        "startupErrorTitle",
    )));
    alert.setInformativeText(&NSString::from_str(&crate::appearance::text(
        language,
        "startupErrorMessage",
    )));
    #[cfg(feature = "internal-test")]
    if std::env::var_os("GBF_INTERNAL_TEST_STARTUP_ERROR").is_some() {
        use objc2_app_kit::{NSApplication, NSModalPanelRunLoopMode};
        use objc2_foundation::{NSRunLoop, NSTimer};
        // Only our own startup alert is dismissed. Never handles OS authorization.
        let block = block2::RcBlock::new(|_: std::ptr::NonNull<NSTimer>| {
            let main = MainThreadMarker::new().expect("modal timer runs on main thread");
            let app = NSApplication::sharedApplication(main);
            let visible = app.modalWindow().is_some_and(|window| window.isVisible());
            if let Ok(path) = std::env::var("GBF_INTERNAL_TEST_REPORT") {
                let _ = std::fs::write(
                    path,
                    serde_json::json!({"passed": visible, "startupAlert": true}).to_string(),
                );
            }
            app.stopModal();
        });
        // No thread-bound objects are captured; scheduling is on the main modal loop.
        unsafe {
            let timer = NSTimer::timerWithTimeInterval_repeats_block(0.3, false, &block);
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode);
        }
    }
    alert.runModal();
}
