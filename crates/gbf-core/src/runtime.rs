#[path = "proxy_test.rs"]
mod proxy_test;
use crate::acceleration::{self, AccelerationStatus, LineView, SharedStatus, Tunnel};
use crate::error::ErrorCode;
use crate::{
    cache::Cache,
    certificate::{self, Authority, CertificateStatus},
    config::{read_password, write_password, LineSelection, Mode, Settings},
    metrics::{Metrics, ProbeSample, Snapshot},
    proxy::{self, ContextState},
    routing,
};
use anyhow::{bail, Context, Result};
pub use proxy_test::ProxyTestResult;
pub type LineTestResult = ProxyTestResult;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};

#[cfg(test)]
use std::sync::atomic::Ordering;

struct Running {
    tunnel: Option<Tunnel>,
    context: Arc<ContextState>,
    listener: JoinHandle<()>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::{Language, Preferences, Theme};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn control_notifications_cover_busy_failures_idempotence_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let core = Runtime::new(dir.path().into()).unwrap();
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        core.settings.write().unwrap().listen_port = port;
        let mut changes = core.subscribe_control();
        let operation = core.operation().await;
        changes.changed().await.unwrap();
        assert!(changes.borrow_and_update().busy);
        drop(operation);
        assert!(!core.control_state().busy);
        assert!(core.start().await.is_err());
        assert_eq!(core.control_state(), ControlState::default());
        drop(occupied);
        core.start().await.unwrap();
        let start = *core.started.read().unwrap();
        assert!(core.control_state().running);
        core.start().await.unwrap();
        assert_eq!(*core.started.read().unwrap(), start);
        core.stop().await.unwrap();
        core.stop().await.unwrap();
        assert_eq!(core.control_state(), ControlState::default());
        assert!(TcpListener::bind(("127.0.0.1", port)).await.is_ok());
        core.shutdown().await.unwrap();
        assert!(core.start().await.is_err());
        assert!(core.control_state().shutting_down);
        assert!(!core.control_state().running);
    }

    #[tokio::test]
    async fn generic_client_rejects_acceleration_without_changing_direct_or_proxy_settings() {
        let dir = tempfile::tempdir().unwrap();
        let core = Runtime::new(dir.path().into()).unwrap();
        assert!(!core.status().await.unwrap().authorization.configured);
        let accelerate = Settings {
            mode: Mode::Accelerate,
            ..Default::default()
        };
        assert_eq!(
            core.save(accelerate.clone(), None)
                .await
                .unwrap_err()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::AuthNotConfigured)
        );
        assert!(core.probe(accelerate, None).await.is_err());
        assert_eq!(Settings::load(dir.path()).unwrap().mode, Mode::Direct);
        core.save(
            Settings {
                mode: Mode::Socks5,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().mode, Mode::Socks5);
        assert!(!dir.path().join("deployments").exists());
        assert!(crate::config_store::load(dir.path())
            .unwrap()
            .authorizations
            .is_empty());
    }

    struct MemoryVault(std::sync::Mutex<String>);
    impl PasswordVault for MemoryVault {
        fn read(&self) -> Result<String> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn write(&self, v: &str) -> Result<()> {
            *self.0.lock().unwrap() = v.into();
            Ok(())
        }
    }
    struct FailingVault {
        value: std::sync::Mutex<String>,
        writes: std::sync::atomic::AtomicUsize,
        fail: usize,
    }
    impl PasswordVault for FailingVault {
        fn read(&self) -> Result<String> {
            Ok(self.value.lock().unwrap().clone())
        }
        fn write(&self, value: &str) -> Result<()> {
            if self.writes.fetch_add(1, Ordering::SeqCst) + 1 == self.fail {
                bail!(ErrorCode::SecretWriteFailed);
            }
            *self.value.lock().unwrap() = value.into();
            Ok(())
        }
    }
    struct FailingSettings(std::sync::atomic::AtomicBool);
    impl SettingsWriter for FailingSettings {
        fn save(&self, root: &std::path::Path, settings: &Settings) -> Result<()> {
            if self.0.swap(false, Ordering::SeqCst) {
                anyhow::bail!("injected configuration write failure");
            }
            settings.save(root)
        }
    }
    #[tokio::test]
    async fn injected_setting_and_secret_failures_restore_route_or_stop_cleanly() {
        for failure in ["settings", "secret", "rollback"] {
            let (dir, mut core, server, port) = switching_fixture().await;
            core.vault = Arc::new(FailingVault {
                value: std::sync::Mutex::new("old-secret".into()),
                writes: Default::default(),
                fail: match failure {
                    "secret" => 1,
                    "rollback" => 2,
                    _ => usize::MAX,
                },
            });
            if failure == "settings" {
                core.settings_writer = Arc::new(FailingSettings(true.into()));
            }
            let old = core.settings.read().unwrap().clone();
            core.start().await.unwrap();
            let mut next = old.clone();
            next.mode = Mode::Http;
            next.upstream_port = if failure == "rollback" { 1 } else { port };
            next.username = "changed".into();
            let error = core
                .save(next, Some("new-secret".into()))
                .await
                .unwrap_err();
            let stopped = failure == "rollback";
            assert_eq!(
                error.downcast_ref::<ErrorCode>(),
                Some(&if stopped {
                    ErrorCode::SwitchRestoreFailed
                } else {
                    ErrorCode::SwitchFailedRestored
                })
            );
            assert_eq!(core.status().await.unwrap().running, !stopped);
            assert_eq!(Settings::load(dir.path()).unwrap().username, old.username);
            assert_eq!(core.settings.read().unwrap().mode, old.mode);
            if !stopped {
                assert_eq!(core.vault.read().unwrap(), "old-secret");
            }
            core.stop().await.unwrap();
            assert!(TcpListener::bind(("127.0.0.1", old.listen_port))
                .await
                .is_ok());
            server.abort();
        }
    }
    async fn switching_fixture() -> (tempfile::TempDir, Runtime, JoinHandle<()>, u16) {
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = origin.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut b = Vec::new();
                    while !b.ends_with(b"\r\n\r\n") {
                        match socket.read_u8().await {
                            Ok(x) => b.push(x),
                            Err(_) => return,
                        };
                        if b.len() > 8192 {
                            return;
                        }
                    }
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                });
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        Settings {
            listen_port: port,
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let mut core = Runtime::new(dir.path().into()).unwrap();
        core.switch_probe_url = format!("http://127.0.0.1:{origin_port}/");
        core.vault = Arc::new(MemoryVault(std::sync::Mutex::new("old-secret".into())));
        (dir, core, server, origin_port)
    }
    #[tokio::test]
    async fn immediate_switch_keeps_counters_and_rolls_back_credentials() {
        let (dir, core, server, port) = switching_fixture().await;
        core.start().await.unwrap();
        let before = Instant::now() - Duration::from_secs(120);
        *core.started.write().unwrap() = Some(before);
        let metrics = core.metrics.read().unwrap().clone();
        metrics.requests.store(7, Ordering::Relaxed);
        metrics.downloads.store(3, Ordering::Relaxed);
        let mut next = core.settings.read().unwrap().clone();
        next.mode = Mode::Http;
        next.upstream_port = port;
        next.username = "user".into();
        core.save(next.clone(), Some("new-secret".into()))
            .await
            .unwrap();
        assert!(core.status().await.unwrap().running);
        assert_eq!(*core.started.read().unwrap(), Some(before));
        assert!(Arc::ptr_eq(&metrics, &core.metrics.read().unwrap()));
        assert_eq!(metrics.requests.load(Ordering::Relaxed), 7);
        assert_eq!(core.vault.read().unwrap(), "new-secret");
        assert_eq!(Settings::load(dir.path()).unwrap().upstream_port, port);
        let mut bad = next;
        bad.upstream_port = 1;
        bad.username = "bad-user".into();
        let error = core.save(bad, Some("bad-secret".into())).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::SwitchFailedRestored)
        );
        assert!(core.status().await.unwrap().running);
        assert_eq!(core.vault.read().unwrap(), "new-secret");
        assert_eq!(Settings::load(dir.path()).unwrap().username, "user");
        assert_eq!(metrics.downloads.load(Ordering::Relaxed), 3);
        core.stop().await.unwrap();
        server.abort();
    }
    #[tokio::test]
    async fn restore_failure_stops_and_stopped_save_never_starts() {
        let (dir, core, server, port) = switching_fixture().await;
        let mut settings = core.settings.read().unwrap().clone();
        settings.mode = Mode::Http;
        settings.upstream_port = port;
        core.save(settings.clone(), None).await.unwrap();
        assert!(!core.status().await.unwrap().running);
        core.start().await.unwrap();
        server.abort();
        let _ = server.await;
        let mut bad = settings.clone();
        bad.upstream_port = 1;
        assert_eq!(
            core.save(bad, None)
                .await
                .unwrap_err()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::SwitchRestoreFailed)
        );
        assert!(!core.status().await.unwrap().running);
        assert_eq!(Settings::load(dir.path()).unwrap().upstream_port, port);
        assert!(
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, settings.listen_port))
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn normal_stop_flushes_access_order_after_workers_finish() {
        let (_dir, core, server, _) = switching_fixture().await;
        core.start().await.unwrap();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/stop.js";
        let key = crate::cache::CacheKey::from_url(uri).unwrap();
        let request = http::Request::builder().uri(uri).body(()).unwrap();
        let response = http::Response::builder()
            .header("cache-control", "public, max-age=600")
            .body(())
            .unwrap();
        core.cache
            .put(
                &key,
                crate::cache::Cached {
                    policy: crate::cache::policy(&request, &response).unwrap(),
                    body: bytes::Bytes::from_static(b"asset"),
                },
                1000,
            )
            .unwrap();
        let db = rusqlite::Connection::open(core.cache.root().join("index.sqlite3")).unwrap();
        let before: i64 = db
            .query_row("SELECT accessed FROM entries", [], |r| r.get(0))
            .unwrap();
        core.cache.get_ram(&key);
        core.stop().await.unwrap();
        let after: i64 = db
            .query_row("SELECT accessed FROM entries", [], |r| r.get(0))
            .unwrap();
        assert!(after > before);
        server.abort();
    }
    #[tokio::test]
    async fn independent_autostart_save_does_not_restart_and_concurrent_saves_serialize() {
        let (_dir, core, server, _) = switching_fixture().await;
        core.start().await.unwrap();
        let start = *core.started.read().unwrap();
        let metrics = core.metrics.read().unwrap().clone();
        let mut a = core.settings.read().unwrap().clone();
        a.autostart = true;
        let mut b = a.clone();
        b.autostart = false;
        let (x, y) = tokio::join!(core.save(a, None), core.save(b, None));
        x.unwrap();
        y.unwrap();
        assert_eq!(*core.started.read().unwrap(), start);
        assert!(Arc::ptr_eq(&metrics, &core.metrics.read().unwrap()));
        assert!(core.status().await.unwrap().running);
        core.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn audit_cancellation_releases_maintenance_and_shutdown_closes_admission() {
        let dir = tempfile::tempdir().unwrap();
        let core = Arc::new(Runtime::new(dir.path().into()).unwrap());
        let cache = core.cache.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let held = tokio::task::spawn_blocking(move || cache.hold_disk(entered_tx, release_rx));
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        core.start_audit().await.unwrap();
        assert!(core.control_state().maintenance);
        assert!(core.audit.progress.lock().unwrap().running);
        assert_eq!(
            core.start().await.unwrap_err().downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::CacheMaintenanceBusy)
        );
        assert!(core.save(Settings::default(), None).await.is_err());
        assert!(core.clear_cache().await.is_err());
        let cancelled = tokio::spawn({
            let core = core.clone();
            async move { core.cancel_audit().await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !core.audit.cancel.load(Ordering::Relaxed) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        release_tx.send(()).unwrap();
        held.await.unwrap();
        cancelled.await.unwrap().unwrap();
        let status = core.status().await.unwrap();
        assert!(!status.audit.running);
        assert!(status.audit.cancelled);
        assert!(!core.control_state().maintenance);
        core.clear_cache().await.unwrap();
        core.shutdown().await.unwrap();
        assert!(core.start_audit().await.is_err());
    }

    #[tokio::test]
    async fn cache_preference_changes_preserve_live_route_counters_and_other_preferences() {
        use crate::preferences::CachePreferencePatch;
        let (_dir, core, server, _) = switching_fixture().await;
        core.start().await.unwrap();
        let started = *core.started.read().unwrap();
        let metrics = core.metrics.read().unwrap().clone();
        metrics.requests.store(7, Ordering::Relaxed);
        let background = crate::preferences::CachePreferences {
            prefetch_enabled: false,
            warmup_enabled: false,
        };
        let saved = core
            .save_cache_preferences(CachePreferencePatch {
                prefetch_enabled: Some(false),
                warmup_enabled: Some(false),
            })
            .await
            .unwrap();
        assert_eq!(saved, background);
        assert_eq!(*core.started.read().unwrap(), started);
        assert!(Arc::ptr_eq(&metrics, &core.metrics.read().unwrap()));
        assert_eq!(metrics.requests.load(Ordering::Relaxed), 7);
        assert_eq!(core.status().await.unwrap().cache_preferences, background);
        assert!(core.status().await.unwrap().running);
        assert_eq!(
            Settings::load(&core.root).unwrap().cache_preferences,
            background
        );
        core.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn cache_preferences_merge_without_overwriting_other_settings_and_audit_blocks_mutations()
    {
        use crate::preferences::CachePreferencePatch;
        let dir = tempfile::tempdir().unwrap();
        crate::config_store::update(dir.path(), |d| d.settings.listen_port = 8125).unwrap();
        let core = Runtime::new(dir.path().into()).unwrap();
        assert!(
            core.status()
                .await
                .unwrap()
                .cache_preferences
                .prefetch_enabled
        );
        let mut draft = core.settings.read().unwrap().clone();
        draft.listen_port = 8126;
        core.save_cache_preferences(CachePreferencePatch {
            prefetch_enabled: Some(false),
            warmup_enabled: None,
        })
        .await
        .unwrap();
        core.save(draft, None).await.unwrap();
        let saved = Settings::load(dir.path()).unwrap();
        assert_eq!(saved.listen_port, 8126);
        assert!(!saved.cache_preferences.prefetch_enabled);
        assert!(saved.cache_preferences.warmup_enabled);
        core.audit.progress.lock().unwrap().running = true;
        assert!(core
            .start()
            .await
            .unwrap_err()
            .downcast_ref::<ErrorCode>()
            .is_some_and(|e| *e == ErrorCode::CacheMaintenanceBusy));
        assert!(core.clear_cache().await.is_err());
        assert!(core.start_audit().await.is_err());
        core.audit.progress.lock().unwrap().running = false;
        core.start_audit().await.unwrap();
        core.cancel_audit().await.unwrap();
        assert!(!core.status().await.unwrap().audit.running);
        core.clear_cache().await.unwrap();
    }
    #[tokio::test]
    async fn preferences_do_not_overwrite_connection_drafts_or_stop_active_core() {
        let dir = tempfile::tempdir().unwrap();
        let core = Runtime::new(dir.path().to_path_buf()).unwrap();
        let mut draft = core.settings.read().unwrap().clone();
        draft.listen_port = 8125;
        let preferences = Preferences {
            theme: Theme::Dark,
            language: Language::Traditional,
        };
        core.save_preferences(preferences).await.unwrap();
        core.save(draft, None).await.unwrap();
        let saved = Settings::load(dir.path()).unwrap();
        assert_eq!(saved.preferences, preferences);
        assert_eq!(saved.listen_port, 8125);
        let metrics = core.metrics.read().unwrap().clone();
        metrics.requests.store(7, Ordering::Relaxed);
        let context = Arc::new(
            ContextState::new(saved, String::new(), core.cache.clone(), None, metrics).unwrap(),
        );
        *core.active.lock().await = Some(Running {
            context: context.clone(),
            listener: tokio::spawn(async {}),
            tunnel: None,
        });
        *core.started.write().unwrap() = Some(Instant::now());
        core.save_preferences(Preferences {
            theme: Theme::Light,
            language: Language::Simplified,
        })
        .await
        .unwrap();
        let status = core.status().await.unwrap();
        assert!(status.running);
        assert_eq!(status.metrics.requests, 7);
        assert!(!context.cancel.is_cancelled());
        assert_eq!(status.preferences.language, Language::Simplified);
        core.stop().await.unwrap();
    }

    #[tokio::test]
    async fn timed_head_distinguishes_success_failure_and_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
            }
            assert!(request.starts_with(b"HEAD /probe HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{address}/probe");
        assert!(matches!(
            timed_head(&client, &url).await,
            ProbeSample::Success(_)
        ));
        server.await.unwrap();
        assert_eq!(timed_head(&client, &url).await, ProbeSample::Failure);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/probe", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        assert_eq!(
            timed_head_with_timeout(&client, &url, Duration::from_millis(50)).await,
            ProbeSample::Timeout
        );
        server.abort();
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProbe {
    pub latency_ms: u64,
    pub line_id: Option<String>,
    pub line_name: Option<String>,
}

trait PasswordVault: Send + Sync {
    fn read(&self) -> Result<String>;
    fn write(&self, value: &str) -> Result<()>;
}
struct SystemVault;
impl PasswordVault for SystemVault {
    fn read(&self) -> Result<String> {
        read_password()
    }
    fn write(&self, value: &str) -> Result<()> {
        write_password(value)
    }
}

trait SettingsWriter: Send + Sync {
    fn save(&self, root: &std::path::Path, settings: &Settings) -> Result<()>;
}
struct DiskSettings;
impl SettingsWriter for DiskSettings {
    fn save(&self, root: &std::path::Path, settings: &Settings) -> Result<()> {
        settings.save(root)
    }
}

/// Small, disk-free lifecycle snapshot for native controls and hidden windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlState {
    pub running: bool,
    pub busy: bool,
    pub maintenance: bool,
    pub shutting_down: bool,
}

struct Operation<'a> {
    core: &'a Runtime,
    _lock: tokio::sync::MutexGuard<'a, ()>,
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        let running = self.core.started.read().unwrap().is_some();
        self.core.control.send_modify(|state| {
            state.running = running;
            state.busy = false;
        });
    }
}

pub struct Runtime {
    vault: Arc<dyn PasswordVault>,
    settings_writer: Arc<dyn SettingsWriter>,
    #[cfg(test)]
    switch_probe_url: String,
    pub root: PathBuf,
    pub authorization: crate::authorization::Authorization,
    operations: Mutex<()>,
    control: tokio::sync::watch::Sender<ControlState>,
    proxy_test: Mutex<Option<proxy_test::Job>>,
    pub cache: Arc<Cache>,
    pub settings: RwLock<Settings>,
    pub certificate: RwLock<CertificateStatus>,
    metrics: RwLock<Arc<Metrics>>,
    active: Mutex<Option<Running>>,
    started: RwLock<Option<Instant>>,
    acceleration: SharedStatus,
    selected_status: RwLock<Option<SharedStatus>>,
    audit: Arc<crate::cache::AuditState>,
    audit_job: Mutex<Option<JoinHandle<()>>>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub running: bool,
    pub authorization: crate::authorization::View,
    pub acceleration: AccelerationStatus,
    pub acceleration_lines: Vec<LineView>,
    pub cache_preferences: crate::preferences::CachePreferences,
    pub audit: crate::cache::AuditProgress,
    pub settings: crate::connection::SettingsView,
    pub preferences: crate::preferences::Preferences,
    pub metrics: Snapshot,
    pub cache_bytes: u64,
    pub certificate: CertificateStatus,
    pub pac_url: String,
}
impl Runtime {
    fn switch_probe_url(&self) -> &str {
        #[cfg(test)]
        {
            &self.switch_probe_url
        }
        #[cfg(not(test))]
        {
            "https://game.granbluefantasy.jp/"
        }
    }
    pub fn new(root: PathBuf) -> Result<Self> {
        Self::new_with_profile(root, None)
    }
    pub fn new_with_profile(
        root: PathBuf,
        profile: Option<crate::EmbeddedPublicProfile>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        crate::config_store::initialize(&root)?;
        let settings = Settings::load(&root)?;
        let cache = Arc::new(Cache::open(&root.join("cache"))?);
        let cert = certificate::status(&root);
        Ok(Self {
            vault: Arc::new(SystemVault),
            settings_writer: Arc::new(DiskSettings),
            #[cfg(test)]
            switch_probe_url: "https://game.granbluefantasy.jp/".into(),
            authorization: crate::authorization::Authorization::new(root.clone(), profile)?,
            operations: Mutex::new(()),
            control: tokio::sync::watch::channel(ControlState::default()).0,
            proxy_test: Mutex::new(None),
            root,
            cache,
            settings: RwLock::new(settings),
            certificate: RwLock::new(cert),
            metrics: RwLock::new(Arc::new(Metrics::default())),
            active: Mutex::new(None),
            started: RwLock::new(None),
            acceleration: Arc::new(RwLock::new(Default::default())),
            selected_status: RwLock::new(None),
            audit: Arc::new(Default::default()),
            audit_job: Mutex::new(None),
        })
    }
    pub fn control_state(&self) -> ControlState {
        *self.control.borrow()
    }
    pub fn subscribe_control(&self) -> tokio::sync::watch::Receiver<ControlState> {
        self.control.subscribe()
    }
    async fn operation(&self) -> Operation<'_> {
        let lock = self.operations.lock().await;
        self.control.send_modify(|state| state.busy = true);
        Operation {
            core: self,
            _lock: lock,
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        // Close admission before joining the operation queue, so queued starts cannot
        // reopen the listener after the final stop.
        self.control.send_modify(|state| state.shutting_down = true);
        self.stop().await
    }
    pub async fn status(&self) -> Result<Status> {
        let started = *self.started.read().unwrap();
        let settings = self.settings.read().unwrap().clone();
        let metrics = self.metrics.read().unwrap().snapshot(started);
        let cache = self.cache.clone();
        let cache_bytes = tokio::task::spawn_blocking(move || cache.usage()).await??;
        Ok(Status {
            running: started.is_some(),
            authorization: self.authorization.view(),
            acceleration: self.acceleration_status(),
            acceleration_lines: self
                .authorization
                .available_lines()
                .into_iter()
                .map(|(id, name)| LineView {
                    revision: self
                        .authorization
                        .line(&id)
                        .map(|line| line.revision(&self.root))
                        .unwrap_or_default(),
                    id,
                    name,
                })
                .collect(),
            cache_preferences: settings.cache_preferences,
            audit: self.audit.progress.lock().unwrap().clone(),
            pac_url: settings.pac_url(),
            settings: crate::connection::SettingsView::new(&settings),
            preferences: settings.preferences,
            metrics,
            cache_bytes,
            certificate: self.certificate.read().unwrap().clone(),
        })
    }
    pub async fn cancel_line_test(&self, id: Option<&str>) {
        self.cancel_proxy_test(id).await;
    }
    pub async fn start(&self) -> Result<()> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        self.start_inner().await
    }
    async fn start_inner(&self) -> Result<()> {
        self.start_preserving(None, None, false).await
    }
    async fn start_preserving(
        &self,
        retained: Option<(Arc<Metrics>, Instant)>,
        forced: Option<acceleration::Line>,
        verify_path: bool,
    ) -> Result<()> {
        if self.control_state().shutting_down {
            bail!(ErrorCode::NativeOperationFailed);
        }
        let mut active = self.active.lock().await;
        if active.is_some() {
            return Ok(());
        }
        self.ensure_no_audit()?;
        let settings = self.settings.read().unwrap().clone();
        settings.validate()?;
        if matches!(settings.mode, Mode::Http | Mode::Socks5) {
            let addresses = tokio::net::lookup_host((
                settings.upstream_host.trim_matches(['[', ']']),
                settings.upstream_port,
            ))
            .await
            .context(ErrorCode::DnsFailed)?;
            if addresses
                .into_iter()
                .any(|a| a.ip().is_loopback() && a.port() == settings.listen_port)
            {
                bail!(ErrorCode::ProxyLoop);
            }
        }
        let root = self.root.clone();
        let secure_settings = settings.clone();
        let vault = self.vault.clone();
        let (authority, password, cert) = tokio::task::spawn_blocking(move || -> Result<_> {
            let cert = certificate::status(&root);
            let authority = if secure_settings.https_cache {
                if !cert.trusted {
                    bail!(ErrorCode::CertificateRequired);
                }
                Some(Authority::load(&root)?)
            } else {
                None
            };
            let password = if matches!(secure_settings.mode, Mode::Http | Mode::Socks5)
                && !secure_settings.username.is_empty()
            {
                vault.read()?
            } else {
                String::new()
            };
            Ok((authority, password, cert))
        })
        .await??;
        *self.certificate.write().unwrap() = cert;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, settings.listen_port))
            .await
            .context(ErrorCode::ProxyBindFailed)?;
        let mut tunnel = if settings.mode == Mode::Accelerate {
            Some(if let Some(line) = forced {
                Tunnel::start(&self.root, line, self.acceleration.clone()).await?
            } else {
                self.open_acceleration(&settings).await?.0
            })
        } else {
            *self.acceleration.write().unwrap() = Default::default();
            None
        };
        let mode = match settings.mode {
            Mode::Direct => "direct",
            Mode::Accelerate => "accelerate",
            Mode::Http => "http",
            Mode::Socks5 => "socks5",
        };
        let routed = tunnel
            .as_ref()
            .map(|t| t.routed_settings(&settings))
            .unwrap_or(settings);
        let metrics = retained
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_else(|| Arc::new(Metrics::default()));
        let context = match ContextState::new(
            routed,
            password,
            self.cache.clone(),
            authority,
            metrics.clone(),
        ) {
            Ok(context) => Arc::new(context),
            Err(error) => {
                if let Some(tunnel) = tunnel.as_mut() {
                    tunnel.stop().await;
                }
                return Err(error);
            }
        };
        if verify_path && tunnel.is_none() {
            let probe = context
                .upstream
                .head(self.switch_probe_url())
                .timeout(Duration::from_secs(5))
                .send()
                .await;
            if !probe.is_ok_and(|r| r.status().is_success()) {
                bail!(ErrorCode::ConnectionTestFailed)
            }
        }
        *self.metrics.write().unwrap() = metrics;
        let listener = tokio::spawn(proxy::serve(listener, context.clone()));
        for target in 0..3 {
            if target == 0 && tunnel.is_none() {
                continue;
            }
            let probe = context.clone();
            let session: Option<crate::probe::SessionReader> = if target == 0 {
                tunnel.as_ref().map(|t| Box::new(t.session_reader()) as _)
            } else {
                None
            };
            let tcp_target = if target == 0 {
                tunnel.as_ref().map(|t| (t.line.host.clone(), t.line.port))
            } else {
                None
            };
            context.tasks.spawn(async move {
                crate::probe::monitor(probe, target, session, tcp_target).await;
            });
        }
        context
            .configure_background(context.settings.cache_preferences)
            .await;
        let line = self.acceleration.read().unwrap().line_id.clone();
        tracing::info!(
            mode,
            line = line.as_deref().unwrap_or("none"),
            "proxy_started"
        );
        *active = Some(Running {
            context,
            listener,
            tunnel,
        });
        *self.started.write().unwrap() = Some(
            retained
                .map(|(_, start)| start)
                .unwrap_or_else(Instant::now),
        );
        Ok(())
    }
    pub async fn stop(&self) -> Result<()> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        self.stop_inner().await
    }
    async fn stop_inner(&self) -> Result<()> {
        self.cancel_proxy_test(None).await;
        let mut active = self.active.lock().await;
        if let Some(mut running) = active.take() {
            *self.started.write().unwrap() = None;
            running.context.cancel.cancel();
            if let Some(tunnel) = running.tunnel.as_mut() {
                tunnel.stop().await;
            }
            *self.acceleration.write().unwrap() = Default::default();
            *self.selected_status.write().unwrap() = None;
            running
                .context
                .configure_background(crate::preferences::CachePreferences {
                    prefetch_enabled: false,
                    warmup_enabled: false,
                })
                .await;
            let _ = running.listener.await;
            running.context.tasks.close();
            running.context.tasks.wait().await;
            tracing::info!("proxy_stopped");
        }
        self.cancel_audit().await?;
        for target in 0..3 {
            self.metrics.read().unwrap().reset_probe(target);
        }
        let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || cache.flush_accesses()).await??;
        Ok(())
    }
    pub async fn save(&self, mut settings: Settings, password: Option<String>) -> Result<()> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        self.ensure_no_audit()?;
        settings.validate()?;
        let old = self.settings.read().unwrap().clone();
        settings.preferences = old.preferences;
        settings.cache_preferences = old.cache_preferences;
        let started = *self.started.read().unwrap();
        if started.is_some()
            && (settings.listen_port != old.listen_port
                || settings.cache_limit_gb != old.cache_limit_gb)
        {
            bail!(ErrorCode::StopRequired)
        }
        if settings.https_cache && !old.https_cache {
            let root = self.root.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                if !certificate::status(&root).trusted {
                    bail!(ErrorCode::CertificateRequired)
                }
                Authority::load(&root)?;
                Ok(())
            })
            .await??;
        }
        let path_changed = route_changed(&old, &settings, password.is_some());
        if settings.mode == Mode::Accelerate && path_changed {
            self.authorization.refresh().await?;
            let ids = self.authorization.available_lines();
            let id = if settings.line_selection == LineSelection::Auto {
                ids.first()
                    .map(|x| x.0.as_str())
                    .ok_or(ErrorCode::AuthLineDenied)?
            } else {
                &settings.selected_line_id
            };
            self.authorization.line(id)?;
        }
        let restart =
            started.is_some() && (path_changed || old.https_cache != settings.https_cache);
        let old_line = self
            .active
            .lock()
            .await
            .as_ref()
            .and_then(|r| r.tunnel.as_ref().map(|t| t.line.clone()));
        let old_password = if password.is_some() {
            Some({
                let vault = self.vault.clone();
                tokio::task::spawn_blocking(move || vault.read()).await??
            })
        } else {
            None
        };
        let metrics = self.metrics.read().unwrap().clone();
        if restart {
            self.stop_inner().await?;
        }
        let change = async {
            if let Some(value) = password {
                let vault = self.vault.clone();
                tokio::task::spawn_blocking(move || vault.write(&value)).await??;
            }
            let next = settings.clone();
            let root = self.root.clone();
            let writer = self.settings_writer.clone();
            tokio::task::spawn_blocking(move || writer.save(&root, &next)).await??;
            *self.settings.write().unwrap() = settings.clone();
            if restart {
                let forced = if !path_changed {
                    old_line.clone()
                } else {
                    None
                };
                self.start_preserving(started.map(|s| (metrics.clone(), s)), forced, true)
                    .await?;
                if path_changed {
                    metrics.reset_line();
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(original) = change {
            let restore = async {
                if restart {
                    self.stop_inner().await?;
                }
                let secret_result = async {
                    if let Some(value) = old_password {
                        let vault = self.vault.clone();
                        tokio::task::spawn_blocking(move || vault.write(&value)).await??;
                    }
                    Ok::<(), anyhow::Error>(())
                }
                .await;
                let next = old.clone();
                let root = self.root.clone();
                let writer = self.settings_writer.clone();
                let settings_result =
                    tokio::task::spawn_blocking(move || writer.save(&root, &next)).await;
                *self.settings.write().unwrap() = old.clone();
                // Attempt both restorations; never retain the new route merely because
                // the credential rollback failed. Restart only after both succeed.
                settings_result??;
                secret_result?;
                *self.selected_status.write().unwrap() = None;
                if restart {
                    self.start_preserving(started.map(|s| (metrics, s)), old_line, true)
                        .await?;
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if restore.is_err() {
                *self.settings.write().unwrap() = old;
                *self.started.write().unwrap() = None;
                let _ = self.stop_inner().await;
                bail!(ErrorCode::SwitchRestoreFailed)
            }
            if restart {
                bail!(ErrorCode::SwitchFailedRestored)
            }
            return Err(original);
        }
        Ok(())
    }
    pub async fn save_preferences(
        &self,
        preferences: crate::preferences::Preferences,
    ) -> Result<()> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        let _guard = self.active.lock().await;
        let mut settings = self.settings.read().unwrap().clone();
        settings.preferences = preferences;
        let root = self.root.clone();
        let next = settings.clone();
        tokio::task::spawn_blocking(move || next.save(&root))
            .await?
            .context(ErrorCode::ConfigWriteFailed)?;
        *self.settings.write().unwrap() = settings;
        Ok(())
    }
    fn ensure_no_audit(&self) -> Result<()> {
        if self.audit.progress.lock().unwrap().running {
            bail!(ErrorCode::CacheMaintenanceBusy);
        }
        Ok(())
    }
    pub async fn save_cache_preferences(
        &self,
        patch: crate::preferences::CachePreferencePatch,
    ) -> Result<crate::preferences::CachePreferences> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        let active = self.active.lock().await;
        let mut settings = self.settings.read().unwrap().clone();
        if let Some(value) = patch.prefetch_enabled {
            settings.cache_preferences.prefetch_enabled = value;
        }
        if let Some(value) = patch.warmup_enabled {
            settings.cache_preferences.warmup_enabled = value;
        }
        let next = settings.clone();
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || next.save(&root))
            .await?
            .context(ErrorCode::ConfigWriteFailed)?;
        *self.settings.write().unwrap() = settings.clone();
        if let Some(running) = &*active {
            running
                .context
                .configure_background(settings.cache_preferences)
                .await;
        }
        Ok(settings.cache_preferences)
    }
    pub async fn start_audit(&self) -> Result<()> {
        let _operation = self.operation().await;
        if self.control_state().shutting_down {
            bail!(ErrorCode::NativeOperationFailed);
        }
        let active = self.active.lock().await;
        if active.is_some() {
            bail!(ErrorCode::StopRequired);
        }
        let mut job = self.audit_job.lock().await;
        self.ensure_no_audit()?;
        if let Some(previous) = job.take() {
            let _ = previous.await;
        }
        self.audit
            .cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
        *self.audit.progress.lock().unwrap() = crate::cache::AuditProgress {
            running: true,
            ..Default::default()
        };
        let cache = self.cache.clone();
        let state = self.audit.clone();
        self.control.send_modify(|state| state.maintenance = true);
        let control = self.control.clone();
        #[cfg(feature = "internal-test")]
        let hold_marker = self.root.join(".internal-audit-hold");
        *job = Some(tokio::spawn(async move {
            let progress = state.clone();
            let result = tokio::task::spawn_blocking(move || {
                #[cfg(feature = "internal-test")]
                {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while hold_marker.exists()
                        && !progress.cancel.load(std::sync::atomic::Ordering::Relaxed)
                        && Instant::now() < deadline
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                cache.audit(&progress)
            })
            .await;
            let mut p = state.progress.lock().unwrap();
            if !matches!(result, Ok(Ok(()))) {
                p.failed += 1;
            }
            p.cancelled = state.cancel.load(std::sync::atomic::Ordering::Relaxed);
            p.running = false;
            control.send_modify(|state| state.maintenance = false);
        }));
        Ok(())
    }
    pub async fn cancel_audit(&self) -> Result<()> {
        let mut handle = self.audit_job.lock().await;
        self.audit
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(job) = handle.take() {
            job.await.context(ErrorCode::CacheAuditFailed)?;
        }
        Ok(())
    }
    pub async fn clear_cache(&self) -> Result<()> {
        let _operation = self.operation().await;
        let active = self.active.lock().await;
        if active.is_some() {
            bail!(ErrorCode::StopRequired);
        }
        self.ensure_no_audit()?;
        let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || cache.clear()).await??;
        Ok(())
    }
    pub async fn manage_certificate(&self, action: &str) -> Result<CertificateStatus> {
        let _operation = self.operation().await;
        let active = self.active.lock().await;
        if active.is_some() {
            bail!(ErrorCode::StopRequired);
        }
        let root = self.root.clone();
        let action = action.to_string();
        let removing = action == "remove";
        let (result, current) = tokio::task::spawn_blocking(move || {
            let result = match action.as_str() {
                "install" => certificate::install(&root),
                "remove" => certificate::remove(&root),
                "check" => certificate::checked_status(&root),
                _ => Err(anyhow::anyhow!(ErrorCode::InvalidCommand)),
            };
            (result, certificate::status(&root))
        })
        .await?;
        // An installation can create the public certificate even when trust was declined.
        *self.certificate.write().unwrap() = current;
        let status = result?;
        if removing {
            let mut settings = self.settings.read().unwrap().clone();
            settings.https_cache = false;
            settings.save(&self.root)?;
            *self.settings.write().unwrap() = settings;
        }
        *self.certificate.write().unwrap() = status.clone();
        Ok(status)
    }
    fn acceleration_status(&self) -> AccelerationStatus {
        self.selected_status
            .read()
            .unwrap()
            .as_ref()
            .unwrap_or(&self.acceleration)
            .read()
            .unwrap()
            .clone()
    }
    async fn open_acceleration(&self, settings: &Settings) -> Result<(Tunnel, Option<u64>)> {
        *self.selected_status.write().unwrap() = None;
        if settings.line_selection == LineSelection::Manual {
            let tunnel = Tunnel::start(
                &self.root,
                self.acceleration_line(&settings.selected_line_id).await?,
                self.acceleration.clone(),
            )
            .await?;
            return Ok((tunnel, None));
        }
        *self.acceleration.write().unwrap() = AccelerationStatus {
            state: acceleration::Phase::Selecting,
            ..Default::default()
        };
        let result = async {
            self.authorization.refresh().await?;
            let lines = self
                .authorization
                .available_lines()
                .into_iter()
                .map(|(id, _)| self.authorization.line(&id))
                .collect::<Result<Vec<_>>>()?;
            crate::selection::select(&self.root, settings, lines).await
        }
        .await;
        match result {
            Ok(selected) => {
                *self.selected_status.write().unwrap() = Some(selected.tunnel.status.clone());
                Ok((
                    selected.tunnel,
                    Some((selected.score.median_micros / 1000) as u64),
                ))
            }
            Err(error) => {
                *self.acceleration.write().unwrap() = AccelerationStatus {
                    state: acceleration::Phase::Error,
                    error: Some(
                        error
                            .downcast_ref::<ErrorCode>()
                            .copied()
                            .unwrap_or(ErrorCode::SshConnectionFailed),
                    ),
                    line_id: None,
                };
                Err(error)
            }
        }
    }
    pub async fn probe(
        &self,
        settings: Settings,
        password: Option<String>,
    ) -> Result<ConnectionProbe> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        let active = self.active.lock().await;
        if active.is_some() {
            bail!(ErrorCode::StopRequired);
        }
        self.ensure_no_audit()?;
        settings.validate()?;
        let (mut tunnel, median) = if settings.mode == Mode::Accelerate {
            let (t, m) = self.open_acceleration(&settings).await?;
            (Some(t), m)
        } else {
            (None, None)
        };
        let selected = self
            .acceleration_status()
            .line_id
            .filter(|_| tunnel.is_some());
        let name = selected.as_ref().and_then(|id| {
            self.authorization
                .available_lines()
                .into_iter()
                .find(|(i, _)| i == id)
                .map(|(_, name)| name)
        });
        let routed = tunnel
            .as_ref()
            .map(|t| t.routed_settings(&settings))
            .unwrap_or(settings);
        let result = if let Some(ms) = median {
            Ok(ms)
        } else {
            Self::probe_routed(routed, password).await
        };
        if let Some(t) = tunnel.as_mut() {
            t.stop().await;
        }
        *self.selected_status.write().unwrap() = None;
        *self.acceleration.write().unwrap() = Default::default();
        result.map(|latency_ms| ConnectionProbe {
            latency_ms,
            line_id: selected,
            line_name: name,
        })
    }

    async fn acceleration_line(&self, id: &str) -> Result<acceleration::Line> {
        let refreshed = self.authorization.refresh().await;
        if let Err(error) = refreshed {
            if error.downcast_ref::<ErrorCode>() != Some(&ErrorCode::AuthUnavailable) {
                return Err(error);
            }
        }
        self.authorization.line(id)
    }
    pub async fn activate_authorization(&self, code: Option<String>) -> Result<()> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        if self.active.lock().await.is_some() {
            bail!(ErrorCode::StopRequired);
        }
        self.authorization.activate_code(code).await
    }
    pub async fn unbind_authorization(&self) -> Result<()> {
        let _operation = self.operation().await;
        self.cancel_proxy_test(None).await;
        if self.active.lock().await.is_some() {
            bail!(ErrorCode::StopRequired);
        }
        self.authorization.unbind().await?;
        Ok(())
    }
    pub async fn authorization_poll(&self) -> Result<()> {
        if !self.authorization.view().configured {
            return Ok(());
        }
        // Network polling is not a foreground operation. Do not hold the lifecycle
        // lock or disable UI/tray controls while waiting for the Control service.
        let result = self.authorization.refresh().await;
        let lock = self.operations.lock().await;
        let current = self.settings.read().unwrap().clone();
        let running = self.started.read().unwrap().is_some();
        let active_id = if running {
            self.acceleration_status().line_id
        } else {
            None
        };
        // Another foreground action may have completed while this poll waited.
        // Decide from the latest atomic authorization snapshot and current route.
        let (phase, available) = self.authorization.route_snapshot();
        let line_denied = route_denied(&current, active_id.as_deref(), &available);
        let denied = phase == crate::authorization::AuthPhase::Revoked
            || (phase == crate::authorization::AuthPhase::Active && line_denied);
        if denied && running && current.mode == Mode::Accelerate {
            self.control.send_modify(|state| state.busy = true);
            let _operation = Operation {
                core: self,
                _lock: lock,
            };
            self.cancel_proxy_test(None).await;
            self.stop_inner().await?;
            *self.acceleration.write().unwrap() = AccelerationStatus {
                state: acceleration::Phase::Error,
                error: Some(ErrorCode::AuthRevoked),
                line_id: active_id,
            };
            tracing::info!("authorization_stopped");
            return Err(ErrorCode::AuthRevoked.into());
        }
        result
    }

    async fn probe_routed(settings: Settings, password: Option<String>) -> Result<u64> {
        settings.validate()?;
        if matches!(settings.mode, Mode::Http | Mode::Socks5) {
            let resolved = tokio::net::lookup_host((
                settings.upstream_host.trim_matches(['[', ']']),
                settings.upstream_port,
            ))
            .await?;
            if resolved
                .into_iter()
                .any(|a| a.ip().is_loopback() && a.port() == settings.listen_port)
            {
                bail!(ErrorCode::ProxyLoop);
            }
        }
        let pass = if let Some(value) = password {
            value
        } else if matches!(settings.mode, Mode::Http | Mode::Socks5)
            && !settings.username.is_empty()
        {
            tokio::task::spawn_blocking(read_password).await??
        } else {
            String::new()
        };
        let client = routing::client(&settings, &pass, true)?;
        let start = Instant::now();
        let result = crate::probe::head(&client, crate::probe::GAME, crate::probe::DEADLINE).await;
        if !matches!(result.sample, ProbeSample::Success(_)) {
            bail!(ErrorCode::ConnectionTestFailed);
        }
        Ok(start.elapsed().as_millis() as u64)
    }
}

fn route_changed(old: &Settings, next: &Settings, password_changed: bool) -> bool {
    old.mode != next.mode
        || (next.mode == Mode::Accelerate
            && (old.line_selection != next.line_selection
                || (next.line_selection == LineSelection::Manual
                    && old.selected_line_id != next.selected_line_id)))
        || (matches!(next.mode, Mode::Http | Mode::Socks5)
            && (password_changed
                || old.upstream_host != next.upstream_host
                || old.upstream_port != next.upstream_port
                || old.username != next.username
                || old.proxy_protocol != next.proxy_protocol))
}

fn route_denied(
    settings: &Settings,
    active_id: Option<&str>,
    available: &[(String, String)],
) -> bool {
    if settings.line_selection == LineSelection::Auto {
        active_id
            .map(|used| !available.iter().any(|(id, _)| id == used))
            .unwrap_or(available.is_empty())
    } else {
        !available
            .iter()
            .any(|(id, _)| id == &settings.selected_line_id)
    }
}

#[cfg(test)]
async fn timed_head(client: &reqwest::Client, url: &str) -> ProbeSample {
    timed_head_with_timeout(client, url, Duration::from_secs(5)).await
}
#[cfg(test)]
async fn timed_head_with_timeout(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> ProbeSample {
    crate::probe::head(client, url, timeout).await.sample
}
