use gbf_core::{
    error::{CommandError, ErrorCode},
    runtime::{ControlState, Runtime},
};
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use tauri::{Emitter, Manager};

#[derive(Clone, Copy, Serialize)]
pub struct Notice {
    pub id: u64,
    pub code: ErrorCode,
}
#[derive(Default)]
pub struct NativeControls {
    pending: AtomicBool,
    revision: AtomicU64,
    notice: Mutex<Option<Notice>>,
}
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub state: ControlState,
    pub notice: Option<Notice>,
}

pub fn acknowledge(app: &tauri::AppHandle, id: u64) {
    let controls = app.state::<NativeControls>();
    let mut notice = controls.notice.lock().unwrap();
    if notice.is_some_and(|notice| notice.id == id) {
        *notice = None;
    }
}

pub fn snapshot(app: &tauri::AppHandle) -> Snapshot {
    let mut state = app
        .try_state::<Arc<Runtime>>()
        .map(|core| core.control_state())
        .unwrap_or_default();
    let notice = app.try_state::<NativeControls>().and_then(|native| {
        state.busy |= native.pending.load(Ordering::SeqCst);
        *native.notice.lock().unwrap()
    });
    Snapshot { state, notice }
}

pub fn refresh(app: &tauri::AppHandle) {
    let target = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(core) = target.try_state::<Arc<Runtime>>() {
            let language = core.settings.read().unwrap().preferences.language;
            if let Some(tray) = target.tray_by_id(crate::lifecycle::TRAY_ID) {
                if let Ok(menu) = crate::appearance::tray_menu(&target, language) {
                    let _ = tray.set_menu(Some(menu));
                }
            }
        }
        let _ = target.emit("native-control", snapshot(&target));
    });
}

pub fn install(app: &tauri::AppHandle) {
    app.manage(NativeControls::default());
    let mut changes = app.state::<Arc<Runtime>>().subscribe_control();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while changes.changed().await.is_ok() {
            refresh(&app);
        }
    });
}

pub fn action(app: &tauri::AppHandle, start: bool) {
    let native = app.state::<NativeControls>();
    let state = snapshot(app).state;
    if state.busy
        || state.shutting_down
        || (!state.running && state.maintenance)
        || native
            .pending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        return;
    }
    *native.notice.lock().unwrap() = None;
    // Publish/disable on the UI thread before handing the fixed action to async work.
    refresh(app);
    let core = app.state::<Arc<Runtime>>().inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = if start {
            core.start().await
        } else {
            core.stop().await
        };
        let native = app.state::<NativeControls>();
        if let Err(error) = result {
            let code = CommandError::from_error(error, ErrorCode::InternalFailure).code;
            *native.notice.lock().unwrap() = Some(Notice {
                id: native.revision.fetch_add(1, Ordering::SeqCst) + 1,
                code,
            });
            // Persist before notifying or showing: a webview still loading can fetch it.
            let target = app.clone();
            let _ = app.run_on_main_thread(move || crate::lifecycle::show(&target));
        }
        native.pending.store(false, Ordering::SeqCst);
        refresh(&app);
    });
}
