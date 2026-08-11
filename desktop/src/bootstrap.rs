use std::{fs, sync::Arc};

use anyhow::{bail, Context, Result};
use cruisemesh_core::{
    friend_card_user_id, make_friend_card, make_friend_link, parse_friend_text,
    parse_relay_setup_text, relay_token_is_deposit, Contact, Identity, MessageStore, RelaySetup,
};
use serde::{Deserialize, Serialize};

use crate::{
    config::NodeConfig, identity_store::IdentityStore, platform::dpapi, store_paths::AppPaths,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub relay_url: String,
    pub member_token: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BootstrapStatus {
    pub display_name: String,
    pub user_id: String,
    pub relay_configured: bool,
    pub contacts: usize,
    pub reduced_mode: bool,
}

pub struct BootstrapStore {
    paths: AppPaths,
    identity: Identity,
    config: NodeConfig,
    store: Arc<MessageStore>,
}

impl BootstrapStore {
    pub fn open(paths: AppPaths) -> Result<Self> {
        let config = NodeConfig::load_or_create(&paths.config)?;
        let identity = IdentityStore::new(paths.identity.clone()).load_or_create()?;
        let store = Arc::new(MessageStore::open(
            paths.messages.to_string_lossy().into_owned(),
        )?);
        Ok(Self {
            paths,
            identity,
            config,
            store,
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn store(&self) -> Arc<MessageStore> {
        Arc::clone(&self.store)
    }

    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    pub fn relay_config(&self) -> Result<Option<RelayConfig>> {
        if !self.paths.relay.exists() {
            return Ok(None);
        }
        let protected = fs::read(&self.paths.relay)
            .with_context(|| format!("failed to read {}", self.paths.relay.display()))?;
        let plaintext = dpapi::unprotect(&protected)?;
        let config: RelayConfig = serde_json::from_slice(&plaintext)
            .context("saved Shore Pass configuration is invalid")?;
        if relay_token_is_deposit(config.member_token.clone()) {
            bail!("saved Shore Pass contains a deposit token, not a member token");
        }
        Ok(Some(config))
    }

    pub fn import_relay_setup(&self, text: &str) -> Result<RelaySetup> {
        let setup = parse_relay_setup_text(text.to_string())?;
        if relay_token_is_deposit(setup.relay_token.clone()) {
            bail!("a Shore Pass must contain a member token");
        }
        let config = RelayConfig {
            relay_url: setup.relay_url.clone(),
            member_token: setup.relay_token.clone(),
        };
        let plaintext = serde_json::to_vec(&config)?;
        let protected = dpapi::protect(&plaintext)?;
        fs::write(&self.paths.relay, protected)
            .with_context(|| format!("failed to write {}", self.paths.relay.display()))?;
        Ok(setup)
    }

    pub fn friend_card_json(&self) -> Result<String> {
        let relay = self.relay_config()?;
        let (url, token) = relay
            .map(|value| (Some(value.relay_url), Some(value.member_token)))
            .unwrap_or((None, None));
        make_friend_card(
            self.config.display_name.clone(),
            self.identity.clone(),
            url,
            token,
        )
        .map_err(Into::into)
    }

    pub fn friend_link(&self) -> Result<String> {
        Ok(make_friend_link(self.friend_card_json()?)?)
    }

    pub fn import_friend(&self, text: &str) -> Result<Contact> {
        let card = parse_friend_text(text.to_string())?;
        let user_id = friend_card_user_id(card.clone());
        if user_id == self.identity.user_id {
            bail!("cannot import this helper's own friend card");
        }
        let contact = Contact {
            user_id,
            name: card.name,
            sign_pk: card.sign_pk,
            agree_pk: card.agree_pk,
            relay_url: card.relay_url,
            relay_token: card.relay_token,
            nickname: None,
        };
        Ok(self.store.upsert_imported_contact(contact)?)
    }

    pub fn status(&self) -> Result<BootstrapStatus> {
        let relay_configured = self.relay_config()?.is_some();
        Ok(BootstrapStatus {
            display_name: self.config.display_name.clone(),
            user_id: cruisemesh_core::format_user_id(self.identity.user_id.clone()),
            relay_configured,
            contacts: self.store.list_contacts()?.len(),
            reduced_mode: !relay_configured,
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use cruisemesh_core::{
        generate_identity, make_relay_setup_card, parse_friend_text, relay_token_is_deposit,
    };

    fn bootstrap() -> (tempfile::TempDir, BootstrapStore) {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(temp.path().join("CruiseMesh")).unwrap();
        let store = BootstrapStore::open(paths).unwrap();
        (temp, store)
    }

    #[test]
    fn identity_is_stable_across_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(temp.path().join("CruiseMesh")).unwrap();
        let first = BootstrapStore::open(paths.clone()).unwrap();
        let first_id = first.identity.user_id.clone();
        drop(first);
        let second = BootstrapStore::open(paths).unwrap();
        assert_eq!(second.identity.user_id, first_id);
    }

    #[test]
    fn emitted_card_attenuates_the_member_token() {
        let (_temp, store) = bootstrap();
        let setup = make_relay_setup_card(
            "https://relay.example".into(),
            "family-member-secret".into(),
        )
        .unwrap();
        store.import_relay_setup(&setup).unwrap();

        let card = parse_friend_text(store.friend_link().unwrap()).unwrap();
        let shared = card.relay_token.unwrap();
        assert!(relay_token_is_deposit(shared.clone()));
        assert_ne!(shared, "family-member-secret");
        assert_eq!(
            store.relay_config().unwrap().unwrap().member_token,
            "family-member-secret"
        );
    }

    #[test]
    fn offline_friend_import_is_mutual_bootstrap_half() {
        let (_temp, store) = bootstrap();
        let phone = generate_identity();
        let json = make_friend_card("Emma".into(), phone.clone(), None, None).unwrap();
        let link = make_friend_link(json).unwrap();
        let imported = store.import_friend(&link).unwrap();
        assert_eq!(imported.user_id, phone.user_id);
        assert_eq!(store.store.list_contacts().unwrap().len(), 1);
    }
}
