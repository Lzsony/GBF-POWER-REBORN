//! Native event smoke test. Isolated identity/data; no system proxy or CA changes.
#[cfg(any(target_os = "macos", windows))]
#[path = "../src/appearance.rs"]
mod appearance;
#[cfg(any(target_os = "macos", windows))]
#[path = "../src/lifecycle.rs"]
mod lifecycle;
#[cfg(any(target_os = "macos", windows))]
#[path = "../src/tray_control.rs"]
mod tray_control;
#[cfg(any(target_os = "macos", windows))]
#[path = "../src/window_layout.rs"]
mod window_layout;

#[cfg(any(target_os = "macos", windows))]
static RESULT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

#[cfg(any(target_os = "macos", windows))]
fn finish(app: &tauri::AppHandle, code: i32) {
    use std::sync::atomic::Ordering;
    use tauri::Manager;
    #[cfg(windows)]
    {
        let _ = gbf_core::windows_autostart::Registration::new("GBF Native Lifecycle Test")
            .remove_test_registration();
    }
    RESULT.store(code, Ordering::Relaxed);
    let target = app.clone();
    if app
        .run_on_main_thread(move || {
            // Remove the test's Dock presence before stopping AppKit. Destroy bypasses
            // the close-to-tray handler, which is intentionally active during smoke().
            #[cfg(target_os = "macos")]
            let _ = target.set_activation_policy(tauri::ActivationPolicy::Accessory);
            if let Some(window) = target.get_webview_window("main") {
                let _ = window.destroy();
            }
            drop(target.remove_tray_by_id(lifecycle::TRAY_ID));
            // Allow AppKit and Dock to observe the visibility change before exit.
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                target.exit(code);
            });
        })
        .is_err()
    {
        std::process::exit(1);
    }
}

#[cfg(any(target_os = "macos", windows))]
fn main() {
    use tauri::Manager;
    // This executable is a test harness, never part of the shipping app. A stalled
    // event loop must not leave an unattended GUI process running indefinitely.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(45));
        eprintln!("FAIL: native test watchdog forced exit");
        std::process::exit(1);
    });
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = "cc.lzsony.gbf-power-reborn.native-lifecycle-test".into();
    context.config_mut().product_name = Some("GBF Native Lifecycle Test".into());
    let builder = tauri::Builder::default();
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _, _| {
        lifecycle::show(app)
    }));
    builder
        .setup(|app| {
            if std::env::args().any(|arg| arg == "--second-instance") {
                return Err("single-instance plugin failed to intercept the second launch".into());
            }
            let data = tempfile::tempdir()?;
            let core = std::sync::Arc::new(gbf_core::runtime::Runtime::new(data.path().join("core"))?);
            app.manage(core);
            tray_control::install(app.handle());
            let builder = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External("about:blank".parse()?));
            #[cfg(target_os = "macos")]
            let builder = builder.incognito(true);
            let window = builder
                .data_directory(data.path().to_path_buf())
                .title("GBF native lifecycle test").inner_size(400.0, 520.0).decorations(true).visible(false).build()?;
            app.manage(data);
            window_layout::fit_initial(&window)?;
            window.show()?;
            tauri::tray::TrayIconBuilder::with_id(lifecycle::TRAY_ID)
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?).build(app)?;
            lifecycle::install(app.handle())?;
            let _ = appearance::system_language();
            let app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Await a separate worker so a panic is observed by the supervisor.
                let exercise = std::env::args().nth(1).unwrap_or_default();
                let timeout = if exercise == "--verify-timeout-cleanup" {
                    std::time::Duration::from_millis(100)
                } else { std::time::Duration::from_secs(30) };
                let worker_app = app.clone();
                let mut worker = tokio::spawn(async move {
                    if exercise == "--verify-panic-cleanup" { panic!("intentional cleanup verification"); }
                    if exercise == "--verify-timeout-cleanup" { std::future::pending::<()>().await; }
                    smoke(worker_app).await
                });
                let result = match tokio::time::timeout(timeout, &mut worker).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) => Err(format!("native test worker failed: {error}")),
                    Err(_) => { worker.abort(); let _ = worker.await; Err("native test timed out".into()) }
                };
                match result {
                    Ok(()) => {
                        println!("PASS: minimize/close -> hidden; restore -> visible; proxy connection preserved; stop released listener");
                        finish(&app, 0);
                    }
                    Err(error) => { eprintln!("FAIL: {error}"); finish(&app, 1); }
                }
            });
            Ok(())
        })
        .on_menu_event(|app, event| {
            if event.id.as_ref() == "quit" { finish(app, 1); }
            if event.id.as_ref() == "start" { tray_control::action(app, true); }
            if event.id.as_ref() == "stop" { tray_control::action(app, false); }
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                lifecycle::close_requested(window, api);
            }
        })
        .build(context).expect("native test app").run_return(|_, event| {
            // Destroying the last window must not race the explicit cleanup exit.
            if let tauri::RunEvent::ExitRequested { api, code: None, .. } = event {
                api.prevent_exit();
            }
        });
    std::process::exit(RESULT.load(std::sync::atomic::Ordering::Relaxed));
}

#[cfg(any(target_os = "macos", windows))]
async fn ui<T: Send + 'static>(
    app: &tauri::AppHandle,
    work: impl FnOnce(&tauri::AppHandle) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let target = app.clone();
    app.run_on_main_thread(move || {
        let _ = tx.send(work(&target));
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|e| e.to_string())?
}

#[cfg(any(target_os = "macos", windows))]
async fn smoke(app: tauri::AppHandle) -> Result<(), String> {
    use gbf_core::preferences::{Language, Preferences, Theme};
    use gbf_core::{
        cache::Cache,
        config::{Mode, Settings},
        metrics::Metrics,
        proxy::{serve, ContextState},
    };
    use std::{
        sync::{atomic::Ordering, Arc},
        time::Duration,
    };
    use tauri::{Listener, Manager};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    ui(&app, |app| {
        let window = app.get_webview_window("main").unwrap();
        let monitor = window.current_monitor().unwrap().unwrap();
        if window.is_resizable().map_err(|e|e.to_string())? || window.is_maximizable().map_err(|e|e.to_string())? || window.is_fullscreen().map_err(|e|e.to_string())? {
            return Err("main window must remain fixed and non-maximizable".into());
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SendMessageW, GWL_STYLE, WS_THICKFRAME, WS_MAXIMIZEBOX, WM_NCLBUTTONDBLCLK, HTCAPTION};
            let hwnd = window.hwnd().map_err(|e|e.to_string())?.0 as windows_sys::Win32::Foundation::HWND;
            let before = window.outer_size().map_err(|e|e.to_string())?;
            unsafe {
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
                if style & (WS_THICKFRAME | WS_MAXIMIZEBOX) != 0 { return Err("resize or maximize style remains enabled".into()); }
                SendMessageW(hwnd, WM_NCLBUTTONDBLCLK, HTCAPTION as usize, 0);
            }
            if window.is_maximized().map_err(|e|e.to_string())? || before != window.outer_size().map_err(|e|e.to_string())? {
                return Err("titlebar double click changed fixed window".into());
            }
            println!("PASS: Windows resize/maximize styles absent; titlebar double click retains dimensions");
        }
        println!("PASS: native window is fixed, non-maximizable and not fullscreen");
        let work = monitor.work_area();
        let position = window.outer_position().unwrap();
        let outer = window.outer_size().unwrap();
        #[cfg(not(target_os = "macos"))]
        let inner = window
            .inner_size()
            .unwrap()
            .to_logical::<f64>(monitor.scale_factor());
        #[cfg(target_os = "macos")]
        let inner = {
            let native = unsafe { &*(window.ns_window().unwrap() as *const objc2_app_kit::NSWindow) };
            let content = native.contentLayoutRect();
            tauri::LogicalSize::new(content.size.width, content.size.height)
        };
        if position.x < work.position.x
            || position.y < work.position.y
            || i64::from(position.x) + i64::from(outer.width)
                > i64::from(work.position.x) + i64::from(work.size.width)
            || i64::from(position.y) + i64::from(outer.height)
                > i64::from(work.position.y) + i64::from(work.size.height)
            || inner.width > 401.0
            || inner.height > 521.0
        {
            return Err(format!("portrait window exceeds the work area: position={position:?}, outer={outer:?}, inner={inner:?}, work={work:?}"));
        }
        println!(
            "PASS: initial portrait window fits work area ({:.0} x {:.0} logical, native DPI scale {})",
            inner.width, inner.height, monitor.scale_factor()
        );
        Ok(())
    })
    .await?;
    let baseline = ui(&app, |app| {
        app.get_webview_window("main")
            .unwrap()
            .theme()
            .map_err(|e| e.to_string())
    })
    .await?;
    for theme in [Theme::Dark, Theme::Light, Theme::Auto] {
        ui(&app, move |app| {
            appearance::apply_theme(app, theme).map_err(|e| e.to_string())
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let actual = ui(&app, |app| {
            app.get_webview_window("main")
                .unwrap()
                .theme()
                .map_err(|e| e.to_string())
        })
        .await?;
        let expected = match theme {
            Theme::Dark => tauri::Theme::Dark,
            Theme::Light => tauri::Theme::Light,
            Theme::Auto => baseline,
        };
        if actual != expected {
            return Err(format!(
                "native theme mismatch: {theme:?} -> {actual:?}, expected {expected:?}"
            ));
        }
    }
    for (language, expected) in [
        (Language::Simplified, "显示主窗口"),
        (Language::Traditional, "顯示主視窗"),
    ] {
        ui(&app, move |app| {
            appearance::apply(
                app,
                Preferences {
                    theme: Theme::Auto,
                    language,
                },
            )
            .map_err(|e| e.to_string())?;
            let menu = appearance::tray_menu(app, language).map_err(|e| e.to_string())?;
            let item = menu.get("show").unwrap();
            if item.as_menuitem().unwrap().text().unwrap() != expected {
                return Err("native menu translation mismatch".into());
            }
            Ok(())
        })
        .await?;
    }
    println!("PASS: native light/dark/auto and two-language menus");
    tray_smoke(&app).await?;
    #[cfg(windows)]
    ui(&app, |app| {
        let _ = app;
        let launch = gbf_core::windows_autostart::Registration::new("GBF Native Lifecycle Test");
        launch
            .set(
                true,
                &std::env::current_exe().map_err(|e| e.to_string())?,
                &[],
            )
            .map_err(|e| e.to_string())?;
        if !launch.snapshot().map_err(|e| e.to_string())?.enabled() {
            return Err("test login startup was not enabled".into());
        }
        launch
            .remove_test_registration()
            .map_err(|e| e.to_string())?;
        if launch.snapshot().map_err(|e| e.to_string())?.enabled() {
            return Err("test login startup was not removed".into());
        }
        println!("PASS: isolated login startup registration and removal");
        Ok(())
    })
    .await?;
    // Exercise the real proxy with an opaque API tunnel backed by a local echo upstream.
    let source = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let source_port = source.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
        let (mut stream, _) = source.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(stream.read_u8().await.unwrap());
        }
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        let (mut read, mut write) = stream.into_split();
        let _ = tokio::io::copy(&mut read, &mut write).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let settings = Settings {
        mode: Mode::Http,
        upstream_port: source_port,
        listen_port: port,
        ..Default::default()
    };
    let state = Arc::new(
        ContextState::new(
            settings,
            String::new(),
            Arc::new(Cache::open(dir.path()).unwrap()),
            None,
            Arc::new(Metrics::default()),
        )
        .unwrap(),
    );
    let server = tokio::spawn(serve(listener, state.clone()));
    let mut browser = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    browser.write_all(b"CONNECT game.granbluefantasy.jp:443 HTTP/1.1\r\nHost: game.granbluefantasy.jp:443\r\n\r\n").await.unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(browser.read_u8().await.unwrap());
    }
    if !header.starts_with(b"HTTP/1.1 200") {
        return Err("proxy tunnel failed".into());
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let visibility_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = visibility_events.clone();
    let event_id = app.listen("main-visibility", move |event| {
        if let Ok(value) = serde_json::from_str::<bool>(event.payload()) {
            observed.lock().unwrap().push(value);
        }
    });
    for minimize in [true, false] {
        ui(&app, move |app| {
            let window = app.get_webview_window("main").unwrap();
            if minimize {
                window.minimize()
            } else {
                window.close()
            }
            .map_err(|e| e.to_string())
        })
        .await?;
        let mut hidden = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            hidden = ui(&app, |app| {
                let window = app.get_webview_window("main").unwrap();
                Ok(!lifecycle::main_window_visible(app.clone())
                    && !window.is_visible().unwrap()
                    && !window.is_minimized().unwrap()
                    && native_visibility(app, false))
            })
            .await?;
            if hidden {
                break;
            }
        }
        if !hidden {
            let diagnostic = ui(&app, |app| {
                let w = app.get_webview_window("main").unwrap();
                Ok(format!(
                    "visible={:?} minimized={:?} native_hidden={} tray={}",
                    w.is_visible(),
                    w.is_minimized(),
                    native_visibility(app, false),
                    app.tray_by_id(lifecycle::TRAY_ID).is_some()
                ))
            })
            .await?;
            eprintln!("{diagnostic}");
            return Err(format!(
                "native {} did not hide to tray",
                if minimize { "minimize" } else { "close" }
            ));
        }
        browser.write_all(b"alive").await.unwrap();
        let mut reply = [0; 5];
        browser.read_exact(&mut reply).await.unwrap();
        if &reply != b"alive" || state.metrics.connections.load(Ordering::Relaxed) != 1 {
            return Err("proxy connection changed while hidden".into());
        }
        #[cfg(windows)]
        if !minimize {
            let status = tokio::process::Command::new(std::env::current_exe().unwrap())
                .arg("--second-instance")
                .creation_flags(0x08000000)
                .status()
                .await
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err("second instance did not exit successfully".into());
            }
            println!("PASS: second instance restored the existing window");
        } else {
            ui(&app, |app| {
                lifecycle::show(app);
                Ok(())
            })
            .await?;
        }
        #[cfg(not(windows))]
        ui(&app, |app| {
            lifecycle::show(app);
            Ok(())
        })
        .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let visible = ui(&app, |app| {
            let window = app.get_webview_window("main").unwrap();
            Ok(lifecycle::main_window_visible(app.clone())
                && window.is_visible().unwrap()
                && !window.is_minimized().unwrap()
                && native_visibility(app, true))
        })
        .await?;
        if !visible {
            return Err("restore did not show regular window".into());
        }
    }
    app.unlisten(event_id);
    if *visibility_events.lock().unwrap() != [false, true, false, true] {
        return Err("native visibility event sequence mismatch".into());
    }
    println!("PASS: visibility query and native hide/show events agree");
    state.cancel.cancel();
    server.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
    if TcpStream::connect(("127.0.0.1", port)).await.is_ok()
        || state.metrics.connections.load(Ordering::Relaxed) != 0
    {
        return Err("proxy did not release listener/connections".into());
    }
    echo.abort();
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_visibility(_app: &tauri::AppHandle, visible: bool) -> bool {
    let ns =
        objc2_app_kit::NSApplication::sharedApplication(objc2::MainThreadMarker::new().unwrap());
    ns.activationPolicy()
        == if visible {
            objc2_app_kit::NSApplicationActivationPolicy::Regular
        } else {
            objc2_app_kit::NSApplicationActivationPolicy::Accessory
        }
}

#[cfg(windows)]
fn native_visibility(app: &tauri::AppHandle, visible: bool) -> bool {
    use tauri::Manager;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    };
    let window = app.get_webview_window("main").unwrap();
    let hwnd = window.hwnd().unwrap().0 as windows_sys::Win32::Foundation::HWND;
    unsafe {
        (IsWindowVisible(hwnd) != 0) == visible
            && (!visible || GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW == 0)
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn main() {
    eprintln!("This smoke test requires macOS or Windows.");
}

#[cfg(any(target_os = "macos", windows))]
async fn tray_smoke(app: &tauri::AppHandle) -> Result<(), String> {
    use gbf_core::{
        preferences::{Language, Preferences, Theme},
        runtime::Runtime,
    };
    use std::{sync::Arc, time::Duration};
    use tauri::Manager;
    let core = app.state::<Arc<Runtime>>().inner().clone();
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let port = occupied.local_addr().unwrap().port();
    let mut settings = core.settings.read().unwrap().clone();
    settings.mode = gbf_core::config::Mode::Direct;
    settings.https_cache = false;
    settings.listen_port = port;
    core.save(settings, None).await.map_err(|e| e.to_string())?;
    ui(app, |app| {
        lifecycle::hide(app).map_err(|e| e.to_string())?;
        tray_control::action(app, true);
        tray_control::action(app, true);
        if !tray_control::snapshot(app).state.busy {
            return Err("tray did not synchronously disable repeated click".into());
        }
        Ok(())
    })
    .await?;
    for _ in 0..100 {
        if !tray_control::snapshot(app).state.busy {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let failed = tray_control::snapshot(app);
    if failed.state.busy
        || failed.state.running
        || failed.notice.map(|n| n.code) != Some(gbf_core::error::ErrorCode::ProxyBindFailed)
    {
        return Err(format!(
            "tray bind failure missing durable notice or final state: {:?}, {:?}",
            failed.state,
            failed.notice.map(|n| n.code)
        ));
    }
    ui(app, |app| {
        if lifecycle::main_window_visible(app.clone()) {
            Ok(())
        } else {
            Err("tray failure did not show window".into())
        }
    })
    .await?;
    let notice_id = failed.notice.unwrap().id;
    tray_control::acknowledge(app, notice_id + 1);
    if tray_control::snapshot(app).notice.is_none() {
        return Err("stale acknowledgement cleared a newer error".into());
    }
    tray_control::acknowledge(app, notice_id);
    if tray_control::snapshot(app).notice.is_some() {
        return Err("handled tray error remained pending".into());
    }
    drop(occupied);
    ui(app, |app| {
        lifecycle::hide(app).map_err(|e| e.to_string())?;
        tray_control::action(app, true);
        Ok(())
    })
    .await?;
    for _ in 0..100 {
        let state = tray_control::snapshot(app).state;
        if state.running && !state.busy {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    if !core.control_state().running {
        return Err("tray start did not start proxy".into());
    }
    for language in [Language::Simplified, Language::Traditional] {
        core.save_preferences(Preferences {
            language,
            theme: Theme::Auto,
        })
        .await
        .map_err(|e| e.to_string())?;
        ui(app, move |app| {
            appearance::apply(
                app,
                Preferences {
                    language,
                    theme: Theme::Auto,
                },
            )
            .map_err(|e| e.to_string())?;
            let menu = appearance::tray_menu(app, language).map_err(|e| e.to_string())?;
            let item = menu.get("stop").ok_or("running tray lost stop item")?;
            if item.as_menuitem().unwrap().text().unwrap() != appearance::text(language, "stop")
                || !item.as_menuitem().unwrap().is_enabled().unwrap()
            {
                return Err("tray state lost during language change".into());
            }
            if lifecycle::main_window_visible(app.clone()) {
                return Err("successful hidden tray operation opened window".into());
            }
            Ok(())
        })
        .await?;
    }
    ui(app, |app| {
        tray_control::action(app, false);
        tray_control::action(app, false);
        Ok(())
    })
    .await?;
    for _ in 0..100 {
        let state = tray_control::snapshot(app).state;
        if !state.running && !state.busy {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    if core.control_state().running || tray_control::snapshot(app).state.busy {
        return Err("tray stop did not settle".into());
    }
    let _released = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|_| "tray stop left listener bound")?;
    ui(app, |app| {
        lifecycle::show(app);
        Ok(())
    })
    .await?;
    println!("PASS: native tray hidden start/stop, double-click dedup, two-language running state, bind failure recovery and listener cleanup");
    Ok(())
}
