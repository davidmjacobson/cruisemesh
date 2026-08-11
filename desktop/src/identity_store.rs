use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use cruisemesh_core::{decode_identity_bytes, encode_identity_bytes, generate_identity, Identity};

#[cfg(windows)]
use crate::platform::dpapi;

#[derive(Clone, Debug)]
pub struct IdentityStore {
    path: PathBuf,
}

impl IdentityStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(windows)]
    pub fn load_or_create(&self) -> Result<Identity> {
        if self.path.exists() {
            let protected = fs::read(&self.path)
                .with_context(|| format!("failed to read {}", self.path.display()))?;
            let encoded = dpapi::unprotect(&protected)?;
            return decode_identity_bytes(encoded).context("saved helper identity is invalid");
        }

        let identity = generate_identity();
        self.save(&identity)?;
        Ok(identity)
    }

    #[cfg(windows)]
    pub fn save(&self, identity: &Identity) -> Result<()> {
        let encoded = encode_identity_bytes(identity.clone());
        let protected = dpapi::protect(&encoded)?;
        fs::write(&self.path, protected)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }

    #[cfg(not(windows))]
    pub fn load_or_create(&self) -> Result<Identity> {
        anyhow::bail!("DPAPI identity storage is only available on Windows")
    }

    #[cfg(not(windows))]
    pub fn save(&self, _identity: &Identity) -> Result<()> {
        anyhow::bail!("DPAPI identity storage is only available on Windows")
    }
}
