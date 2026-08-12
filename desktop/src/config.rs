use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::DEFAULT_DISPLAY_NAME;

pub const CURRENT_TERMS_VERSION: &str = "2026-08-08";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    pub display_name: String,
    pub prevent_sleep_on_ac: bool,
    pub share_online: bool,
    pub firewall_prompt_dismissed: bool,
    pub friends_of_friends: bool,
    pub friends_of_friends_revision: u64,
    pub terms_version: Option<String>,
    pub muted_chat_ids: Vec<String>,
    pub avatar_epoch: i64,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            display_name: DEFAULT_DISPLAY_NAME.to_string(),
            prevent_sleep_on_ac: true,
            share_online: true,
            firewall_prompt_dismissed: false,
            friends_of_friends: true,
            friends_of_friends_revision: 0,
            terms_version: None,
            muted_chat_ids: Vec::new(),
            avatar_epoch: 0,
        }
    }
}

impl NodeConfig {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if !path.exists() {
            let value = Self::default();
            value.save(path)?;
            return Ok(value);
        }
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid node config in {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_helper_safe() {
        let config = NodeConfig::default();
        assert_eq!(config.display_name, "Cabin PC");
        assert!(config.prevent_sleep_on_ac);
        assert!(config.share_online);
    }

    #[test]
    fn round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        let config = NodeConfig::load_or_create(&path).unwrap();
        assert_eq!(NodeConfig::load_or_create(&path).unwrap(), config);
    }
}
