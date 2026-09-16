use super::*;

const WRITE_ITEMS: usize = 32;
const WRITE_BYTES: usize = 64 * 1024 * 1024;

impl Cache {
    /// Publish a validated response without waiting for filesystem or SQLite locks.
    /// The write table pins bodies and accounts for queued AND executing work.
    pub(crate) fn stage(&self, key: CacheKey, entry: Cached, download: bool) -> Option<u64> {
        if entry.body.is_empty() || entry.body.len() > MAX_OBJECT || !entry.policy.is_storable() {
            return None;
        }
        let policy = serde_json::to_string(&entry.policy).ok()?;
        if policy_location(&policy, &key.hash) != Some(key.language.directory()) {
            return None;
        }
        let mut ram = self.ram.lock().unwrap();
        let queued: usize = ram.writes.values().map(|(_, e)| e.body.len()).sum();
        if ram.writes.len() >= WRITE_ITEMS || queued + entry.body.len() > WRITE_BYTES {
            return None;
        }
        if let Some(old) = ram.ram.pop(&key) {
            ram.ram_size -= old.body.len();
        }
        while ram.ram_size + queued + entry.body.len() > RAM_LIMIT
            || ram.ram.len() + ram.writes.len() >= ram.ram.cap().get()
        {
            let (_, old) = ram.ram.pop_lru()?;
            ram.ram_size -= old.body.len();
        }
        ram.write_sequence = ram.write_sequence.checked_add(1)?;
        let id = ram.write_sequence;
        ram.pending.remove(&key);
        // A 304 can supersede a not-yet-written download. Carry its single credit.
        let inherited = ram
            .latest_write
            .get(&key)
            .copied()
            .is_some_and(|old| ram.write_downloads.remove(&old));
        if download || inherited {
            ram.write_downloads.insert(id);
        }
        ram.latest_write.insert(key.clone(), id);
        ram.writes.insert(id, (key, entry));
        Some(id)
    }

    /// Runs on one bounded background writer; cancelled/replaced generations cannot revive data.
    pub(crate) fn persist(&self, id: u64, limit: u64) -> Result<Option<bool>> {
        let mut store = self.inner.lock().unwrap();
        let (key, entry, download) = {
            let mut ram = self.ram.lock().unwrap();
            let Some((key, entry)) = ram.writes.get(&id).cloned() else {
                return Ok(None);
            };
            if ram.latest_write.get(&key) != Some(&id) {
                ram.writes.remove(&id);
                ram.write_downloads.remove(&id);
                return Ok(None);
            }
            let download = ram.write_downloads.remove(&id);
            (key, entry, download)
        };
        let saved = (|| -> Result<()> {
            let policy = serde_json::to_string(&entry.policy)?;
            crate::config_store::atomic_write(&key.path(&self.root, "body"), &entry.body)?;
            let accessed = store.tick();
            store.db.execute("INSERT OR REPLACE INTO entries (key,policy,size,digest,accessed,language) VALUES (?1,?2,?3,?4,?5,?6)",params![key.hash,policy,entry.body.len(),digest(&entry.body),accessed,key.language.directory()])?;
            Ok(())
        })();
        let current = {
            let mut ram = self.ram.lock().unwrap();
            ram.writes.remove(&id);
            if saved.is_err() && download {
                if let Some(next) = ram
                    .latest_write
                    .get(&key)
                    .copied()
                    .filter(|next| *next != id)
                {
                    ram.write_downloads.insert(next);
                }
            }
            if ram.latest_write.get(&key) == Some(&id) {
                ram.latest_write.remove(&key);
                true
            } else {
                false
            }
        };
        // A failed disk write does not invalidate already validated, usable RAM data.
        if current {
            Self::remember(&mut store, &key, entry)?;
        }
        saved?;
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
        Ok(Some(download))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(uri: &str, text: &'static [u8]) -> Cached {
        let req = Request::builder().uri(uri).body(()).unwrap();
        let res = Response::builder()
            .header("cache-control", "public,max-age=600")
            .body(())
            .unwrap();
        Cached {
            policy: policy(&req, &res).unwrap(),
            body: Bytes::from_static(text),
        }
    }
    #[test]
    fn delayed_writes_cannot_revive_removed_replaced_or_cleared_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js";
        let key = CacheKey::from_url(uri).unwrap();
        let first = cache.stage(key.clone(), entry(uri, b"old"), true).unwrap();
        let second = cache.stage(key.clone(), entry(uri, b"new"), true).unwrap();
        assert!(cache.persist(first, 1024).unwrap().is_none());
        assert_eq!(cache.get_ram(&key).unwrap().body, "new");
        assert!(cache.persist(second, 1024).unwrap().is_some());
        let removed = cache
            .stage(key.clone(), entry(uri, b"removed"), true)
            .unwrap();
        cache.remove(&key).unwrap();
        assert!(cache.persist(removed, 1024).unwrap().is_none());
        let cleared = cache
            .stage(key.clone(), entry(uri, b"cleared"), true)
            .unwrap();
        cache.clear().unwrap();
        assert!(cache.persist(cleared, 1024).unwrap().is_none());
        assert!(cache.get(&key).unwrap().is_none());
        drop(cache);
        assert!(Cache::open(dir.path())
            .unwrap()
            .get(&key)
            .unwrap()
            .is_none());
    }
    #[test]
    fn queue_bounds_and_disk_failure_keep_valid_response_available() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js";
        let key = CacheKey::from_url(uri).unwrap();
        let mut tickets = Vec::new();
        for _ in 0..WRITE_ITEMS {
            tickets.push(
                cache
                    .stage(key.clone(), entry(uri, b"const x=1;"), true)
                    .unwrap(),
            );
        }
        assert!(cache
            .stage(key.clone(), entry(uri, b"overflow"), true)
            .is_none());
        for id in tickets.iter().take(WRITE_ITEMS - 1) {
            assert!(cache.persist(*id, 1024).unwrap().is_none());
        }
        fs::create_dir(key.path(dir.path(), "body")).unwrap();
        assert!(cache.persist(*tickets.last().unwrap(), 1024).is_err());
        assert_eq!(cache.get_ram(&key).unwrap().body, "const x=1;");
        assert!(cache.ram.lock().unwrap().writes.is_empty());
        fs::remove_dir(key.path(dir.path(), "body")).unwrap();
        cache.clear().unwrap();
        assert!(cache.get_ram(&key).is_none());
    }
    #[test]
    fn byte_budget_includes_active_writes_and_ram() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js";
        let key = CacheKey::from_url(uri).unwrap();
        let mut value = entry(uri, b"x");
        value.body = Bytes::from(vec![b'x'; MAX_OBJECT]);
        for _ in 0..4 {
            assert!(cache.stage(key.clone(), value.clone(), true).is_some());
        }
        assert!(cache.stage(key, value, true).is_none());
        let ram = cache.ram.lock().unwrap();
        assert!(
            ram.ram_size
                + ram
                    .writes
                    .values()
                    .map(|(_, v)| v.body.len())
                    .sum::<usize>()
                <= RAM_LIMIT
        );
    }
    #[test]
    fn staged_response_keeps_vary_and_quota_rules() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js";
        let key = CacheKey::from_url(uri).unwrap();
        let req = Request::builder()
            .uri(uri)
            .header("accept-encoding", "identity")
            .body(())
            .unwrap();
        let res = Response::builder()
            .header("cache-control", "public,max-age=600")
            .header("vary", "accept-encoding")
            .body(())
            .unwrap();
        let ticket = cache
            .stage(
                key.clone(),
                Cached {
                    policy: policy(&req, &res).unwrap(),
                    body: Bytes::from_static(b"const x=1;"),
                },
                true,
            )
            .unwrap();
        let entry = cache.get_ram(&key).unwrap();
        assert!(fresh(&entry, &req).is_some());
        let different = Request::builder()
            .uri(uri)
            .header("accept-encoding", "gzip")
            .body(())
            .unwrap();
        assert!(fresh(&entry, &different).is_none());
        assert!(cache.persist(ticket, 1).unwrap().is_some());
        assert_eq!(cache.usage().unwrap(), 0);
        assert!(cache.get_ram(&key).is_none());
    }
    #[test]
    fn metadata_refresh_preserves_one_unpersisted_download_credit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js";
        let key = CacheKey::from_url(uri).unwrap();
        let body = entry(uri, b"const x=1;");
        let downloaded = cache.stage(key.clone(), body.clone(), true).unwrap();
        let revalidated = cache.stage(key.clone(), body.clone(), false).unwrap();
        assert_eq!(cache.persist(downloaded, 1024).unwrap(), None);
        assert_eq!(cache.persist(revalidated, 1024).unwrap(), Some(true));
        let next = cache.stage(key, body, false).unwrap();
        assert_eq!(cache.persist(next, 1024).unwrap(), Some(false));
    }
}
