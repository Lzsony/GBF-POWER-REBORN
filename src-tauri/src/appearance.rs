use gbf_core::preferences::{Language, Preferences, Theme};
use std::{collections::HashMap, sync::OnceLock};
use tauri::{
    menu::{Menu, MenuItem},
    Manager,
};

pub fn text(language: Language, key: &str) -> String {
    static MAPS: OnceLock<[HashMap<String, String>; 4]> = OnceLock::new();
    let maps = MAPS.get_or_init(|| {
        [
            serde_json::from_str(include_str!("../../src/locales/zh-TW.json"))
                .expect("valid translations"),
            serde_json::from_str(include_str!("../../src/locales/zh-CN.json"))
                .expect("valid translations"),
            serde_json::from_str(include_str!("../../src/locales/ja.json"))
                .expect("valid translations"),
            serde_json::from_str(include_str!("../../src/locales/en.json"))
                .expect("valid translations"),
        ]
    });
    let index = match language {
        Language::Traditional => 0,
        Language::Simplified => 1,
        Language::Japanese => 2,
        Language::English => 3,
    };
    maps[index].get(key).cloned().unwrap_or_else(|| key.into())
}
pub fn system_language() -> Language {
    #[cfg(target_os = "macos")]
    {
        let languages = objc2_foundation::NSLocale::preferredLanguages();
        languages
            .firstObject()
            .map(|s| Language::from_system(&s.to_string()))
            .unwrap_or(Language::English)
    }
    #[cfg(windows)]
    {
        let mut buffer = [0u16; 85];
        let count = unsafe {
            windows_sys::Win32::Globalization::GetUserDefaultLocaleName(
                buffer.as_mut_ptr(),
                buffer.len() as i32,
            )
        };
        if count > 1 {
            Language::from_system(&String::from_utf16_lossy(&buffer[..count as usize - 1]))
        } else {
            Language::English
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    Language::English
}
pub fn tray_menu(app: &tauri::AppHandle, language: Language) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(app, "show", text(language, "trayShow"), true, None::<&str>)?;
    let state = crate::tray_control::snapshot(app).state;
    let action = MenuItem::with_id(
        app,
        if state.running { "stop" } else { "start" },
        text(language, if state.running { "stop" } else { "start" }),
        !state.busy && !state.shutting_down && (state.running || !state.maintenance),
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", text(language, "trayQuit"), true, None::<&str>)?;
    Menu::with_items(app, &[&show, &action, &quit])
}
pub fn apply_theme(app: &tauri::AppHandle, theme: Theme) -> tauri::Result<()> {
    let native_theme = match theme {
        Theme::Auto => None,
        Theme::Light => Some(tauri::Theme::Light),
        Theme::Dark => Some(tauri::Theme::Dark),
    };
    app.set_theme(native_theme);
    if let Some(window) = app.get_webview_window("main") {
        window.set_theme(native_theme)?;
        let dark = match theme {
            Theme::Dark => true,
            Theme::Light => false,
            Theme::Auto => window.theme()? == tauri::Theme::Dark,
        };
        window.set_background_color(Some(if dark {
            tauri::window::Color(23, 27, 32, 255)
        } else {
            tauri::window::Color(246, 248, 250, 255)
        }))?;
    }
    Ok(())
}
pub fn apply(app: &tauri::AppHandle, preferences: Preferences) -> tauri::Result<()> {
    apply_theme(app, preferences.theme)?;
    if let Some(tray) = app.tray_by_id(crate::lifecycle::TRAY_ID) {
        tray.set_menu(Some(tray_menu(app, preferences.language)?))?;
    }
    #[cfg(target_os = "macos")]
    {
        use tauri::menu::{PredefinedMenuItem, Submenu};
        let quit = MenuItem::with_id(
            app,
            "quit",
            text(preferences.language, "trayQuit"),
            true,
            Some("CmdOrCtrl+Q"),
        )?;
        let about = Submenu::with_items(app, "GBF POWER REBORN", true, &[&quit])?;
        let undo = PredefinedMenuItem::undo(app, Some(&text(preferences.language, "undo")))?;
        let redo = PredefinedMenuItem::redo(app, Some(&text(preferences.language, "redo")))?;
        let cut = PredefinedMenuItem::cut(app, Some(&text(preferences.language, "cut")))?;
        let copy = PredefinedMenuItem::copy(app, Some(&text(preferences.language, "copy")))?;
        let paste = PredefinedMenuItem::paste(app, Some(&text(preferences.language, "paste")))?;
        let all =
            PredefinedMenuItem::select_all(app, Some(&text(preferences.language, "selectAll")))?;
        let edit = Submenu::with_items(
            app,
            text(preferences.language, "editMenu"),
            true,
            &[&undo, &redo, &cut, &copy, &paste, &all],
        )?;
        app.set_menu(Menu::with_items(app, &[&about, &edit])?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_strings_and_placeholders_cover_all_languages() {
        for (language, show, title) in [
            (
                Language::Traditional,
                "顯示主視窗",
                "GBF POWER REBORN 無法啟動",
            ),
            (
                Language::Simplified,
                "显示主窗口",
                "GBF POWER REBORN 无法启动",
            ),
            (
                Language::Japanese,
                "メインウィンドウを表示",
                "GBF POWER REBORN を起動できません",
            ),
            (
                Language::English,
                "Show window",
                "Unable to start GBF POWER REBORN",
            ),
        ] {
            assert_eq!(text(language, "trayShow"), show);
            assert_eq!(text(language, "startupErrorTitle"), title);
            assert!(text(language, "startupErrorMessage").contains("schemaVersion 1"));
            assert!(text(language, "runtimeMissingFile").contains("{file}"));
            for key in [
                "trayQuit",
                "editMenu",
                "undo",
                "redo",
                "cut",
                "copy",
                "paste",
                "selectAll",
                "runtimeMissingLocales",
                "runtimeUnavailable",
                "runtimeInstallPrompt",
            ] {
                let translated = text(language, key);
                assert!(!translated.trim().is_empty());
                assert_ne!(translated, key);
            }
        }
    }
}
