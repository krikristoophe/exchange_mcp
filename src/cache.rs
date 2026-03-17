//! In-memory cache for IMAP data with per-key TTL.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::imap::client::{EmailDetail, EmailSummary, FolderInfo, FolderStatus};

struct CacheEntry<V> {
    value: V,
    expires_at: Instant,
}

impl<V: Clone> CacheEntry<V> {
    fn new(value: V, ttl: Duration) -> Self {
        Self {
            value,
            expires_at: Instant::now() + ttl,
        }
    }

    fn is_valid(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

/// Per-user email cache. Shared via Arc inside ImapClient.
pub struct EmailCache {
    /// Folder list cache
    folders: RwLock<Option<CacheEntry<Vec<FolderInfo>>>>,
    /// Email summaries per folder key
    summaries: RwLock<HashMap<String, CacheEntry<Vec<EmailSummary>>>>,
    /// Email details per (folder, uid)
    details: RwLock<HashMap<(String, u32), CacheEntry<EmailDetail>>>,
    /// Folder status per folder
    status: RwLock<HashMap<String, CacheEntry<FolderStatus>>>,

    ttl_folders: Duration,
    ttl_summaries: Duration,
    ttl_details: Duration,
    ttl_status: Duration,
}

impl EmailCache {
    pub fn new() -> Self {
        Self {
            folders: RwLock::new(None),
            summaries: RwLock::new(HashMap::new()),
            details: RwLock::new(HashMap::new()),
            status: RwLock::new(HashMap::new()),
            ttl_folders: Duration::from_secs(300),    // 5 minutes
            ttl_summaries: Duration::from_secs(120),   // 2 minutes
            ttl_details: Duration::from_secs(600),     // 10 minutes
            ttl_status: Duration::from_secs(60),       // 1 minute
        }
    }

    // -- Folders --

    pub fn get_folders(&self) -> Option<Vec<FolderInfo>> {
        let guard = self.folders.read().ok()?;
        guard.as_ref().and_then(|e| {
            if e.is_valid() {
                Some(e.value.clone())
            } else {
                None
            }
        })
    }

    pub fn set_folders(&self, folders: Vec<FolderInfo>) {
        if let Ok(mut guard) = self.folders.write() {
            *guard = Some(CacheEntry::new(folders, self.ttl_folders));
        }
    }

    // -- Email summaries --

    /// Cache key for summaries includes folder + limit to avoid returning
    /// wrong-sized results.
    pub fn get_summaries(&self, folder: &str, limit: u32) -> Option<Vec<EmailSummary>> {
        let key = format!("{folder}:{limit}");
        let guard = self.summaries.read().ok()?;
        guard.get(&key).and_then(|e| {
            if e.is_valid() {
                Some(e.value.clone())
            } else {
                None
            }
        })
    }

    pub fn set_summaries(&self, folder: &str, limit: u32, summaries: Vec<EmailSummary>) {
        let key = format!("{folder}:{limit}");
        if let Ok(mut guard) = self.summaries.write() {
            guard.insert(key, CacheEntry::new(summaries, self.ttl_summaries));
        }
    }

    // -- Search results (keyed by folder + query + limit) --

    pub fn get_search(&self, folder: &str, query: &str, limit: u32) -> Option<Vec<EmailSummary>> {
        let key = format!("search:{folder}:{query}:{limit}");
        let guard = self.summaries.read().ok()?;
        guard.get(&key).and_then(|e| {
            if e.is_valid() {
                Some(e.value.clone())
            } else {
                None
            }
        })
    }

    pub fn set_search(
        &self,
        folder: &str,
        query: &str,
        limit: u32,
        summaries: Vec<EmailSummary>,
    ) {
        let key = format!("search:{folder}:{query}:{limit}");
        if let Ok(mut guard) = self.summaries.write() {
            guard.insert(key, CacheEntry::new(summaries, self.ttl_summaries));
        }
    }

    // -- Email details --

    pub fn get_detail(&self, folder: &str, uid: u32) -> Option<EmailDetail> {
        let guard = self.details.read().ok()?;
        guard.get(&(folder.to_string(), uid)).and_then(|e| {
            if e.is_valid() {
                Some(e.value.clone())
            } else {
                None
            }
        })
    }

    pub fn set_detail(&self, folder: &str, uid: u32, detail: EmailDetail) {
        if let Ok(mut guard) = self.details.write() {
            guard.insert(
                (folder.to_string(), uid),
                CacheEntry::new(detail, self.ttl_details),
            );
        }
    }

    // -- Folder status --

    pub fn get_status(&self, folder: &str) -> Option<FolderStatus> {
        let guard = self.status.read().ok()?;
        guard.get(folder).and_then(|e| {
            if e.is_valid() {
                Some(e.value.clone())
            } else {
                None
            }
        })
    }

    pub fn set_status(&self, folder: &str, status: FolderStatus) {
        if let Ok(mut guard) = self.status.write() {
            guard.insert(folder.to_string(), CacheEntry::new(status, self.ttl_status));
        }
    }

    // -- Invalidation --

    /// Invalidate all cached data for a folder (after move, delete, flag changes).
    pub fn invalidate_folder(&self, folder: &str) {
        if let Ok(mut guard) = self.summaries.write() {
            guard.retain(|k, _| {
                // List keys: "{folder}:{limit}"
                // Search keys: "search:{folder}:{query}:{limit}"
                let is_list_key = k.starts_with(&format!("{folder}:"));
                let is_search_key = k.starts_with(&format!("search:{folder}:"));
                !is_list_key && !is_search_key
            });
        }
        if let Ok(mut guard) = self.details.write() {
            guard.retain(|(f, _), _| f != folder);
        }
        if let Ok(mut guard) = self.status.write() {
            guard.remove(folder);
        }
    }

    /// Invalidate the cached folder list (after create/rename/delete folder).
    pub fn invalidate_folders_list(&self) {
        if let Ok(mut guard) = self.folders.write() {
            *guard = None;
        }
    }

    /// Invalidate a specific email detail (after flag change).
    pub fn invalidate_detail(&self, folder: &str, uid: u32) {
        if let Ok(mut guard) = self.details.write() {
            guard.remove(&(folder.to_string(), uid));
        }
    }
}
