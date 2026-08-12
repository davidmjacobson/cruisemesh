use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    pub root: PathBuf,
    pub identity: PathBuf,
    pub relay: PathBuf,
    pub config: PathBuf,
    pub messages: PathBuf,
    pub logs: PathBuf,
    pub endpoint_cache: PathBuf,
    pub ipc_lock: PathBuf,
    pub avatar: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .context("LOCALAPPDATA is unavailable")?;
        Self::under(local_app_data.join("CruiseMesh"))
    }

    pub fn under(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        let logs = root.join("logs");
        fs::create_dir_all(&logs)
            .with_context(|| format!("failed to create {}", logs.display()))?;
        Ok(Self {
            identity: root.join("identity.dpapi"),
            relay: root.join("relay.json.dpapi"),
            config: root.join("config.json"),
            messages: root.join("messages.db"),
            endpoint_cache: root.join("lan-endpoints.json"),
            ipc_lock: root.join("ipc.lock"),
            avatar: root.join("avatar.jpg"),
            root,
            logs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_stage_one_data_layout() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(temp.path().join("CruiseMesh")).unwrap();
        assert_eq!(paths.identity, paths.root.join("identity.dpapi"));
        assert_eq!(paths.relay, paths.root.join("relay.json.dpapi"));
        assert_eq!(paths.messages, paths.root.join("messages.db"));
        assert!(paths.logs.is_dir());
    }
}
