use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use cruisemesh_core::{
    lan_endpoint_cache_is_fresh, lan_endpoint_host_is_local, LanEndpointContent,
};
use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedEndpoint {
    pub contact_user_id: Vec<u8>,
    pub network_id: Vec<u8>,
    pub instance_token: Vec<u8>,
    pub host: String,
    pub port: u16,
    pub expires_at_ms: i64,
    pub saved_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFile {
    entries: Vec<CachedEndpoint>,
}

#[derive(Clone, Debug)]
pub struct EndpointCache {
    path: PathBuf,
}

impl EndpointCache {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn record(
        &self,
        contact_user_id: Vec<u8>,
        content: LanEndpointContent,
        now_ms: i64,
    ) -> Result<bool> {
        if !lan_endpoint_host_is_local(content.host.clone()) || content.expires_at_ms <= now_ms {
            return Ok(false);
        }
        let mut cache = self.load()?;
        cache.entries.retain(|entry| {
            entry.expires_at_ms > now_ms
                && !(entry.contact_user_id == contact_user_id
                    && entry.network_id == content.network_id)
        });
        cache.entries.push(CachedEndpoint {
            contact_user_id,
            network_id: content.network_id,
            instance_token: content.instance_token,
            host: content.host,
            port: content.port,
            expires_at_ms: content.expires_at_ms,
            saved_at_ms: now_ms,
        });
        cache
            .entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.saved_at_ms));
        cache.entries.truncate(MAX_ENTRIES);
        self.save(&cache)?;
        Ok(true)
    }

    pub fn fresh_for_contact(
        &self,
        contact_user_id: &[u8],
        now_ms: i64,
    ) -> Result<Vec<CachedEndpoint>> {
        let mut entries: Vec<_> = self
            .load()?
            .entries
            .into_iter()
            .filter(|entry| {
                entry.contact_user_id == contact_user_id
                    && entry.expires_at_ms > now_ms
                    && lan_endpoint_cache_is_fresh(entry.saved_at_ms, now_ms)
            })
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.saved_at_ms));
        Ok(entries)
    }

    fn load(&self) -> Result<CacheFile> {
        if !self.path.exists() {
            return Ok(CacheFile::default());
        }
        let bytes = fs::read(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid endpoint cache in {}", self.path.display()))
    }

    fn save(&self, cache: &CacheFile) -> Result<()> {
        let bytes = serde_json::to_vec(cache)?;
        fs::write(&self.path, bytes)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(host: &str, expires_at_ms: i64) -> LanEndpointContent {
        LanEndpointContent {
            instance_token: vec![1; 16],
            network_id: vec![2; 16],
            host: host.into(),
            port: 45_892,
            expires_at_ms,
        }
    }

    #[test]
    fn rejects_public_and_expired_hints() {
        let temp = tempfile::tempdir().unwrap();
        let cache = EndpointCache::new(temp.path().join("cache.json"));
        assert!(!cache
            .record(vec![3; 16], content("8.8.8.8", 200), 100)
            .unwrap());
        assert!(!cache
            .record(vec![3; 16], content("192.168.1.2", 100), 100)
            .unwrap());
        assert!(!cache.path.exists());
    }

    #[test]
    fn replaces_a_contacts_hint_on_the_same_network() {
        let temp = tempfile::tempdir().unwrap();
        let cache = EndpointCache::new(temp.path().join("cache.json"));
        cache
            .record(vec![3; 16], content("192.168.1.2", 500), 100)
            .unwrap();
        cache
            .record(vec![3; 16], content("192.168.1.3", 600), 200)
            .unwrap();
        let entries = cache.fresh_for_contact(&[3; 16], 250).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].host, "192.168.1.3");
    }
}
