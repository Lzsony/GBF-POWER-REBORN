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

fn localized(traditional: &str, simplified: &str) -> String {
    if crate::appearance::system_language() == gbf_core::preferences::Language::Simplified {
        simplified.to_owned()
    } else {
        traditional.to_owned()
    }
}

pub fn startup_error(error: &dyn std::fmt::Display) {
    let prefix = localized("無法啟動 GBF POWER REBORN", "无法启动 GBF POWER REBORN");
    self::error(&format!("{prefix}：\n{error}"));
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
            bail!(localized(&format!("隨附 WebView2 Runtime 不完整：缺少 {file}。請重新解壓 portable_webview2 套件。"), &format!("随附 WebView2 Runtime 不完整：缺少 {file}。请重新解压 portable_webview2 套件。")));
        }
    }
    if !path.join("Locales").is_dir() {
        bail!(localized(
            "隨附 WebView2 Runtime 缺少 Locales，請重新解壓完整套件。",
            "随附 WebView2 Runtime 缺少 Locales，请重新解压完整套件。"
        ));
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
        tauri::webview_version().map_err(|e| {
            anyhow::anyhow!(
                "{}：{e}",
                localized(
                    "隨附 WebView2 Runtime 無法使用，請重新解壓完整套件",
                    "随附 WebView2 Runtime 无法使用，请重新解压完整套件"
                )
            )
        })?;
    } else {
        // Do not let a stale process environment redirect the lightweight package.
        std::env::remove_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER");
        if tauri::webview_version().is_err() {
            let message = wide(&localized("此電腦缺少可用的 Microsoft WebView2 Runtime。\n請安裝 Evergreen Runtime 後重新啟動，或使用 portable_webview2 完整套件。\n\n是否開啟 Microsoft 官方下載頁？", "此电脑缺少可用的 Microsoft WebView2 Runtime。\n请安装 Evergreen Runtime 后重新启动，或使用 portable_webview2 完整套件。\n\n是否打开 Microsoft 官方下载页？"));
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
