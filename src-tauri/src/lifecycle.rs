use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{Emitter, Manager};
#[derive(Default)]
struct VisibilityState {
    hidden: AtomicBool,
}

pub const TRAY_ID: &str = "main-tray";

pub fn hide(app: &tauri::AppHandle) -> tauri::Result<()> {
    // Never remove the user's only way of reaching the app.
    if app.tray_by_id(TRAY_ID).is_none() {
        return Ok(());
    }
    app.state::<VisibilityState>()
        .hidden
        .store(true, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window("main") {
        // Remove the native minimized tile before hiding the window.
        window.unminimize()?;
        window.hide()?;
        let _ = app.emit("main-visibility", false);
        #[cfg(target_os = "macos")]
        app.set_activation_policy(tauri::ActivationPolicy::Accessory)?;
    }
    Ok(())
}

pub fn show(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<VisibilityState>() {
        state.hidden.store(false, Ordering::Relaxed);
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        if window.show().is_ok() {
            let _ = app.emit("main-visibility", true);
        }
        let _ = window.set_focus();
    }
}

pub fn close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    if window.app_handle().tray_by_id(TRAY_ID).is_some() {
        api.prevent_close();
        if hide(window.app_handle()).is_err() {
            tracing::warn!("window_hide_failed");
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use block2::RcBlock;
    use objc2::{
        rc::Retained,
        runtime::{AnyObject, ProtocolObject},
    };
    use objc2_app_kit::{NSWindowDidDeminiaturizeNotification, NSWindowDidMiniaturizeNotification};
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
    use std::{cell::RefCell, ptr::NonNull};

    struct Observer(Retained<ProtocolObject<dyn NSObjectProtocol>>);
    impl Drop for Observer {
        fn drop(&mut self) {
            unsafe {
                NSNotificationCenter::defaultCenter()
                    .removeObserver(AsRef::<AnyObject>::as_ref(&*self.0));
            }
        }
    }
    thread_local! { static OBSERVER: RefCell<Vec<Observer>> = const { RefCell::new(Vec::new()) }; }

    pub fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
        let window = app.get_webview_window("main").expect("main window exists");
        let ptr = window.ns_window()?;
        let handle = app.clone();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            let target = handle.clone();
            let _ = handle.run_on_main_thread(move || {
                if hide(&target).is_err() {
                    tracing::warn!("minimize_hide_failed");
                }
            });
        });
        // Register for THIS window only. The notification is sent on AppKit's main thread.
        let token = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSWindowDidMiniaturizeNotification),
                Some(&*(ptr as *const AnyObject)),
                None,
                &block,
            )
        };
        let handle = app.clone();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            if handle
                .state::<VisibilityState>()
                .hidden
                .load(Ordering::Relaxed)
            {
                let target = handle.clone();
                let _ = handle.run_on_main_thread(move || {
                    if let Some(window) = target.get_webview_window("main") {
                        let _ = window.hide();
                    }
                });
            }
        });
        // AppKit's deminiaturize animation may order the window front AFTER hide().
        let restored = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSWindowDidDeminiaturizeNotification),
                Some(&*(ptr as *const AnyObject)),
                None,
                &block,
            )
        };
        OBSERVER
            .with(|observer| *observer.borrow_mut() = vec![Observer(token), Observer(restored)]);
        Ok(())
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{SIZE_MINIMIZED, WM_NCDESTROY, WM_SIZE},
        },
    };
    const SUBCLASS_ID: usize = 0x474246;
    unsafe extern "system" fn callback(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        data: usize,
    ) -> LRESULT {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(callback), id);
            drop(Box::from_raw(data as *mut tauri::AppHandle));
        } else if message == WM_SIZE && wparam == SIZE_MINIMIZED as usize {
            let app = &*(data as *const tauri::AppHandle);
            let target = app.clone();
            let _ = app.run_on_main_thread(move || {
                if hide(&target).is_err() {
                    tracing::warn!("minimize_hide_failed");
                }
            });
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
    pub fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
        let window = app.get_webview_window("main").expect("main window exists");
        let data = Box::into_raw(Box::new(app.clone()));
        let installed = unsafe {
            SetWindowSubclass(
                window.hwnd()?.0 as HWND,
                Some(callback),
                SUBCLASS_ID,
                data as usize,
            )
        };
        if installed == 0 {
            unsafe {
                drop(Box::from_raw(data));
            }
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

pub fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
    app.manage(VisibilityState::default());
    #[cfg(any(target_os = "macos", windows))]
    native::install(app)?;
    Ok(())
}

#[tauri::command]
pub fn main_window_visible(app: tauri::AppHandle) -> bool {
    !app.state::<VisibilityState>()
        .hidden
        .load(Ordering::Relaxed)
        && app
            .get_webview_window("main")
            .is_some_and(|w| w.is_visible().unwrap_or(false))
}
