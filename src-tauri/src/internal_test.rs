//! Only compiled into explicit internal-test builds; never shipped.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tauri::Manager;
#[cfg(not(windows))]
use tauri_plugin_autostart::ManagerExt;

static STATUS_READS: AtomicU64 = AtomicU64::new(0);
pub fn record_status() {
    STATUS_READS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(windows))]
pub fn data_root() -> anyhow::Result<PathBuf> {
    let root = PathBuf::from(std::env::var("GBF_INTERNAL_TEST_DATA")?);
    anyhow::ensure!(root.is_absolute(), "Test root must be absolute");
    let parent = root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing test parent"))?;
    anyhow::ensure!(
        parent
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("gbf-macos-")),
        "Test root must belong to the runner"
    );
    anyhow::ensure!(
        !parent.symlink_metadata()?.file_type().is_symlink(),
        "Linked test root"
    );
    Ok(root)
}

pub fn script() -> anyhow::Result<String> {
    match std::env::var("GBF_INTERNAL_TEST_SCRIPT") {
        Ok(path) => Ok(std::fs::read_to_string(path)?),
        Err(_) => Ok(String::new()),
    }
}

#[tauri::command]
pub async fn internal_test_control(
    app: tauri::AppHandle,
    action: String,
    result: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    match action.as_str() {
        "report" => {
            let path = std::env::var("GBF_INTERNAL_TEST_REPORT").map_err(|e| e.to_string())?;
            let value = result.ok_or("Missing result")?;
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        "hide" | "show" | "tray-start" | "tray-stop" => {
            let target = app.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move || {
                let result = match action.as_str() {
                    "hide" => crate::lifecycle::hide(&target).map_err(|e| e.to_string()),
                    "show" => {
                        crate::lifecycle::show(&target);
                        Ok(())
                    }
                    "tray-start" => {
                        crate::tray_control::action(&target, true);
                        Ok(())
                    }
                    _ => {
                        crate::tray_control::action(&target, false);
                        Ok(())
                    }
                };
                let _ = tx.send(result);
            })
            .map_err(|e| e.to_string())?;
            rx.await.map_err(|e| e.to_string())??;
        }
        "autostart-on" | "autostart-off" => {
            let enabled = action == "autostart-on";
            #[cfg(not(windows))]
            if enabled {
                app.autolaunch().enable()
            } else {
                app.autolaunch().disable()
            }
            .map_err(|e| e.to_string())?;
            #[cfg(windows)]
            crate::autostart_registration()
                .set(
                    enabled,
                    &std::env::current_exe().map_err(|e| e.to_string())?,
                    &[],
                )
                .map_err(|e| e.to_string())?;
        }
        "audit-hold" | "audit-release" | "audit-fixture" => {
            let core = app.state::<std::sync::Arc<gbf_core::runtime::Runtime>>();
            let marker = core.root.join(".internal-audit-hold");
            match action.as_str() {
                "audit-hold" => std::fs::write(marker, b"held").map_err(|e| e.to_string())?,
                "audit-release" => match std::fs::remove_file(marker) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.to_string()),
                },
                _ => {
                    let state = core.control_state();
                    if state.running || state.maintenance {
                        return Err("Fixture creation requires an idle cache".into());
                    }
                    let cache = core.root.join("cache/ja");
                    std::fs::write(cache.join(format!("{}.body", "0".repeat(64))), b"orphan")
                        .map_err(|e| e.to_string())?;
                    std::fs::write(
                        cache.join(format!("{}.pending", "1".repeat(64))),
                        b"partial",
                    )
                    .map_err(|e| e.to_string())?;
                    std::fs::write(cache.join("audit-notes.txt"), b"keep")
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        "snapshot" => {}
        _ => return Err("Unknown test action".into()),
    }
    let window = app
        .get_webview_window("main")
        .ok_or("Missing test window")?;
    #[cfg(not(windows))]
    let autostart = app.autolaunch().is_enabled().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let autostart = crate::autostart_registration()
        .snapshot()
        .map_err(|e| e.to_string())?
        .enabled();
    let root = app
        .state::<std::sync::Arc<gbf_core::runtime::Runtime>>()
        .root
        .clone();
    let cache = root.join("cache/ja");
    Ok(serde_json::json!({
        "statusReads": STATUS_READS.load(Ordering::Relaxed),
        "innerSize": window.inner_size().ok(),
        "outerSize": window.outer_size().ok(),
        "scaleFactor": window.scale_factor().ok(),
        "visible": window.is_visible().map_err(|e| e.to_string())?,
        "autostart": autostart,
        "auditFixture": {
            "orphanExists": cache.join(format!("{}.body", "0".repeat(64))).exists(),
            "pendingExists": cache.join(format!("{}.pending", "1".repeat(64))).exists(),
            "notesPreserved": std::fs::read(cache.join("audit-notes.txt")).is_ok_and(|data| data == b"keep"),
        },
        "fixture": std::env::var("GBF_INTERNAL_TEST_DATA").ok().and_then(|p| std::fs::read(PathBuf::from(p).join("fixture.json")).ok()).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()),
    }))
}
