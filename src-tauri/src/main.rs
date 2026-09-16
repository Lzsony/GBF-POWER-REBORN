#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appearance;
#[cfg(feature = "internal-test")]
mod internal_test;
mod lifecycle;
mod logging;
#[cfg(target_os = "macos")]
mod macos_startup;
mod sites;
mod tray_control;
mod window_layout;
#[cfg(windows)]
mod windows_runtime;
use gbf_core::{
    error::{CommandError, ErrorCode},
    preferences::Preferences,
};
use lifecycle::show;

use gbf_core::{
    connection::{apply_input, display_url, ConnectionInput},
    runtime::{Runtime, Status},
};
use std::sync::Arc;
use tauri::{Manager, State};
#[cfg(not(windows))]
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

#[cfg(windows)]
fn autostart_registration() -> gbf_core::windows_autostart::Registration {
    gbf_core::windows_autostart::Registration::new(if cfg!(feature = "internal-test") {
        "GBF Internal Test"
    } else {
        "cc.lzsony.gbf-power-reborn"
    })
}

type Core<'a> = State<'a, Arc<Runtime>>;
type CommandResult<T> = Result<T, CommandError>;
fn error(e: impl Into<anyhow::Error>) -> CommandError {
    CommandError::from_error(e, ErrorCode::InternalFailure)
}
fn categorized(e: impl Into<anyhow::Error>, code: ErrorCode) -> CommandError {
    CommandError::from_error(e, code)
}

#[tauri::command]
async fn get_status(core: Core<'_>) -> CommandResult<Status> {
    #[cfg(feature = "internal-test")]
    internal_test::record_status();
    core.status().await.map_err(error)
}
#[tauri::command]
fn get_native_control(app: tauri::AppHandle) -> tray_control::Snapshot {
    tray_control::snapshot(&app)
}
#[tauri::command]
fn acknowledge_native_notice(app: tauri::AppHandle, id: u64) {
    tray_control::acknowledge(&app, id);
}
#[tauri::command]
async fn start_proxy(core: Core<'_>) -> CommandResult<()> {
    core.start().await.map_err(error)
}
#[tauri::command]
async fn stop_proxy(core: Core<'_>) -> CommandResult<()> {
    core.stop().await.map_err(error)
}
#[tauri::command]
async fn save_settings(
    app: tauri::AppHandle,
    core: Core<'_>,
    input: ConnectionInput,
    gate: tauri::State<'_, tokio::sync::Mutex<()>>,
) -> CommandResult<()> {
    let _gate = gate.lock().await;
    let current = core.settings.read().unwrap().clone();
    let (settings, password) = apply_input(&current, &input).map_err(error)?;
    #[cfg(windows)]
    {
        let _ = app;
        let mut settings = settings;
        let launch = autostart_registration();
        let previous = launch
            .snapshot()
            .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
        if settings.autostart != current.autostart {
            launch
                .set(
                    settings.autostart,
                    &std::env::current_exe().map_err(error)?,
                    &[],
                )
                .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
        } else {
            // A concurrent Task Manager disable is not an explicit UI enable request.
            settings.autostart = previous.enabled();
        }
        if let Err(e) = core.save(settings, password).await {
            launch
                .restore(&previous)
                .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
            return Err(error(e));
        }
    }
    #[cfg(not(windows))]
    {
        let previous = app
            .autolaunch()
            .is_enabled()
            .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
        if previous != settings.autostart {
            if settings.autostart {
                app.autolaunch()
                    .enable()
                    .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
            } else {
                app.autolaunch()
                    .disable()
                    .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
            }
        }
        if let Err(e) = core.save(settings, password).await {
            if previous {
                app.autolaunch()
                    .enable()
                    .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
            } else {
                app.autolaunch()
                    .disable()
                    .map_err(|e| categorized(e, ErrorCode::AutostartFailed))?;
            }
            return Err(error(e));
        }
    }
    Ok(())
}
#[tauri::command]
async fn test_proxy_url(
    core: Core<'_>,
    test_id: String,
    input: ConnectionInput,
) -> CommandResult<serde_json::Value> {
    let (settings, _) = apply_input(&core.settings.read().unwrap(), &input).map_err(error)?;
    let result = core.inner().test_proxy_port(test_id, settings).await;
    Ok(
        serde_json::json!({"testId":result.test_id,"connected":result.state=="success","state":result.state}),
    )
}
#[tauri::command]
async fn cancel_proxy_test(core: Core<'_>, test_id: String) -> CommandResult<()> {
    core.cancel_proxy_test(Some(&test_id)).await;
    Ok(())
}
#[tauri::command]
async fn test_connection(
    core: Core<'_>,
    input: ConnectionInput,
) -> CommandResult<gbf_core::runtime::ConnectionProbe> {
    let (settings, password) =
        apply_input(&core.settings.read().unwrap(), &input).map_err(error)?;
    core.probe(settings, password).await.map_err(error)
}
#[tauri::command]
async fn reveal_proxy_url(core: Core<'_>) -> CommandResult<String> {
    let settings = core.settings.read().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let password = if settings.username.is_empty() {
            String::new()
        } else {
            gbf_core::config::read_password().map_err(error)?
        };
        Ok(display_url(&settings, Some(&password)))
    })
    .await
    .map_err(error)?
}
#[tauri::command]
async fn clear_cache(core: Core<'_>) -> CommandResult<()> {
    core.clear_cache().await.map_err(error)
}
#[tauri::command]
async fn manage_certificate(
    core: Core<'_>,
    action: String,
) -> CommandResult<gbf_core::certificate::CertificateStatus> {
    core.manage_certificate(&action).await.map_err(error)
}
#[tauri::command]
async fn open_game_site(app: tauri::AppHandle, site: String) -> CommandResult<()> {
    let url = sites::url(&site).map_err(CommandError::from)?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| categorized(e, ErrorCode::BrowserOpenFailed))
}
#[tauri::command]
async fn open_local(app: tauri::AppHandle, core: Core<'_>, kind: String) -> CommandResult<()> {
    let path = match kind.as_str() {
        "data" => core.root.clone(),
        "certificate" => {
            let path = gbf_core::certificate::cert_path(&core.root);
            if !path.exists() {
                return Err(ErrorCode::CertificateMissing.into());
            }
            path
        }
        _ => return Err(ErrorCode::InvalidCommand.into()),
    };
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| categorized(e, ErrorCode::DirectoryOpenFailed))
}
#[tauri::command]
async fn save_preferences(
    app: tauri::AppHandle,
    core: Core<'_>,
    preferences: Preferences,
) -> CommandResult<()> {
    core.save_preferences(preferences).await.map_err(error)?;
    let target = app.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(appearance::apply(&target, preferences));
    })
    .map_err(|e| categorized(e, ErrorCode::NativeOperationFailed))?;
    rx.await
        .map_err(error)?
        .map_err(|e| categorized(e, ErrorCode::NativeOperationFailed))
}
#[tauri::command]
async fn quit_app(app: tauri::AppHandle, core: Core<'_>) -> CommandResult<()> {
    core.shutdown().await.map_err(error)?;
    app.exit(0);
    Ok(())
}
fn quit(app: &tauri::AppHandle) {
    let core = app.state::<Arc<Runtime>>().inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = core.shutdown().await;
        app.exit(0);
    });
}
fn main() {
    #[cfg(windows)]
    if let Err(error) = windows_runtime::prepare() {
        windows_runtime::startup_error(&error);
        std::process::exit(1);
    }
    #[allow(unused_mut)]
    let mut context = tauri::generate_context!();
    #[cfg(feature = "internal-test")]
    {
        let id = std::env::var("GBF_INTERNAL_TEST_ID")
            .expect("GBF-INTERNAL-TEST-BUILD-DO-NOT-DISTRIBUTE");
        assert!(id.starts_with("cc.lzsony.gbf-power-reborn.test-"));
        context.config_mut().identifier = id;
        context.config_mut().product_name = Some("GBF Internal Test".into());
    }
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(tokio::sync::Mutex::new(()))
        .plugin(tauri_plugin_opener::init());
    #[cfg(not(windows))]
    let builder = builder.plugin(
        tauri_plugin_autostart::Builder::new()
            .app_name(if cfg!(feature = "internal-test") {
                std::env::var("GBF_INTERNAL_TEST_ID").expect("isolated test identity")
            } else {
                "cc.lzsony.gbf-power-reborn".to_owned()
            })
            .build(),
    );
    let app = builder
        .invoke_handler(tauri::generate_handler![
            #[cfg(feature = "internal-test")]
            internal_test::internal_test_control,
            get_status,
            get_native_control,
            acknowledge_native_notice,
            lifecycle::main_window_visible,
            start_proxy,
            stop_proxy,
            save_settings,
            test_connection,
            test_proxy_url,
            cancel_proxy_test,
            reveal_proxy_url,
            save_preferences,
            clear_cache,
            manage_certificate,
            open_local,
            open_game_site,
            quit_app
        ])
        .setup(|app| {
            let result: Result<(), Box<dyn std::error::Error>> = (|| {
                #[cfg(all(windows, not(feature = "internal-test")))]
                let root = gbf_core::data_directory::windows_root(&app.path().local_data_dir()?)?;
                #[cfg(all(windows, feature = "internal-test"))]
                let root = {
                    let base =
                        std::path::PathBuf::from(std::env::var("GBF_INTERNAL_TEST_LOCALAPPDATA")?);
                    if !base.is_absolute() {
                        return Err("Internal test root must be absolute".into());
                    }
                    gbf_core::data_directory::windows_root(&base)?
                };
                #[cfg(all(not(windows), not(feature = "internal-test")))]
                let root = app.path().local_data_dir()?.join("GBF Power Reborn");
                #[cfg(all(not(windows), feature = "internal-test"))]
                let root = internal_test::data_root()?;
                let existing = root.join("config.json").exists();
                gbf_core::config_store::initialize(&root)?;
                let log = logging::writer(&root.join("logs"))?;
                let _ = tracing_subscriber::fmt()
                    .with_env_filter("gbf_core=info,gbf_power_reborn=info")
                    .event_format(logging::Readable)
                    .with_writer(log)
                    .try_init();
                #[cfg(windows)]
                let webview_data = gbf_core::data_directory::runtime_directory(&root)?;
                let core = Arc::new(Runtime::new(root)?);
                if !existing {
                    let mut settings = core.settings.write().unwrap();
                    settings.preferences.language = appearance::system_language();
                    settings.save(&core.root)?;
                }
                let preferences = core.settings.read().unwrap().preferences;
                #[cfg(windows)]
                let enabled = autostart_registration().reconcile(&std::env::current_exe()?, &[])?;
                #[cfg(windows)]
                {
                    core.settings.write().unwrap().autostart = enabled;
                }
                #[cfg(not(windows))]
                if let Ok(enabled) = app.autolaunch().is_enabled() {
                    core.settings.write().unwrap().autostart = enabled;
                }
                app.manage(core);
                tray_control::install(app.handle());
                let builder =
                    tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?;
                #[cfg(feature = "internal-test")]
                let builder = builder.initialization_script(internal_test::script()?);
                // WKWebView ignores data_directory on macOS. Internal runs must
                // not use the executable's shared persistent WebKit store.
                #[cfg(all(target_os = "macos", feature = "internal-test"))]
                let builder = builder.incognito(true);
                #[cfg(windows)]
                let builder = builder.data_directory(webview_data);
                let window = builder.visible(false).build()?;
                window_layout::fit_initial(&window)?;
                let menu = appearance::tray_menu(app.handle(), preferences.language)?;
                tauri::tray::TrayIconBuilder::with_id(lifecycle::TRAY_ID)
                    .icon(tauri::image::Image::from_bytes({
                        #[cfg(target_os = "macos")]
                        {
                            include_bytes!("../icons/tray.png")
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            include_bytes!("../icons/icon.png")
                        }
                    })?)
                    .icon_as_template(cfg!(target_os = "macos"))
                    .tooltip("GBF POWER REBORN")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .build(app)?;
                lifecycle::install(app.handle())?;
                appearance::apply(app.handle(), preferences)?;
                lifecycle::show(app.handle());
                if std::env::args().any(|arg| arg == "--resume-proxy") {
                    tray_control::action(app.handle(), true);
                }
                Ok(())
            })();
            // macOS setup is deferred until AppKit starts. Returning Err here
            // makes Tauri panic inside an Objective-C callback, before build's
            // error branch can show a dialog.
            #[cfg(target_os = "macos")]
            if let Err(error) = &result {
                macos_startup::error();
                eprintln!("{error}");
                std::process::exit(1);
            }
            #[cfg(windows)]
            if let Err(error) = &result {
                windows_runtime::startup_error(&error);
            }
            result
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show(app),
            "start" => tray_control::action(app, true),
            "stop" => tray_control::action(app, false),
            "quit" => quit(app),
            _ => (),
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                lifecycle::close_requested(window, api);
            }
        })
        .build(context);
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            #[cfg(target_os = "macos")]
            macos_startup::error();
            #[cfg(windows)]
            windows_runtime::startup_error(&error);
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested {
            api, code: None, ..
        } = event
        {
            api.prevent_exit();
            quit(app);
        }
    });
}
