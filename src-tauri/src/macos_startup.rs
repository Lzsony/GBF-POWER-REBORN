//! Finder launches have no visible stderr. Keep startup failures native and actionable.
pub fn error() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSAlert;
    use objc2_foundation::NSString;
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let alert = NSAlert::new(main);
    let simplified =
        crate::appearance::system_language() == gbf_core::preferences::Language::Simplified;
    alert.setMessageText(&NSString::from_str(if simplified {
        "GBF POWER REBORN 无法启动"
    } else {
        "GBF POWER REBORN 無法啟動"
    }));
    alert.setInformativeText(&NSString::from_str(if simplified {
        "无法读取配置或初始化应用程序。此版本要求 schemaVersion 1；请检查应用数据的格式与访问权限。现有数据不会自动重置。"
    } else {
        "無法讀取設定或初始化應用程式。此版本要求 schemaVersion 1；請檢查應用資料的格式與存取權限。現有資料不會自動重設。"
    }));
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
