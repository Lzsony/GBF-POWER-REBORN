use anyhow::{bail, Result};
use std::path::Path;
use windows_sys::Win32::{
    UI::Shell::ShellExecuteW,
    UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONERROR, MB_OK, MB_YESNO},
};

const DOWNLOAD: &str = "https://developer.microsoft.com/microsoft-edge/webview2/#download-section";
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn localized(key: &str) -> String {
    crate::appearance::text(crate::appearance::system_language(), key)
}

pub fn startup_error(error: &dyn std::fmt::Display) {
    self::error(&format!(
        "{}\n{}\n\n{error}",
        localized("startupErrorTitle"),
        localized("startupErrorMessage")
    ));
}

pub fn error(message: &str) {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide("GBF POWER REBORN").as_ptr(),
            MB_ICONERROR | MB_OK,
        );
    }
}

fn validate_fixed(path: &Path) -> Result<()> {
    for file in [
        "msedgewebview2.exe",
        "msedge.dll",
        "icudtl.dat",
        "resources.pak",
    ] {
        if !path.join(file).is_file() {
            bail!(localized("runtimeMissingFile").replace("{file}", file));
        }
    }
    if !path.join("Locales").is_dir() {
        bail!(localized("runtimeMissingLocales"));
    }
    Ok(())
}

pub fn prepare() -> Result<()> {
    let exe = std::env::current_exe()?;
    let parent = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing application directory"))?;
    let fixed = parent.join("WebView2Runtime");
    let expected_fixed = if parent.join("manifest.json").is_file() {
        let manifest = std::fs::read_to_string(parent.join("manifest.json"))?;
        let value: serde_json::Value =
            serde_json::from_str(manifest.trim_start_matches('\u{feff}'))?;
        value["package"] == "portable_webview2"
    } else {
        false
    };
    if expected_fixed || fixed.try_exists()? {
        validate_fixed(&fixed)?;
        std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", &fixed);
        tauri::webview_version()
            .map_err(|e| anyhow::anyhow!("{}：{e}", localized("runtimeUnavailable")))?;
    } else {
        // Do not let a stale process environment redirect the lightweight package.
        std::env::remove_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER");
        if tauri::webview_version().is_err() {
            let message = wide(&localized("runtimeInstallPrompt"));
            let answer = unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    wide("GBF POWER REBORN").as_ptr(),
                    MB_ICONERROR | MB_YESNO,
                )
            };
            if answer == IDYES {
                unsafe {
                    ShellExecuteW(
                        std::ptr::null_mut(),
                        wide("open").as_ptr(),
                        wide(DOWNLOAD).as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        1,
                    );
                }
            }
            // Already explained by the native prompt; do not show a second dialog.
            std::process::exit(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_or_incomplete_fixed_runtime_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        assert!(validate_fixed(&root.path().join("absent")).is_err());
        std::fs::write(root.path().join("msedgewebview2.exe"), b"fixture").unwrap();
        assert!(validate_fixed(root.path()).is_err());
    }
}
