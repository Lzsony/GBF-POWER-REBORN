use super::*;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(serde::Deserialize)]
struct PolicyHeaders {
    #[serde(with = "http_serde::header_map")]
    req: http::HeaderMap,
    #[serde(with = "http_serde::header_map")]
    res: http::HeaderMap,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn insert(cache: &Cache, uri: &str, data: &'static [u8], max_age: u32) -> CacheKey {
        let key = CacheKey::from_url(uri).unwrap();
        let request = Request::builder()
            .uri(uri)
            .header("accept-encoding", "identity")
            .body(())
            .unwrap();
        let response = Response::builder()
            .header("cache-control", format!("public, max-age={max_age}"))
            .header("vary", "accept-encoding")
            .body(())
            .unwrap();
        cache
            .put(
                &key,
                Cached {
                    policy: super::super::policy(&request, &response).unwrap(),
                    body: Bytes::from_static(data),
                },
                1024 * 1024,
            )
            .unwrap();
        key
    }
    #[test]
    fn audit_repairs_corruption_keeps_expired_and_unknown_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let good = insert(
            &cache,
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/good.js",
            b"good",
            0,
        );
        let bad = insert(
            &cache,
            "https://prd-game-a-granbluefantasy.akamaized.net/assets_en/bad.js",
            b"bad",
            600,
        );
        fs::write(bad.path(dir.path(), "body"), b"damaged").unwrap();
        let unknown_uri = "https://prd-game-a-granbluefantasy.akamaized.net/old/a.js";
        let hash = digest(unknown_uri.as_bytes());
        let policy = CachePolicy::new(
            &Request::builder().uri(unknown_uri).body(()).unwrap(),
            &Response::builder()
                .header("cache-control", "public, max-age=600")
                .body(())
                .unwrap(),
        );
        {
            let store = cache.inner.lock().unwrap();
            store
                .db
                .execute(
                    "INSERT INTO entries VALUES(?1,?2,3,?3,0,'ja')",
                    params![
                        hash,
                        serde_json::to_string(&policy).unwrap(),
                        digest(b"old")
                    ],
                )
                .unwrap();
        }
        fs::write(dir.path().join("ja").join(format!("{hash}.body")), b"old").unwrap();
        fs::write(dir.path().join("ja/notes.txt"), b"keep").unwrap();
        fs::write(
            dir.path()
                .join("ja")
                .join(format!("{}.body", digest(b"orphan"))),
            b"orphan",
        )
        .unwrap();
        fs::write(
            dir.path()
                .join("en")
                .join(format!("{}.pending", digest(b"pending"))),
            b"partial",
        )
        .unwrap();
        let audit = AuditState::default();
        cache.audit(&audit).unwrap();
        let p = audit.progress.lock().unwrap().clone();
        assert_eq!(p.checked, 3);
        assert_eq!(p.repaired, 4);
        assert_eq!(p.failed, 0);
        assert!(cache.get(&good).unwrap().is_some());
        assert!(cache.get(&bad).unwrap().is_none());
        assert!(!dir.path().join("ja").join(format!("{hash}.body")).exists());
        assert!(dir.path().join("ja/notes.txt").exists());
        assert_eq!(cache.usage().unwrap(), 4);
        let second = AuditState::default();
        cache.audit(&second).unwrap();
        assert_eq!(second.progress.lock().unwrap().repaired, 0);
    }
    #[test]
    fn audit_cancel_is_resumable_and_symlink_target_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let bad = insert(
            &cache,
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js",
            b"bad",
            600,
        );
        fs::remove_file(bad.path(dir.path(), "body")).unwrap();
        let target = dir.path().join("untouched.txt");
        fs::write(&target, b"safe").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, bad.path(dir.path(), "body")).unwrap();
        let state = AuditState::default();
        state.cancel.store(true, Ordering::Relaxed);
        cache.audit(&state).unwrap();
        assert_eq!(state.progress.lock().unwrap().checked, 0);
        state.cancel.store(false, Ordering::Relaxed);
        cache.audit(&state).unwrap();
        assert_eq!(fs::read(target).unwrap(), b"safe");
        assert_eq!(state.progress.lock().unwrap().repaired, 1);
    }
    #[tokio::test]
    async fn warmup_reads_only_disk_without_changing_usage_statistics_or_access_order() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::open(dir.path()).unwrap());
        let key = insert(
            &cache,
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/warm.js",
            b"asset",
            600,
        );
        cache.evict_ram();
        let before: i64 = cache
            .inner
            .lock()
            .unwrap()
            .db
            .query_row("SELECT accessed FROM entries", [], |r| r.get(0))
            .unwrap();
        let metrics = Arc::new(crate::metrics::Metrics::default());
        cache
            .clone()
            .warm(metrics.clone(), tokio_util::sync::CancellationToken::new())
            .await;
        let after: i64 = cache
            .inner
            .lock()
            .unwrap()
            .db
            .query_row("SELECT accessed FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after);
        assert!(cache.get_ram(&key).is_some());
        assert_eq!(cache.usage().unwrap(), 5);
        let stats = metrics.snapshot(None);
        assert_eq!(stats.requests, 0);
        assert_eq!(stats.downloads, 0);
        assert_eq!(stats.prefetched, 0);
        assert_eq!(stats.received, 0);
        assert_eq!(stats.sent, 0);
        assert_eq!(stats.hit_rate, None);
    }

    #[test]
    fn warmup_preserves_lru_respects_freshness_budget_and_concurrent_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let good = insert(
            &cache,
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js",
            b"good",
            600,
        );
        let stale = insert(
            &cache,
            "https://prd-game-a-granbluefantasy.akamaized.net/assets_en/b.js",
            b"stale",
            0,
        );
        let before: Vec<i64> = {
            let store = cache.inner.lock().unwrap();
            store.ram.lock().unwrap().ram.clear();
            store.ram.lock().unwrap().ram_size = 0;
            let rows = store
                .db
                .prepare("SELECT accessed FROM entries ORDER BY key")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            rows
        };
        // A previously touched, then RAM-evicted entry keeps its pending recency
        // when warmup readmits it; warmup must neither invent nor erase accesses.
        cache.ram.lock().unwrap().pending.insert(good.clone(), 123);
        let records = cache.warmup_candidates().unwrap();
        assert_eq!(records.len(), 2);
        let row = records
            .iter()
            .find(|r| r.hash == good.hash)
            .unwrap()
            .clone();
        assert_eq!(cache.warm_one(row.clone(), 3), 0);
        assert_eq!(cache.warm_one(row, 4), 4);
        assert_eq!(cache.ram.lock().unwrap().pending.get(&good), Some(&123));
        assert_eq!(
            cache.warm_one(
                records
                    .iter()
                    .find(|r| r.hash == stale.hash)
                    .unwrap()
                    .clone(),
                100
            ),
            0
        );
        let after: Vec<i64> = cache
            .inner
            .lock()
            .unwrap()
            .db
            .prepare("SELECT accessed FROM entries ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(before, after);
        let old = records
            .iter()
            .find(|r| r.hash == good.hash)
            .unwrap()
            .clone();
        insert(
            &cache,
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js",
            b"updated",
            600,
        );
        {
            let store = cache.inner.lock().unwrap();
            store.ram.lock().unwrap().ram.clear();
            store.ram.lock().unwrap().ram_size = 0;
        }
        assert_eq!(cache.warm_one(old, 100), 0);
        let store = cache.inner.lock().unwrap();
        assert_eq!(store.ram.lock().unwrap().ram_size, 0);
    }
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditProgress {
    pub running: bool,
    pub cancelled: bool,
    pub checked: u64,
    pub repaired: u64,
    pub failed: u64,
}
#[derive(Default)]
pub struct AuditState {
    pub progress: Mutex<AuditProgress>,
    pub cancel: AtomicBool,
}

#[derive(Clone)]
pub(super) struct DiskRecord {
    rowid: i64,
    hash: String,
    language: String,
    policy: String,
    size: usize,
    digest: String,
}
impl Cache {
    fn records(&self, warmup: bool) -> Result<Vec<DiskRecord>> {
        let store = self.inner.lock().unwrap();
        let sql = if warmup {
            "SELECT rowid,key,language,policy,size,digest FROM entries WHERE language IN ('ja','en') AND accessed>=?1 AND size<=1048576 ORDER BY accessed DESC"
        } else {
            "SELECT rowid,key,language,policy,size,digest FROM entries WHERE ?1>=0 ORDER BY key"
        };
        let mut statement = store.db.prepare(sql)?;
        let records = statement
            .query_map(
                [if warmup {
                    now() - 7 * 24 * 60 * 60 * 1000
                } else {
                    0
                }],
                |r| {
                    Ok(DiskRecord {
                        rowid: r.get(0)?,
                        hash: r.get(1).unwrap_or_default(),
                        language: r.get(2).unwrap_or_default(),
                        policy: r.get(3).unwrap_or_default(),
                        size: r.get(4).unwrap_or_default(),
                        digest: r.get(5).unwrap_or_default(),
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }
    fn record_path(&self, record: &DiskRecord) -> Option<PathBuf> {
        if !valid_hash(&record.hash) || !PARTITIONS.contains(&record.language.as_str()) {
            return None;
        }
        let directory = self.root.join(&record.language);
        if crate::data_directory::is_link(&fs::symlink_metadata(&directory).ok()?) {
            return None;
        }
        Some(directory.join(format!("{}.body", record.hash)))
    }
    fn read_record(&self, record: &DiskRecord) -> Option<Cached> {
        let path = self.record_path(record)?;
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.is_file()
            || record.size == 0
            || record.size > MAX_OBJECT
            || meta.len() != record.size as u64
        {
            return None;
        }
        if policy_location(&record.policy, &record.hash)? != record.language {
            return None;
        }
        let bytes = fs::read(&path).ok()?;
        if bytes.len() != record.size || digest(&bytes) != record.digest {
            return None;
        }
        let policy: CachePolicy = serde_json::from_str(&record.policy).ok()?;
        let value: serde_json::Value = serde_json::from_str(&record.policy).ok()?;
        let url = url::Url::parse(value.get("uri")?.as_str()?).ok()?;
        let headers = serde_json::from_str::<PolicyHeaders>(&record.policy)
            .ok()?
            .res;
        if !valid_content(url.path(), &headers, &bytes) {
            return None;
        }
        Some(Cached {
            policy,
            body: Bytes::from(bytes),
        })
    }
    pub fn audit(&self, state: &AuditState) -> Result<()> {
        for record in self.records(false)? {
            if state.cancel.load(Ordering::Relaxed) {
                break;
            }
            let valid = self.read_record(&record).is_some();
            let result = if valid {
                Ok(())
            } else {
                let mut store = self.inner.lock().unwrap();
                let unchanged = store.db.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE rowid=?1 AND key=?2 AND language=?3 AND policy=?4 AND digest=?5 AND size=?6)", params![record.rowid,record.hash,record.language,record.policy,record.digest,record.size], |r| r.get::<_,bool>(0))?;
                if !unchanged {
                    continue;
                }
                // Invalid metadata must never be interpreted as a filesystem path.
                if self.record_path(&record).is_some() {
                    Self::remove_locked(&self.root, &mut store, &record.hash, &record.language)
                } else {
                    let language = match record.language.as_str() {
                        "ja" => Some(GameLanguage::Japanese),
                        "en" => Some(GameLanguage::English),
                        _ => None,
                    };
                    if let Some(language) = language {
                        let mut store = store.ram.lock().unwrap();
                        store.latest_write.remove(&CacheKey {
                            language,
                            hash: record.hash.clone(),
                        });
                        store.pending.remove(&CacheKey {
                            language,
                            hash: record.hash.clone(),
                        });
                        if let Some(old) = store.ram.pop(&CacheKey {
                            language,
                            hash: record.hash.clone(),
                        }) {
                            store.ram_size -= old.body.len();
                        }
                    }
                    store
                        .db
                        .execute("DELETE FROM entries WHERE rowid=?1", [record.rowid])
                        .map(|_| ())
                        .map_err(Into::into)
                }
            };
            let mut progress = state.progress.lock().unwrap();
            progress.checked += 1;
            if result.is_err() {
                progress.failed += 1;
            } else if !valid {
                progress.repaired += 1;
            }
        }
        if !state.cancel.load(Ordering::Relaxed) {
            for partition in ["", "ja", "en"] {
                let directory = self.root.join(partition);
                if crate::data_directory::is_link(&fs::symlink_metadata(&directory)?) {
                    continue;
                }
                for entry in fs::read_dir(directory)? {
                    if state.cancel.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    let entry = entry?;
                    if !entry.file_type()?.is_file() {
                        continue;
                    }
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    let (hash, pending) = if let Some(h) = name.strip_suffix(".pending") {
                        (h, true)
                    } else if let Some(h) = name.strip_suffix(".body") {
                        (h, false)
                    } else {
                        continue;
                    };
                    if !valid_hash(hash) {
                        continue;
                    }
                    let store = self.inner.lock().unwrap();
                    let exists: bool = store.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM entries WHERE key=?1 AND language=?2)",
                        params![hash, partition],
                        |r| r.get(0),
                    )?;
                    if pending || !exists {
                        let result = remove_file(&entry.path());
                        let mut progress = state.progress.lock().unwrap();
                        if result.is_ok() {
                            progress.repaired += 1;
                        } else {
                            progress.failed += 1;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn warmup_candidates(&self) -> Result<Vec<DiskRecord>> {
        let mut records: Vec<_> = self
            .records(true)?
            .into_iter()
            .filter_map(|r| {
                let value: serde_json::Value = serde_json::from_str(&r.policy).ok()?;
                let url = url::Url::parse(value.get("uri")?.as_str()?).ok()?;
                let path = url.path();
                let priority = if [".js", ".css"].iter().any(|s| path.ends_with(s)) {
                    0
                } else if [".woff", ".woff2", ".ttf"]
                    .iter()
                    .any(|s| path.ends_with(s))
                {
                    1
                } else if [".png", ".jpg", ".jpeg", ".gif", ".webp"]
                    .iter()
                    .any(|s| path.ends_with(s))
                {
                    2
                } else {
                    return None;
                };
                Some((priority, r))
            })
            .collect();
        records.sort_by_key(|(priority, _)| *priority);
        Ok(records.into_iter().take(512).map(|(_, r)| r).collect())
    }
    pub(super) fn warm_one(&self, record: DiskRecord, remaining: usize) -> usize {
        if record.size > remaining {
            return 0;
        }
        let Some(entry) = self.read_record(&record) else {
            let mut store = self.inner.lock().unwrap();
            let unchanged = store.db.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE rowid=?1 AND key=?2 AND language=?3 AND policy=?4 AND digest=?5 AND size=?6)", params![record.rowid,record.hash,record.language,record.policy,record.digest,record.size], |r| r.get::<_,bool>(0)).unwrap_or(false);
            let replacement_pending =
                store.ram.lock().unwrap().latest_write.keys().any(|key| {
                    key.hash == record.hash && key.language.directory() == record.language
                });
            if unchanged && !replacement_pending && self.record_path(&record).is_some() {
                let _ = Self::remove_locked(&self.root, &mut store, &record.hash, &record.language);
            }
            return 0;
        };
        let value: serde_json::Value = serde_json::from_str(&record.policy).unwrap();
        let Some(uri) = value.get("uri").and_then(|v| v.as_str()) else {
            return 0;
        };
        let Some(key) = CacheKey::from_url(uri) else {
            return 0;
        };
        let Ok(mut request) = Request::builder().uri(uri).body(()) else {
            return 0;
        };
        let Ok(headers) = serde_json::from_str::<PolicyHeaders>(&record.policy) else {
            return 0;
        };
        *request.headers_mut() = headers.req;
        if !matches!(
            entry.policy.before_request(&request, SystemTime::now()),
            BeforeRequest::Fresh(_)
        ) {
            return 0;
        }
        let mut store = self.inner.lock().unwrap();
        let unchanged = store.db.query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE key=?1 AND language=?2 AND policy=?3 AND digest=?4 AND size=?5)",params![record.hash,record.language,record.policy,record.digest,record.size],|r| r.get::<_,bool>(0)).unwrap_or(false);
        let resident = {
            let ram = store.ram.lock().unwrap();
            ram.ram.contains(&key) || ram.latest_write.contains_key(&key)
        };
        if !unchanged || resident {
            return 0;
        }
        if Self::remember(&mut store, &key, entry).is_err() {
            return 0;
        }
        record.size
    }
    pub async fn warm(
        self: Arc<Self>,
        metrics: Arc<crate::metrics::Metrics>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        let cache = self.clone();
        let Ok(Ok(records)) = tokio::task::spawn_blocking(move || cache.warmup_candidates()).await
        else {
            return;
        };
        let mut remaining = 32 * 1024 * 1024;
        for record in records {
            while metrics.foreground_busy() && !cancel.is_cancelled() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            if cancel.is_cancelled() || remaining == 0 {
                break;
            }
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                let cache = self.clone();
                let metrics = metrics.clone();
                let record = record.clone();
                // Recheck when the blocking worker actually runs, retaining deferred work.
                match tokio::task::spawn_blocking(move || {
                    if metrics.foreground_busy() {
                        None
                    } else {
                        Some(cache.warm_one(record, remaining))
                    }
                })
                .await
                {
                    Ok(Some(size)) => {
                        remaining -= size;
                        break;
                    }
                    Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                    Err(_) => break,
                }
            }
        }
    }
}
