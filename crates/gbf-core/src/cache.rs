use crate::rules::{self, GameLanguage};
mod content;
mod persistence;
use anyhow::Result;
use bytes::Bytes;
use http::{Request, Response};
use http_cache_semantics::{BeforeRequest, CacheOptions, CachePolicy};
use lru::LruCache;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_OBJECT: usize = 16 * 1024 * 1024;
const RAM_LIMIT: usize = 128 * 1024 * 1024;
const PARTITIONS: [&str; 2] = ["ja", "en"];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    language: GameLanguage,
    hash: String,
}
impl CacheKey {
    pub fn from_url(uri: &str) -> Option<Self> {
        let url = url::Url::parse(uri).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || !rules::is_asset(url.host_str()?)
            || !rules::cache_path(url.path())
        {
            return None;
        }
        Some(Self {
            language: GameLanguage::from_path(url.path())?,
            hash: digest(uri.as_bytes()),
        })
    }
    fn path(&self, root: &Path, extension: &str) -> PathBuf {
        root.join(self.language.directory())
            .join(format!("{}.{extension}", self.hash))
    }
}

#[derive(Clone)]
pub struct Cached {
    pub policy: CachePolicy,
    pub body: Bytes,
}
struct Ram {
    ram: LruCache<CacheKey, Cached>,
    ram_size: usize,
    clock: i64,
    pending: HashMap<CacheKey, i64>,
    writes: HashMap<u64, (CacheKey, Cached)>,
    latest_write: HashMap<CacheKey, u64>,
    write_sequence: u64,
    write_downloads: HashSet<u64>,
}
struct Store {
    db: Connection,
    ram: Arc<Mutex<Ram>>,
}
impl Store {
    fn tick(&mut self) -> i64 {
        let mut ram = self.ram.lock().unwrap();
        ram.clock = now().max(ram.clock.saturating_add(1));
        ram.clock
    }
}
pub struct Cache {
    root: PathBuf,
    inner: Mutex<Store>,
    ram: Arc<Mutex<Ram>>,
}
pub fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
/// Metadata deliberately reads the pinned CachePolicy serialization, retaining the full policy.
/// Unknown URIs are disposable; only Japanese and English assets are retained.
fn policy_location(policy: &str, hash: &str) -> Option<&'static str> {
    serde_json::from_str::<CachePolicy>(policy).ok()?;
    let value: serde_json::Value = serde_json::from_str(policy).ok()?;
    let uri = value.get("uri")?.as_str()?;
    if digest(uri.as_bytes()) != hash {
        return None;
    }
    CacheKey::from_url(uri).map(|key| key.language.directory())
}

fn clean_orphans(root: &Path, db: &Connection) -> Result<()> {
    for partition in PARTITIONS {
        let directory = root.join(partition);
        for file in fs::read_dir(&directory)? {
            let path = file?.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if let Some(hash) = name.strip_suffix(".pending") {
                if valid_hash(hash) {
                    remove_file(&path)?;
                }
            } else if let Some(hash) = name.strip_suffix(".body") {
                if !valid_hash(hash) {
                    continue;
                }
                let exists: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM entries WHERE key=?1 AND language=?2)",
                    params![hash, partition],
                    |r| r.get(0),
                )?;
                if !exists {
                    remove_file(&path)?;
                }
            }
        }
    }
    Ok(())
}

impl Cache {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        for partition in PARTITIONS {
            let dir = root.join(partition);
            if fs::symlink_metadata(&dir).is_ok_and(|m| crate::data_directory::is_link(&m)) {
                anyhow::bail!("cache partition cannot be a symbolic link");
            }
            fs::create_dir_all(dir)?;
        }
        let db = Connection::open(root.join("index.sqlite3"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS entries (key TEXT PRIMARY KEY, policy TEXT NOT NULL, size INTEGER NOT NULL, digest TEXT NOT NULL, accessed INTEGER NOT NULL, language TEXT NOT NULL CHECK(language IN ('ja','en')));")?;
        let columns: Vec<String> = db
            .prepare("PRAGMA table_info(entries)")?
            .query_map([], |r| r.get(1))?
            .collect::<Result<_, _>>()?;
        anyhow::ensure!(
            columns.iter().any(|c| c == "language"),
            "Unsupported cache schema"
        );
        let unsupported: i64 = db.query_row(
            "SELECT count(*) FROM entries WHERE language NOT IN ('ja','en')",
            [],
            |r| r.get(0),
        )?;
        anyhow::ensure!(unsupported == 0, "Unsupported cache partition");
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS entries_lru ON entries(accessed); PRAGMA user_version=1;",
        )?;
        clean_orphans(root, &db)?;
        let clock = db.query_row("SELECT COALESCE(MAX(accessed),0) FROM entries", [], |r| {
            r.get(0)
        })?;
        let ram = Arc::new(Mutex::new(Ram {
            ram: LruCache::new(NonZeroUsize::new(2048).unwrap()),
            ram_size: 0,
            clock,
            pending: HashMap::new(),
            writes: HashMap::new(),
            latest_write: HashMap::new(),
            write_sequence: 0,
            write_downloads: HashSet::new(),
        }));
        Ok(Self {
            root: root.into(),
            inner: Mutex::new(Store {
                db,
                ram: ram.clone(),
            }),
            ram,
        })
    }
    #[cfg(test)]
    pub(crate) fn evict_ram(&self) {
        let mut ram = self.ram.lock().unwrap();
        ram.ram.clear();
        ram.ram_size = 0;
    }
    #[cfg(test)]
    pub(crate) fn hold_disk(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        let _store = self.inner.lock().unwrap();
        entered.send(()).unwrap();
        release.recv().unwrap();
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Never acquires the disk lock or performs filesystem/SQLite work.
    pub fn get_ram(&self, key: &CacheKey) -> Option<Cached> {
        let mut ram = self.ram.lock().unwrap();
        if let Some(entry) = ram
            .latest_write
            .get(key)
            .and_then(|id| ram.writes.get(id))
            .map(|(_, entry)| entry.clone())
        {
            return Some(entry);
        }
        let entry = ram.ram.get(key).cloned()?;
        ram.clock = now().max(ram.clock.saturating_add(1));
        let accessed = ram.clock;
        ram.pending.insert(key.clone(), accessed);
        Some(entry)
    }
    pub fn flush_accesses(&self) -> Result<()> {
        Self::flush_locked(&mut self.inner.lock().unwrap())
    }
    fn flush_locked(store: &mut Store) -> Result<()> {
        // Keep entries until commit succeeds. Concurrent hits only replace timestamps.
        let pending = store.ram.lock().unwrap().pending.clone();
        if pending.is_empty() {
            return Ok(());
        }
        let tx = store.db.transaction()?;
        {
            let mut statement = tx.prepare_cached(
                "UPDATE entries SET accessed=MAX(accessed,?3) WHERE key=?1 AND language=?2",
            )?;
            for (key, accessed) in &pending {
                statement.execute(params![key.hash, key.language.directory(), accessed])?;
            }
        }
        tx.commit()?;
        let mut ram = store.ram.lock().unwrap();
        for (key, accessed) in pending {
            if ram.pending.get(&key) == Some(&accessed) {
                ram.pending.remove(&key);
            }
        }
        Ok(())
    }
    pub fn get(&self, key: &CacheKey) -> Result<Option<Cached>> {
        if let Some(entry) = self.get_ram(key) {
            return Ok(Some(entry));
        }
        self.get_disk(key)
    }
    pub fn get_disk(&self, key: &CacheKey) -> Result<Option<Cached>> {
        let mut store = self.inner.lock().unwrap();
        let row: Option<(String, usize, String)> = store
            .db
            .query_row(
                "SELECT policy,size,digest FROM entries WHERE key=?1 AND language=?2",
                params![key.hash, key.language.directory()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((policy, size, hash)) = row else {
            return Ok(None);
        };
        let path = key.path(&self.root, "body");
        let valid = fs::symlink_metadata(&path)
            .is_ok_and(|m| m.is_file() && m.len() == size as u64 && size <= MAX_OBJECT);
        let bytes = if valid {
            fs::read(path).unwrap_or_default()
        } else {
            Vec::new()
        };
        let valid_content = content::valid_policy_body(&policy, &bytes);
        let policy = serde_json::from_str::<CachePolicy>(&policy);
        if bytes.len() != size
            || bytes.is_empty()
            || digest(&bytes) != hash
            || policy.is_err()
            || !valid_content
        {
            Self::remove_locked(&self.root, &mut store, &key.hash, key.language.directory())?;
            return Ok(None);
        }
        let entry = Cached {
            policy: policy?,
            body: Bytes::from(bytes),
        };
        Self::remember(&mut store, key, entry.clone())?;
        self.get_ram(key);
        Ok(Some(entry))
    }
    pub fn put(&self, key: &CacheKey, entry: Cached, limit: u64) -> Result<()> {
        if entry.body.is_empty() || entry.body.len() > MAX_OBJECT || !entry.policy.is_storable() {
            return Ok(());
        }
        let policy = serde_json::to_string(&entry.policy)?;
        if policy_location(&policy, &key.hash) != Some(key.language.directory()) {
            anyhow::bail!("cache key and policy do not match");
        }
        let mut store = self.inner.lock().unwrap();
        {
            let mut ram = store.ram.lock().unwrap();
            ram.latest_write.remove(key);
            ram.pending.remove(key);
            if let Some(old) = ram.ram.pop(key) {
                ram.ram_size -= old.body.len();
            }
        }
        let target = key.path(&self.root, "body");
        crate::config_store::atomic_write(&target, &entry.body)?;
        let accessed = store.tick();
        store.db.execute("INSERT OR REPLACE INTO entries (key,policy,size,digest,accessed,language) VALUES (?1,?2,?3,?4,?5,?6)",params![key.hash,policy,entry.body.len(),digest(&entry.body),accessed,key.language.directory()])?;
        Self::remember(&mut store, key, entry)?;
        if Self::usage_locked(&store)? > limit {
            Self::flush_locked(&mut store)?;
        }
        while Self::usage_locked(&store)? > limit {
            let (hash, partition): (String, String) = store.db.query_row(
                "SELECT key,language FROM entries ORDER BY accessed ASC,key ASC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Self::remove_locked(&self.root, &mut store, &hash, &partition)?;
        }
        Ok(())
    }
    fn remember(store: &mut Store, key: &CacheKey, entry: Cached) -> Result<()> {
        // At most 2048 pending evicted keys plus 2048 resident keys. Admission
        // backpressure belongs to disk work, never the RAM response path.
        if store.ram.lock().unwrap().pending.len() >= 2048 {
            Self::flush_locked(store)?;
        }
        let mut store = store.ram.lock().unwrap();
        if let Some(old) = store.ram.pop(key) {
            store.ram_size -= old.body.len();
        }
        let queued_bytes: usize = store
            .writes
            .values()
            .map(|(_, entry)| entry.body.len())
            .sum();
        while store.ram_size + queued_bytes + entry.body.len() > RAM_LIMIT
            || store.ram.len() + store.writes.len() >= store.ram.cap().get()
        {
            if let Some((_, old)) = store.ram.pop_lru() {
                store.ram_size -= old.body.len();
            } else {
                break;
            }
        }
        store.ram_size += entry.body.len();
        store.ram.put(key.clone(), entry);
        Ok(())
    }
    fn usage_locked(store: &Store) -> Result<u64> {
        Ok(store
            .db
            .query_row("SELECT COALESCE(SUM(size),0) FROM entries", [], |r| {
                r.get(0)
            })?)
    }
    pub fn usage(&self) -> Result<u64> {
        Self::usage_locked(&self.inner.lock().unwrap())
    }
    fn remove_locked(root: &Path, store: &mut Store, hash: &str, partition: &str) -> Result<()> {
        if !valid_hash(hash) || !PARTITIONS.contains(&partition) {
            anyhow::bail!("invalid cache index location");
        }
        remove_file(&root.join(partition).join(format!("{hash}.body")))?;
        store.db.execute(
            "DELETE FROM entries WHERE key=?1 AND language=?2",
            params![hash, partition],
        )?;
        let language = match partition {
            "ja" => Some(GameLanguage::Japanese),
            "en" => Some(GameLanguage::English),
            _ => None,
        };
        if let Some(language) = language {
            let mut store = store.ram.lock().unwrap();
            store.latest_write.remove(&CacheKey {
                language,
                hash: hash.into(),
            });
            store.pending.remove(&CacheKey {
                language,
                hash: hash.into(),
            });
            if let Some(old) = store.ram.pop(&CacheKey {
                language,
                hash: hash.into(),
            }) {
                store.ram_size -= old.body.len();
            }
        }
        Ok(())
    }
    pub fn remove(&self, key: &CacheKey) -> Result<()> {
        Self::remove_locked(
            &self.root,
            &mut self.inner.lock().unwrap(),
            &key.hash,
            key.language.directory(),
        )
    }
    pub fn clear(&self) -> Result<()> {
        let mut store = self.inner.lock().unwrap();
        // Invalidate even entries that have not reached SQLite yet.
        {
            let mut ram = store.ram.lock().unwrap();
            ram.latest_write.clear();
            ram.ram.clear();
            ram.ram_size = 0;
            ram.pending.clear();
        }
        let keys: Vec<(String, String)> = store
            .db
            .prepare("SELECT key,language FROM entries")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (hash, partition) in keys {
            Self::remove_locked(&self.root, &mut store, &hash, &partition)?;
        }
        clean_orphans(&self.root, &store.db)?;
        Ok(())
    }
}
pub fn eligible(req: &Request<()>) -> bool {
    let h = req.headers();
    req.method() == http::Method::GET
        && ![
            "authorization",
            "cookie",
            "range",
            "if-none-match",
            "if-modified-since",
            "if-match",
            "if-unmodified-since",
        ]
        .iter()
        .any(|k| h.contains_key(*k))
        && !h.get_all("cache-control").iter().any(|v| {
            v.to_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .split(',')
                .any(|d| d.trim() == "no-store")
        })
}
pub fn policy(req: &Request<()>, res: &Response<()>) -> Option<CachePolicy> {
    if res.status() != http::StatusCode::OK || res.headers().contains_key("set-cookie") {
        return None;
    }
    let p = CachePolicy::new_options(
        req,
        res,
        SystemTime::now(),
        CacheOptions {
            shared: true,
            cache_heuristic: 0.0,
            ..Default::default()
        },
    );
    p.is_storable().then_some(p)
}
// CachePolicy's serialized representation is already the persisted cache format.
// Read its original response headers without changing freshness or Vary semantics.
pub(crate) fn stored_response_headers(policy: &CachePolicy) -> Result<http::HeaderMap> {
    #[derive(serde::Deserialize)]
    struct Stored {
        #[serde(with = "http_serde::header_map")]
        res: http::HeaderMap,
    }
    Ok(serde_json::from_value::<Stored>(serde_json::to_value(policy)?)?.res)
}

pub fn fresh(entry: &Cached, req: &Request<()>) -> Option<http::response::Parts> {
    match entry.policy.before_request(req, SystemTime::now()) {
        BeforeRequest::Fresh(parts) => Some(parts),
        _ => None,
    }
}

pub fn valid_content(path: &str, headers: &http::HeaderMap, body: &[u8]) -> bool {
    content::validate(path, headers, body)
}
fn valid_decoded_content(path: &str, headers: &http::HeaderMap, body: &[u8]) -> bool {
    if body.is_empty() {
        return false;
    }
    let mime = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if mime.starts_with("text/html") {
        return false;
    }
    let path = path.to_ascii_lowercase();
    if path.ends_with(".png") {
        return body.starts_with(b"\x89PNG\r\n\x1a\n");
    }
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        return body.starts_with(b"\xff\xd8\xff");
    }
    if path.ends_with(".gif") {
        return body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a");
    }
    if path.ends_with(".webp") {
        return body.starts_with(b"RIFF") && body.get(8..12) == Some(b"WEBP");
    }
    if path.ends_with(".woff") {
        return body.starts_with(b"wOFF");
    }
    if path.ends_with(".woff2") {
        return body.starts_with(b"wOF2");
    }
    if path.ends_with(".ogg") {
        return body.starts_with(b"OggS");
    }
    let prefix = String::from_utf8_lossy(&body[..body.len().min(128)])
        .trim_start_matches('\u{feff}')
        .trim_start()
        .to_ascii_lowercase();
    !prefix.starts_with("<!doctype html") && !prefix.starts_with("<html")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ram_hits_batch_sqlite_writes_and_never_wait_for_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::open(dir.path()).unwrap());
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js";
        let key = CacheKey::from_url(uri).unwrap();
        cache.put(&key, item(uri, b"asset", false), 100).unwrap();
        let before = cache.inner.lock().unwrap().db.total_changes();
        for _ in 0..1000 {
            assert!(cache.get_ram(&key).is_some());
        }
        assert_eq!(cache.inner.lock().unwrap().db.total_changes(), before);
        assert_eq!(cache.ram.lock().unwrap().pending.len(), 1);
        let locked = cache.inner.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let c = cache.clone();
        let k = key.clone();
        let worker = std::thread::spawn(move || {
            tx.send(c.get_ram(&k).is_some()).unwrap();
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(2));
        drop(locked);
        worker.join().unwrap();
        assert!(result.unwrap());
        cache.flush_accesses().unwrap();
        assert_eq!(cache.inner.lock().unwrap().db.total_changes(), before + 1);
        cache.get_ram(&key);
        cache.remove(&key).unwrap();
        cache.flush_accesses().unwrap();
        drop(cache);
        assert!(Cache::open(dir.path())
            .unwrap()
            .get(&key)
            .unwrap()
            .is_none());
    }
    #[test]
    fn admission_flushes_bounded_recency_under_pressure() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        // Exercise the actual admission/touch buffer without thousands of fsyncs.
        for n in 0..5000 {
            let uri = format!("https://prd-game-a-granbluefantasy.akamaized.net/assets/{n}.js");
            let key = CacheKey::from_url(&uri).unwrap();
            Cache::remember(
                &mut cache.inner.lock().unwrap(),
                &key,
                item(&uri, b"x", false),
            )
            .unwrap();
            cache.get_ram(&key);
            assert!(cache.ram.lock().unwrap().pending.len() <= 4096);
        }
        cache.flush_accesses().unwrap();
        assert!(cache.ram.lock().unwrap().pending.is_empty());
        // UPDATE-only flushing cannot invent a missing disk entry.
        assert_eq!(cache.usage().unwrap(), 0);
    }
    #[test]
    fn replacement_and_quota_eviction_use_pending_recency() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let a = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js";
        let b = "https://prd-game-a-granbluefantasy.akamaized.net/assets/b.js";
        let c = "https://prd-game-a-granbluefantasy.akamaized.net/assets/c.js";
        let ka = CacheKey::from_url(a).unwrap();
        let kb = CacheKey::from_url(b).unwrap();
        cache.put(&ka, item(a, b"a", false), 2).unwrap();
        cache.put(&kb, item(b, b"b", false), 2).unwrap();
        cache.get_ram(&ka);
        cache
            .put(&CacheKey::from_url(c).unwrap(), item(c, b"c", false), 2)
            .unwrap();
        assert!(cache.get(&kb).unwrap().is_none());
        cache.put(&ka, item(a, b"z", false), 2).unwrap();
        cache.flush_accesses().unwrap();
        cache.clear().unwrap();
        cache.flush_accesses().unwrap();
        assert_eq!(cache.usage().unwrap(), 0);
        assert!(cache.get_ram(&ka).is_none());
    }
    #[test]
    fn keys_keep_language_version_query_and_host_boundaries() {
        let a =
            CacheKey::from_url("https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js?v=1")
                .unwrap();
        let b = CacheKey::from_url(
            "https://prd-game-a-granbluefantasy.akamaized.net/assets_en/a.js?v=1",
        )
        .unwrap();
        assert_ne!(a, b);
        assert_eq!(a.language, GameLanguage::Japanese);
        assert_eq!(b.language, GameLanguage::English);
        assert_ne!(
            a,
            CacheKey::from_url("https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js?v=2")
                .unwrap()
        );
        assert_ne!(
            a,
            CacheKey::from_url(
                "https://prd-game-a-granbluefantasy-steam.akamaized.net/assets/a.js?v=1"
            )
            .unwrap()
        );
        for uri in [
            "https://prd-game-a-granbluefantasy.akamaized.net/assets_enough/a.js",
            "https://prd-game-a-granbluefantasy.akamaized.net/assets_ja/a.js",
            "https://game.granbluefantasy.jp/assets_en/a.js",
        ] {
            assert!(CacheKey::from_url(uri).is_none());
        }
    }
    fn item(url: &str, body: &'static [u8], vary: bool) -> Cached {
        let req = Request::builder()
            .uri(url)
            .header("accept-language", "zh")
            .body(())
            .unwrap();
        let mut res = Response::builder().header("cache-control", "public, max-age=3600");
        if vary {
            res = res.header("vary", "accept-language");
        }
        Cached {
            policy: policy(&req, &res.body(()).unwrap()).unwrap(),
            body: Bytes::from_static(body),
        }
    }
    #[test]
    fn persistent_cache_validates_and_evicts() {
        let dir = tempfile::tempdir().unwrap();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js?v=1";
        let key = CacheKey::from_url(uri).unwrap();
        let c = Cache::open(dir.path()).unwrap();
        c.put(&key, item(uri, b"abc", false), 10).unwrap();
        drop(c);
        let c = Cache::open(dir.path()).unwrap();
        assert_eq!(c.get(&key).unwrap().unwrap().body, "abc");
        c.put(
            &CacheKey::from_url("https://prd-game-a-granbluefantasy.akamaized.net/assets_en/b.js")
                .unwrap(),
            item(
                "https://prd-game-a-granbluefantasy.akamaized.net/assets_en/b.js",
                b"defghijk",
                false,
            ),
            10,
        )
        .unwrap();
        assert!(c.get(&key).unwrap().is_none());
        drop(c);
        fs::write(
            CacheKey::from_url("https://prd-game-a-granbluefantasy.akamaized.net/assets_en/b.js")
                .unwrap()
                .path(dir.path(), "body"),
            b"bad",
        )
        .unwrap();
        let c = Cache::open(dir.path()).unwrap();
        assert!(c
            .get(
                &CacheKey::from_url(
                    "https://prd-game-a-granbluefantasy.akamaized.net/assets_en/b.js"
                )
                .unwrap()
            )
            .unwrap()
            .is_none());
    }
    #[test]
    fn private_cookie_range_and_vary() {
        let req = Request::builder().uri("https://a/a").body(()).unwrap();
        for name in ["cookie", "authorization", "range"] {
            let mut r = req.clone();
            r.headers_mut().insert(name, "x".parse().unwrap());
            assert!(!eligible(&r));
        }
        assert!(policy(
            &req,
            &Response::builder()
                .header("cache-control", "private,max-age=60")
                .body(())
                .unwrap()
        )
        .is_none());
        assert!(policy(
            &req,
            &Response::builder()
                .header("set-cookie", "x=y")
                .body(())
                .unwrap()
        )
        .is_none());
        let e = item("https://a/a", b"abc", true);
        assert!(fresh(&e, &req).is_none());
        let zh = Request::builder()
            .uri("https://a/a")
            .header("accept-language", "zh")
            .body(())
            .unwrap();
        assert!(fresh(&e, &zh).is_some());
    }
    #[test]
    fn reject_error_pages_and_bad_magic() {
        let headers = http::HeaderMap::new();
        assert!(!valid_content("/assets/a.png", &headers, b"error"));
        assert!(!valid_content(
            "/assets/a.js",
            &headers,
            b"<!doctype html><h1>error</h1>"
        ));
        assert!(valid_content(
            "/assets/a.png",
            &headers,
            b"\x89PNG\r\n\x1a\nremaining"
        ));
    }
}
