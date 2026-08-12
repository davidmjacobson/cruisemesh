use std::{
    fs,
    sync::{Arc, RwLock},
};

use anyhow::{bail, Context, Result};
use cruisemesh_core::{
    fingerprint_words, friend_card_user_id, make_friend_card, make_friend_link,
    parse_friend_import, parse_relay_setup_text, relay_token_is_deposit, shared_card_expired,
    Contact, CoreRelayPassHealth, FriendImport, Identity, MessageStore, RelaySetup,
    SharedFriendCard,
};
use serde::{Deserialize, Serialize};

use crate::{
    config::{NodeConfig, CURRENT_TERMS_VERSION},
    identity_store::IdentityStore,
    platform::dpapi,
    store_paths::AppPaths,
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

#[derive(Clone, Debug, Serialize)]
pub struct ShorePassStatus {
    pub configured: bool,
    pub state: String,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FriendPreview {
    pub name: String,
    pub fingerprint_words: Vec<String>,
    pub already_known: bool,
    pub shared: bool,
    pub expired: bool,
    pub sharer_name: Option<String>,
}

pub struct BootstrapStore {
    paths: AppPaths,
    identity: Identity,
    config: RwLock<NodeConfig>,
    store: Arc<MessageStore>,
    relay_health: RwLock<Option<CoreRelayPassHealth>>,
}

impl BootstrapStore {
    pub fn open(paths: AppPaths) -> Result<Self> {
        crate::backup::apply_pending_restore(&paths)?;
        let config = NodeConfig::load_or_create(&paths.config)?;
        let identity = IdentityStore::new(paths.identity.clone()).load_or_create()?;
        let store = Arc::new(MessageStore::open(
            paths.messages.to_string_lossy().into_owned(),
        )?);
        Ok(Self {
            paths,
            identity,
            config: RwLock::new(config),
            store,
            relay_health: RwLock::new(None),
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn store(&self) -> Arc<MessageStore> {
        Arc::clone(&self.store)
    }

    pub fn config(&self) -> NodeConfig {
        self.config
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn update_display_name(&self, display_name: String) -> Result<NodeConfig> {
        let display_name = display_name.trim().to_string();
        if display_name.is_empty() {
            bail!("display name cannot be empty");
        }
        make_friend_card(display_name.clone(), self.identity.clone(), None, None)?;
        let mut next = self.config();
        next.display_name = display_name;
        self.save_config(next)
    }

    pub fn update_preferences(
        &self,
        prevent_sleep_on_ac: bool,
        share_online: bool,
        friends_of_friends: Option<bool>,
    ) -> Result<NodeConfig> {
        let mut next = self.config();
        next.prevent_sleep_on_ac = prevent_sleep_on_ac;
        next.share_online = share_online;
        if let Some(enabled) = friends_of_friends {
            if enabled != next.friends_of_friends {
                next.friends_of_friends = enabled;
                next.friends_of_friends_revision =
                    next.friends_of_friends_revision.saturating_add(1);
            }
        }
        self.save_config(next)
    }

    pub fn accept_terms(&self) -> Result<NodeConfig> {
        let mut next = self.config();
        next.terms_version = Some(CURRENT_TERMS_VERSION.to_string());
        self.save_config(next)
    }

    pub fn set_muted(&self, conversation_id: &str, muted: bool) -> Result<NodeConfig> {
        let mut next = self.config();
        next.muted_chat_ids.retain(|id| id != conversation_id);
        if muted {
            next.muted_chat_ids.push(conversation_id.to_string());
        }
        self.save_config(next)
    }

    pub fn is_muted(&self, conversation_id: &str) -> bool {
        self.config()
            .muted_chat_ids
            .iter()
            .any(|id| id == conversation_id)
    }

    pub fn terms_accepted(&self) -> bool {
        self.config().terms_version.as_deref() == Some(CURRENT_TERMS_VERSION)
    }

    pub fn record_relay_health(&self, health: CoreRelayPassHealth) {
        *self
            .relay_health
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(health);
    }

    pub fn shore_pass_status(&self) -> Result<ShorePassStatus> {
        if self.relay_config()?.is_none() {
            return Ok(ShorePassStatus {
                configured: false,
                state: "not_set_up".into(),
                title: "Shore Pass not set up".into(),
                detail: "Add a Shore Pass for internet delivery when people are not nearby.".into(),
            });
        }
        Ok(match *self.relay_health.read().unwrap_or_else(|p| p.into_inner()) {
            None => ShorePassStatus {
                configured: true,
                state: "checking".into(),
                title: "Checking Shore Pass".into(),
                detail: "CruiseMesh is checking internet delivery.".into(),
            },
            Some(CoreRelayPassHealth::Ok) => ShorePassStatus {
                configured: true,
                state: "ready".into(),
                title: "Shore Pass is ready".into(),
                detail: "Internet delivery is working.".into(),
            },
            Some(CoreRelayPassHealth::QuotaFull) => ShorePassStatus {
                configured: true,
                state: "storage_full".into(),
                title: "Shore Pass storage is full".into(),
                detail: "Internet delivery is paused until your family collects waiting messages.".into(),
            },
            Some(CoreRelayPassHealth::MessageTooLarge) => ShorePassStatus {
                configured: true,
                state: "message_too_large".into(),
                title: "A message is too large to send".into(),
                detail: "One message cannot go over internet delivery. Other messages still deliver.".into(),
            },
            Some(CoreRelayPassHealth::RateLimited) => ShorePassStatus {
                configured: true,
                state: "slowed".into(),
                title: "Shore Pass is catching up".into(),
                detail: "Syncing is slowed right now. It recovers on its own.".into(),
            },
            Some(CoreRelayPassHealth::Expired) => ShorePassStatus {
                configured: true,
                state: "expired".into(),
                title: "This Shore Pass has expired".into(),
                detail: "Renew it, then open the new setup link.".into(),
            },
            Some(CoreRelayPassHealth::Suspended) => ShorePassStatus {
                configured: true,
                state: "suspended".into(),
                title: "This Shore Pass is suspended".into(),
                detail: "Contact support for help.".into(),
            },
            Some(CoreRelayPassHealth::TokenRejected) => ShorePassStatus {
                configured: true,
                state: "rejected".into(),
                title: "Shore Pass setup was rejected".into(),
                detail: "Check this setup against another family phone.".into(),
            },
            Some(CoreRelayPassHealth::Failing) => ShorePassStatus {
                configured: true,
                state: "unreachable".into(),
                title: "Shore Pass is unreachable".into(),
                detail: "CruiseMesh could not reach Shore Pass. Try again later.".into(),
            },
        })
    }

    pub fn load_avatar_bytes(&self) -> Vec<u8> {
        fs::read(&self.paths.avatar).unwrap_or_default()
    }

    pub fn save_avatar_bytes(&self, bytes: &[u8]) -> Result<NodeConfig> {
        if bytes.is_empty() {
            let _ = fs::remove_file(&self.paths.avatar);
        } else {
            fs::write(&self.paths.avatar, bytes)
                .with_context(|| format!("failed to write {}", self.paths.avatar.display()))?;
        }
        let mut next = self.config();
        next.avatar_epoch = next.avatar_epoch.saturating_add(1);
        self.save_config(next)
    }

    pub fn preview_friend(&self, text: &str) -> Result<FriendPreview> {
        match parse_friend_import(text.to_string())? {
            FriendImport::Direct { card } => self.preview_card(card, None, false),
            FriendImport::Shared { shared } => {
                let expired = shared_card_expired(shared.clone(), now_ms());
                self.preview_card(shared.card.clone(), Some(shared), expired)
            }
        }
    }

    fn preview_card(
        &self,
        card: cruisemesh_core::FriendCard,
        shared: Option<SharedFriendCard>,
        expired: bool,
    ) -> Result<FriendPreview> {
        let user_id = friend_card_user_id(card.clone());
        if user_id == self.identity.user_id {
            bail!("that is this computer's own friend card");
        }
        let known = self.store.get_contact(user_id.clone())?.is_some();
        let sharer_name = shared
            .as_ref()
            .and_then(|value| {
                self.store
                    .get_contact(value.sharer_user_id.clone())
                    .ok()
                    .flatten()
            })
            .map(|contact| {
                contact
                    .nickname
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(contact.name)
            });
        Ok(FriendPreview {
            name: card.name,
            fingerprint_words: fingerprint_words(user_id),
            already_known: known,
            shared: shared.is_some(),
            expired,
            sharer_name,
        })
    }

    fn save_config(&self, next: NodeConfig) -> Result<NodeConfig> {
        next.save(&self.paths.config)?;
        *self
            .config
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = next.clone();
        Ok(next)
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
            self.config().display_name,
            self.identity.clone(),
            url,
            token,
        )
        .map_err(Into::into)
    }

    pub fn friend_link(&self) -> Result<String> {
        Ok(make_friend_link(self.friend_card_json()?)?)
    }

    pub fn import_friend(&self, text: &str) -> Result<(Contact, Option<SharedFriendCard>)> {
        let (card, shared) = match parse_friend_import(text.to_string())? {
            FriendImport::Direct { card } => (card, None),
            FriendImport::Shared { shared } => {
                if shared_card_expired(shared.clone(), now_ms()) {
                    bail!("that code has expired. Ask for a new one.");
                }
                (shared.card.clone(), Some(shared))
            }
        };
        let user_id = friend_card_user_id(card.clone());
        if user_id == self.identity.user_id {
            bail!("cannot import this computer's own friend card");
        }
        if self.store.is_user_blocked(user_id.clone())? {
            self.store.unblock_user(user_id.clone())?;
        }
        let _ = self.store.clear_shared_request_dismissal(user_id.clone());
        let contact = Contact {
            user_id,
            name: card.name,
            sign_pk: card.sign_pk,
            agree_pk: card.agree_pk,
            relay_url: card.relay_url,
            relay_token: card.relay_token,
            nickname: None,
        };
        Ok((self.store.upsert_imported_contact(contact)?, shared))
    }

    pub fn status(&self) -> Result<BootstrapStatus> {
        let relay_configured = self.relay_config()?.is_some();
        Ok(BootstrapStatus {
            display_name: self.config().display_name,
            user_id: cruisemesh_core::format_user_id(self.identity.user_id.clone()),
            relay_configured,
            contacts: self.store.list_contacts()?.len(),
            reduced_mode: !relay_configured,
        })
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
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
    fn preferences_persist_across_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(temp.path().join("CruiseMesh")).unwrap();
        let first = BootstrapStore::open(paths.clone()).unwrap();
        first.update_preferences(false, false, None).unwrap();
        drop(first);

        let second = BootstrapStore::open(paths).unwrap();
        assert!(!second.config().prevent_sleep_on_ac);
        assert!(!second.config().share_online);
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
        let (imported, shared) = store.import_friend(&link).unwrap();
        assert!(shared.is_none());
        assert_eq!(imported.user_id, phone.user_id);
        assert_eq!(store.store.list_contacts().unwrap().len(), 1);
    }
}
