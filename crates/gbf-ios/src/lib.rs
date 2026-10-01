//! Serialized JSON boundary. Secret values and private error details never leave the core.
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use gbf_core::{
    authorization::AuthPhase,
    config::{LineSelection, Mode, Settings},
    error::{CommandError, ErrorCode},
    runtime::Runtime,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    ffi::{c_char, CStr, CString},
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::Mutex,
};

include!(concat!(env!("OUT_DIR"), "/embedded_public_profile.rs"));
const CORE_VERSION_TAG: &str = concat!("GBFP-IOS-CORE-V1:", env!("CARGO_PKG_VERSION"));

struct Engine {
    core: Runtime,
    executor: tokio::runtime::Runtime,
    tunnel: bool,
    _lease: File,
}
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    command: String,
    #[serde(default)]
    root: Option<PathBuf>,
    #[serde(default)]
    tunnel: bool,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    selected_line_id: String,
    #[serde(default)]
    line_selection: LineSelection,
    #[serde(default)]
    https_cache: bool,
    #[serde(default)]
    test_id: String,
}

fn mobile_settings(mut settings: Settings) -> Settings {
    settings.mode = Mode::Accelerate;
    settings.autostart = false;
    settings.close_to_tray = false;
    settings.listen_port = 8123;
    // Retain the shared schema field only for compatibility; iOS applies no disk quota.
    settings.cache_limit_gb = 1;
    settings.cache_preferences.prefetch_enabled = false;
    settings.cache_preferences.warmup_enabled = false;
    settings
}

fn open(request: &Request) -> Result<Engine> {
    let root = request.root.as_ref().ok_or(ErrorCode::ConfigReadFailed)?;
    anyhow::ensure!(root.is_absolute(), ErrorCode::ConfigReadFailed);
    std::fs::create_dir_all(root)?;
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("ios-core.lock"))?;
    lease
        .try_lock_exclusive()
        .context(ErrorCode::StopRequired)?;
    let fresh = !root.join("config.json").exists();
    gbf_core::config_store::initialize(root)?;
    let mut settings = mobile_settings(Settings::load(root)?);
    if fresh {
        settings.https_cache = false;
    }
    settings.save(root)?;
    let profile = EMBEDDED_PUBLIC_PROFILE.map(|(id, url, pem)| gbf_core::EmbeddedPublicProfile {
        deployment_id: id.into(),
        url: url.into(),
        ca_pem: pem.into(),
    });
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let core = {
        let _entered = executor.enter();
        Runtime::new_with_profile(root.clone(), profile)?
    };
    Ok(Engine {
        core,
        executor,
        tunnel: request.tunnel,
        _lease: lease,
    })
}

fn execute(request: Request, slot: &mut Option<Engine>) -> Result<Value> {
    anyhow::ensure!(
        [
            "buildInfo",
            "open",
            "close",
            "refresh",
            "activate",
            "poll",
            "start",
            "stop",
            "status",
            "save",
            "unbind",
            "clearCache",
            "createCertificate",
            "startConnectionCheck",
            "connectionCheckStatus",
            "cancelConnectionCheck"
        ]
        .contains(&request.command.as_str()),
        ErrorCode::InvalidCommand
    );
    if request.command == "buildInfo" {
        return Ok(json!({"profileTag": BUILD_PROFILE_TAG, "coreVersionTag": CORE_VERSION_TAG}));
    }
    if request.command == "open" {
        anyhow::ensure!(slot.is_none(), ErrorCode::NativeOperationFailed);
        *slot = Some(open(&request)?);
        return Ok(json!({"profileTag": BUILD_PROFILE_TAG, "coreVersionTag": CORE_VERSION_TAG}));
    }
    if request.command == "close" {
        if let Some(engine) = slot.take() {
            engine.executor.block_on(engine.core.shutdown())?;
        }
        return Ok(json!({}));
    }
    let engine = slot.as_ref().ok_or(ErrorCode::NativeOperationFailed)?;
    engine.executor.block_on(async {
        let core = &engine.core;
        match request.command.as_str() {
            "refresh" => {
                if engine.tunnel {
                    core.authorization_poll().await?;
                } else {
                    core.authorization.refresh().await?;
                }
            }
            "activate" => {
                anyhow::ensure!(!engine.tunnel, ErrorCode::StopRequired);
                let code = request
                    .code
                    .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
                    .ok_or(ErrorCode::AuthRequired)?;
                core.activate_authorization(Some(code)).await?;
            }
            "poll" => {
                anyhow::ensure!(engine.tunnel, ErrorCode::InvalidCommand);
                core.authorization_poll().await?;
            }
            "start" => {
                anyhow::ensure!(engine.tunnel, ErrorCode::InvalidCommand);
                core.authorization.refresh().await?;
                core.start().await?;
            }
            "stop" => core.stop().await?,
            "startConnectionCheck" => {
                anyhow::ensure!(engine.tunnel, ErrorCode::InvalidCommand);
                return Ok(serde_json::to_value(
                    core.start_connection_check(request.test_id).await?,
                )?);
            }
            "connectionCheckStatus" => {
                anyhow::ensure!(engine.tunnel, ErrorCode::InvalidCommand);
                return Ok(serde_json::to_value(core.connection_check_status().await)?);
            }
            "cancelConnectionCheck" => {
                anyhow::ensure!(engine.tunnel, ErrorCode::InvalidCommand);
                return Ok(serde_json::to_value(
                    core.cancel_connection_check(&request.test_id).await,
                )?);
            }
            "status" => {}
            _ => {
                anyhow::ensure!(
                    !engine.tunnel && !core.control_state().running,
                    ErrorCode::StopRequired
                );
                // Each local operation opens a new core, so authorization must be refreshed.
                core.authorization.refresh().await?;
                match request.command.as_str() {
                    "save" => {
                        let mut settings = core.settings.read().unwrap().clone();
                        settings.selected_line_id = request.selected_line_id;
                        settings.line_selection = request.line_selection;
                        settings.https_cache = request.https_cache;
                        core.save(mobile_settings(settings), None).await?;
                    }
                    "unbind" => core.unbind_authorization().await?,
                    "clearCache" => core.clear_cache().await?,
                    "createCertificate" => {
                        gbf_core::certificate::Authority::create(&core.root)?;
                        let pem =
                            std::fs::read_to_string(gbf_core::certificate::cert_path(&core.root))?;
                        return Ok(json!({"pem": pem}));
                    }
                    _ => bail!(ErrorCode::InvalidCommand),
                }
            }
        }
        if request.command == "unbind" || request.command == "stop" {
            return Ok(json!({}));
        }
        let view = core.authorization.view();
        if matches!(view.state, AuthPhase::Revoked) {
            bail!(ErrorCode::AuthRevoked);
        }
        if matches!(view.state, AuthPhase::Unactivated) {
            bail!(ErrorCode::AuthRequired);
        }
        if !matches!(view.state, AuthPhase::Active) && request.command != "status" {
            bail!(ErrorCode::AuthUnavailable);
        }
        let mut result = serde_json::to_value(core.status().await?)?;
        // iOS does not offer the desktop quota or maintenance contract.
        result.as_object_mut().unwrap().remove("audit");
        result["settings"]
            .as_object_mut()
            .unwrap()
            .remove("cacheLimitGb");
        result["connectionCheck"] = serde_json::to_value(core.connection_check_status().await)?;
        result["pac"] = json!(gbf_core::rules::pac(8123));
        result["profileTag"] = json!(BUILD_PROFILE_TAG);
        result["coreVersionTag"] = json!(CORE_VERSION_TAG);
        Ok(result)
    })
}

fn call(raw: &str) -> String {
    let result = (|| -> Result<Value> {
        anyhow::ensure!(raw.len() <= 64 * 1024, ErrorCode::InvalidCommand);
        let request = serde_json::from_str(raw).context(ErrorCode::InvalidCommand)?;
        let mut engine = ENGINE.lock().map_err(|_| ErrorCode::InternalFailure)?;
        execute(request, &mut engine)
    })();
    match result {
        Ok(value) => json!({"ok": true, "value": value}),
        Err(error) => json!({"ok": false, "error": CommandError::from_error(error, ErrorCode::InternalFailure)}),
    }.to_string()
}

/// # Safety
/// `request` must point to a valid NUL-terminated UTF-8 string for this call.
#[no_mangle]
pub unsafe extern "C" fn gpr_core_call(request: *const c_char) -> *mut c_char {
    let value = std::panic::catch_unwind(|| {
        if request.is_null() {
            return call("{}");
        }
        match unsafe { CStr::from_ptr(request) }.to_str() {
            Ok(raw) => call(raw),
            Err(_) => call("{}"),
        }
    })
    .unwrap_or_else(|_| "{\"ok\":false,\"error\":{\"code\":\"INTERNAL_FAILURE\"}}".into());
    CString::new(value)
        .expect("JSON contains no NUL")
        .into_raw()
}

/// # Safety
/// Free exactly once, only pointers returned by `gpr_core_call`.
#[no_mangle]
pub unsafe extern "C" fn gpr_core_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removed_ios_cache_controls_are_rejected_before_opening_core() {
        for raw in [
            r#"{"command":"audit"}"#,
            r#"{"command":"save","cacheLimitGb":5}"#,
        ] {
            assert!(call(raw).contains("INVALID_COMMAND"));
        }
        let settings = mobile_settings(Settings {
            cache_limit_gb: 90,
            ..Default::default()
        });
        assert_eq!(settings.cache_limit_gb, 1);
    }
    #[test]
    fn rejects_unknown_input_without_echoing_secrets() {
        let result = call(r#"{"command":"activate","code":"private-code","unexpected":true}"#);
        assert!(result.contains("INVALID_COMMAND"));
        assert!(!result.contains("private-code"));
    }
    #[test]
    fn mobile_configuration_cannot_enable_other_modes_or_background_jobs() {
        let settings = mobile_settings(Settings::default());
        assert_eq!(settings.mode, Mode::Accelerate);
        assert!(
            !settings.autostart
                && !settings.cache_preferences.prefetch_enabled
                && !settings.cache_preferences.warmup_enabled
        );
    }
    #[test]
    fn exclusive_owner_prevents_app_and_extension_writing_together() {
        let root = tempfile::tempdir().unwrap();
        let request: Request =
            serde_json::from_value(json!({"command":"open", "root":root.path()})).unwrap();
        let first = open(&request).unwrap();
        assert_eq!(
            open(&request).err().unwrap().downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::StopRequired)
        );
        assert!(
            !first
                .executor
                .block_on(first.core.status())
                .unwrap()
                .authorization
                .configured
        );
        drop(first);
        assert!(open(&request).is_ok());
    }
}
