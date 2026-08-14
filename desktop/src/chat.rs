use std::{collections::HashMap, sync::Arc};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use cruisemesh_core::{
    core_reaction_summaries_by_target, core_tick_status_for, core_visible_chat_messages,
    create_group, decode_attachment_payload, encode_attachment_payload,
    encode_profile_sync_content, encode_reaction_payload, fingerprint_words, voice_capture_plan,
    AttachmentMediaType, AuthoredEnvelope, AuthoredReceipt, Contact, CoreAttachmentPayload,
    CoreMessageTarget, CoreReactionPayload, CoreTickStatus, Group, MessageStore,
    ProfileSyncContent, StoredMessage, KIND_ATTACHMENT_MANIFEST, KIND_GROUP_INVITE,
    KIND_PROFILE_SYNC, KIND_REACTION, KIND_TEXT, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};
use serde::{Deserialize, Serialize};

use crate::{bootstrap::BootstrapStore, lan::session::PeerHub, mesh::inbound::InboundExecutor};

const USER_ID_BYTES: usize = 16;
const MAX_TEXT_BYTES: usize = 64 * 1024;
// Fifty maximum-sized attachments still fit below the UI host's 16 MiB
// response cap after base64 and JSON expansion. Paging can extend this later
// without ever making a single pipe response unbounded.
const CHAT_HISTORY_ROW_LIMIT: u64 = 50;
const REACTION_HISTORY_ROW_LIMIT: u64 = 1_000;

#[derive(Clone)]
pub struct ChatService {
    bootstrap: Arc<BootstrapStore>,
    hub: Arc<PeerHub>,
    inbound: InboundExecutor,
    relay_nudge: Arc<tokio::sync::Notify>,
    listening_port: u16,
}

#[derive(Clone, Debug, Serialize)]
pub struct AppSnapshot {
    pub profile: ProfileView,
    pub node: AppNodeView,
    pub preferences: PreferencesView,
    pub diagnostics: DiagnosticsView,
    pub lan_peers: usize,
    pub contacts: Vec<ContactView>,
    pub conversations: Vec<ConversationSummary>,
    pub attachment_max_blob_bytes: u32,
    pub voice_min_duration_ms: u32,
    pub voice_max_duration_ms: u32,
    pub terms_accepted: bool,
    pub shore_pass: crate::bootstrap::ShorePassStatus,
    pub pending_shared: Vec<PendingSharedView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileView {
    pub display_name: String,
    pub friend_link: String,
    pub fingerprint_words: Vec<String>,
    pub avatar_base64: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AppNodeView {
    pub relay_configured: bool,
    pub contacts: usize,
    pub reduced_mode: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PreferencesView {
    pub prevent_sleep_on_ac: bool,
    pub share_online: bool,
    pub friends_of_friends: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticsView {
    pub helper_version: &'static str,
    pub listening_port: u16,
    pub data_directory: String,
    pub logs_directory: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContactView {
    pub id: String,
    pub display_name: String,
    pub connected_lan: bool,
    pub internet_delivery_configured: bool,
    pub fingerprint_words: Vec<String>,
    pub nickname: Option<String>,
    pub blocked: bool,
    pub muted: bool,
    pub avatar_base64: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationSummary {
    pub id: String,
    pub kind: ConversationKind,
    pub title: String,
    pub member_count: usize,
    pub connected_lan: bool,
    pub unread_count: u32,
    pub preview: Option<String>,
    pub timestamp_ms: Option<i64>,
    pub tick: Option<TickView>,
    pub muted: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationKind {
    Person,
    Group,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationView {
    pub id: String,
    pub kind: ConversationKind,
    pub title: String,
    pub member_count: usize,
    pub has_older: bool,
    pub members: Vec<ConversationMember>,
    pub messages: Vec<MessageView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationMember {
    pub id: String,
    pub display_name: String,
    pub own: bool,
    pub fingerprint_words: Vec<String>,
    pub avatar_base64: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingSharedView {
    pub id: String,
    pub name: String,
    pub fingerprint_words: Vec<String>,
    pub sharer_name: String,
    pub offer_dont_ask_again: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportView {
    pub mailto: String,
    pub address: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShareContactView {
    pub name: String,
    pub code: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MessageView {
    pub id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub own: bool,
    pub lamport: u64,
    pub timestamp_ms: i64,
    pub kind: MessageKind,
    pub text: Option<String>,
    pub attachment: Option<AttachmentView>,
    pub reply_to_id: Option<String>,
    pub reactions: Vec<ReactionView>,
    pub tick: Option<TickView>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    Image,
    Audio,
    GroupInvite,
    UnsupportedAttachment,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttachmentView {
    pub mime_type: String,
    pub duration_ms: i64,
    pub data_base64: String,
    pub caption: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReactionView {
    pub emoji: String,
    pub count: u32,
    pub own: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TickView {
    Sent,
    Delivered,
    Read,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Audio,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentDraft {
    pub kind: AttachmentKind,
    pub mime_type: String,
    pub duration_ms: i64,
    pub data_base64: String,
    #[serde(default)]
    pub caption: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactionTarget {
    pub sender_id: String,
    pub lamport: u64,
    pub kind: u8,
}

enum Conversation {
    Person(Contact),
    Group(Group),
}

impl ChatService {
    pub fn new(
        bootstrap: Arc<BootstrapStore>,
        hub: Arc<PeerHub>,
        inbound: InboundExecutor,
        relay_nudge: Arc<tokio::sync::Notify>,
        listening_port: u16,
    ) -> Self {
        Self {
            bootstrap,
            hub,
            inbound,
            relay_nudge,
            listening_port,
        }
    }

    pub fn snapshot(&self) -> Result<AppSnapshot> {
        let contacts = self.bootstrap.store().list_contacts()?;
        let connected = self.hub.connected_user_ids();
        let status = self.bootstrap.status()?;
        let config = self.bootstrap.config();
        let contact_views = contacts
            .iter()
            .map(|contact| self.contact_view(contact, &connected))
            .collect::<Result<Vec<_>>>()?;
        Ok(AppSnapshot {
            profile: self.profile_view()?,
            node: AppNodeView {
                relay_configured: status.relay_configured,
                contacts: status.contacts,
                reduced_mode: status.reduced_mode,
            },
            preferences: PreferencesView {
                prevent_sleep_on_ac: config.prevent_sleep_on_ac,
                share_online: config.share_online,
                friends_of_friends: config.friends_of_friends,
            },
            diagnostics: DiagnosticsView {
                helper_version: env!("CARGO_PKG_VERSION"),
                listening_port: self.listening_port,
                data_directory: self.bootstrap.paths().root.to_string_lossy().into_owned(),
                logs_directory: self.bootstrap.paths().logs.to_string_lossy().into_owned(),
            },
            lan_peers: self.hub.connected_peer_count(),
            contacts: contact_views,
            conversations: self.list_conversations(contacts, connected)?,
            attachment_max_blob_bytes: cruisemesh_core::attachment_max_blob_bytes(),
            voice_min_duration_ms: voice_capture_plan().min_duration_ms,
            voice_max_duration_ms: voice_capture_plan().max_duration_ms,
            terms_accepted: self.bootstrap.terms_accepted(),
            shore_pass: self.bootstrap.shore_pass_status()?,
            pending_shared: self.pending_shared_views()?,
        })
    }

    pub fn conversation(&self, conversation_id: &str) -> Result<ConversationView> {
        self.conversation_page(conversation_id, None)
    }

    pub fn conversation_page(
        &self,
        conversation_id: &str,
        before_timestamp_ms: Option<i64>,
    ) -> Result<ConversationView> {
        let conversation = self.resolve(conversation_id)?;
        let (chat_id, kind, title, member_count) = match &conversation {
            Conversation::Person(contact) => (
                contact.user_id.clone(),
                ConversationKind::Person,
                contact_display_name(contact),
                2,
            ),
            Conversation::Group(group) => (
                group.id.clone(),
                ConversationKind::Group,
                group.name.clone(),
                group.member_user_ids.len(),
            ),
        };
        let store = self.bootstrap.store();
        let fetch_limit = CHAT_HISTORY_ROW_LIMIT.saturating_add(1);
        let all = store.presentation_messages_before(
            chat_id.clone(),
            before_timestamp_ms,
            fetch_limit,
            REACTION_HISTORY_ROW_LIMIT,
        )?;
        let has_older =
            core_visible_chat_messages(all.clone()).len() as u64 > CHAT_HISTORY_ROW_LIMIT;
        let reaction_map = reaction_map(&all, &self.bootstrap.identity().user_id);
        let mut visible = core_visible_chat_messages(all);
        if has_older && visible.len() as u64 > CHAT_HISTORY_ROW_LIMIT {
            let skip = visible.len() - CHAT_HISTORY_ROW_LIMIT as usize;
            visible = visible.into_iter().skip(skip).collect();
        }
        let delivered = store.receipt_through(
            chat_id.clone(),
            self.bootstrap.identity().user_id.clone(),
            RECEIPT_TYPE_DELIVERED,
        )?;
        let read = store.receipt_through(
            chat_id.clone(),
            self.bootstrap.identity().user_id.clone(),
            RECEIPT_TYPE_READ,
        )?;
        let contacts: HashMap<Vec<u8>, String> = store
            .list_contacts()?
            .into_iter()
            .map(|contact| (contact.user_id.clone(), contact_display_name(&contact)))
            .collect();
        let messages = visible
            .into_iter()
            .map(|message| {
                self.message_view(
                    &store,
                    &chat_id,
                    message,
                    delivered,
                    read,
                    &contacts,
                    &reaction_map,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ConversationView {
            id: conversation_id.to_string(),
            kind,
            title,
            member_count,
            has_older,
            members: self.conversation_members(&conversation)?,
            messages,
        })
    }

    pub async fn send_text(
        &self,
        conversation_id: &str,
        text: String,
        reply_to_id: Option<String>,
    ) -> Result<MessageView> {
        if text.trim().is_empty() {
            bail!("message is empty");
        }
        if text.len() > MAX_TEXT_BYTES {
            bail!("message is too large");
        }
        self.send_payload(
            conversation_id,
            KIND_TEXT,
            text.into_bytes(),
            decode_optional_msg_id(reply_to_id)?,
        )
        .await?;
        self.last_own_message(conversation_id)
    }

    pub async fn send_attachment(
        &self,
        conversation_id: &str,
        draft: AttachmentDraft,
        reply_to_id: Option<String>,
    ) -> Result<MessageView> {
        let blob = BASE64
            .decode(draft.data_base64.as_bytes())
            .context("attachment is not valid base64")?;
        let payload = encode_attachment_payload(CoreAttachmentPayload {
            media_type: match draft.kind {
                AttachmentKind::Image => AttachmentMediaType::Image,
                AttachmentKind::Audio => AttachmentMediaType::Audio,
            },
            mime_type: draft.mime_type,
            duration_ms: draft.duration_ms,
            blob,
            caption: draft.caption,
        })?;
        self.send_payload(
            conversation_id,
            KIND_ATTACHMENT_MANIFEST,
            payload,
            decode_optional_msg_id(reply_to_id)?,
        )
        .await?;
        self.last_own_message(conversation_id)
    }

    pub async fn react(
        &self,
        conversation_id: &str,
        target: ReactionTarget,
        emoji: String,
    ) -> Result<()> {
        let conversation = self.resolve(conversation_id)?;
        let chat_id = conversation_chat_id(&conversation);
        let sender = decode_user_id(&target.sender_id)?;
        let exists = self
            .bootstrap
            .store()
            .messages_for_chat(chat_id)?
            .into_iter()
            .any(|message| {
                message.sender_user_id == sender
                    && message.lamport == target.lamport
                    && message.kind == target.kind
            });
        if !exists {
            bail!("reaction target is not in this conversation");
        }
        let payload = encode_reaction_payload(CoreReactionPayload {
            target: CoreMessageTarget {
                sender_user_id: sender,
                lamport: target.lamport,
                kind: target.kind,
            },
            emoji,
        })?;
        self.send_payload(conversation_id, KIND_REACTION, payload, None)
            .await?;
        Ok(())
    }

    pub async fn mark_read(&self, conversation_id: &str) -> Result<bool> {
        let Conversation::Person(contact) = self.resolve(conversation_id)? else {
            // Core has no per-member group-read aggregation yet. Do not invent
            // a scalar group watermark in the Windows shell.
            return Ok(false);
        };
        let through = self
            .bootstrap
            .store()
            .highest_contiguous_lamport(contact.user_id.clone(), contact.user_id.clone())?;
        if through == 0 {
            return Ok(false);
        }
        let authored = self.bootstrap.store().author_receipt(
            self.bootstrap.identity().clone(),
            contact.clone(),
            contact.user_id.clone(),
            RECEIPT_TYPE_READ,
            through,
            now_ms(),
        )?;
        if let Some(authored) = authored {
            self.send_receipt(&contact, authored).await;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn create_group(&self, name: String, member_ids: Vec<String>) -> Result<String> {
        let store = self.bootstrap.store();
        let mut members = Vec::with_capacity(member_ids.len());
        for id in member_ids {
            let user_id = decode_person_id(&id)?;
            let contact = store
                .get_contact(user_id)?
                .context("group member is not an accepted contact")?;
            members.push(contact);
        }
        if members.is_empty() {
            bail!("select at least one contact");
        }
        let mut ids = members
            .iter()
            .map(|contact| contact.user_id.clone())
            .collect::<Vec<_>>();
        ids.push(self.bootstrap.identity().user_id.clone());
        let group = create_group(name, ids)?;
        store.upsert_group(group.clone())?;
        for invite in store.queue_group_invites(
            self.bootstrap.identity().clone(),
            group.clone(),
            members.clone(),
            now_ms(),
        )? {
            let recipient = invite.envelope.recipient_user_id.clone();
            self.send_authored_to(&recipient, invite).await;
        }
        Ok(group_id(&group.id))
    }

    pub async fn update_profile(&self, display_name: String) -> Result<ProfileView> {
        let config = self.bootstrap.update_display_name(display_name)?;
        let payload = self.profile_sync_payload(&config)?;
        for contact in self.bootstrap.store().list_contacts()? {
            let authored = self.bootstrap.store().author_pairwise_message(
                self.bootstrap.identity().clone(),
                contact.clone(),
                KIND_PROFILE_SYNC,
                payload.clone(),
                None,
                now_ms(),
            )?;
            self.send_authored_to(&contact.user_id, authored).await;
        }
        self.profile_view()
    }

    pub fn update_preferences(
        &self,
        prevent_sleep_on_ac: bool,
        share_online: bool,
        friends_of_friends: Option<bool>,
    ) -> Result<PreferencesView> {
        let config = self.bootstrap.update_preferences(
            prevent_sleep_on_ac,
            share_online,
            friends_of_friends,
        )?;
        self.relay_nudge.notify_one();
        Ok(PreferencesView {
            prevent_sleep_on_ac: config.prevent_sleep_on_ac,
            share_online: config.share_online,
            friends_of_friends: config.friends_of_friends,
        })
    }

    pub async fn import_and_request_friend(&self, text: &str) -> Result<String> {
        let (contact, shared) = self.bootstrap.import_friend(text)?;
        self.send_friend_request(&contact, shared).await?;
        self.relay_nudge.notify_one();
        Ok(contact_display_name(&contact))
    }

    pub fn delete_contact(&self, conversation_id: &str) -> Result<()> {
        let Conversation::Person(contact) = self.resolve(conversation_id)? else {
            bail!("only a person can be deleted this way");
        };
        self.bootstrap.store().delete_contact(contact.user_id)?;
        Ok(())
    }

    pub fn set_nickname(&self, conversation_id: &str, nickname: Option<String>) -> Result<()> {
        let Conversation::Person(contact) = self.resolve(conversation_id)? else {
            bail!("only a person can have a nickname");
        };
        self.bootstrap
            .store()
            .set_contact_nickname(contact.user_id, nickname)?;
        Ok(())
    }

    pub fn set_blocked(&self, conversation_id: &str, blocked: bool) -> Result<()> {
        let Conversation::Person(contact) = self.resolve(conversation_id)? else {
            bail!("only a person can be blocked");
        };
        if blocked {
            self.bootstrap
                .store()
                .block_user(contact.user_id, now_ms())?;
        } else {
            self.bootstrap.store().unblock_user(contact.user_id)?;
        }
        Ok(())
    }

    pub fn set_muted(&self, conversation_id: &str, muted: bool) -> Result<()> {
        let _ = self.resolve(conversation_id)?;
        self.bootstrap.set_muted(conversation_id, muted)?;
        Ok(())
    }

    pub fn report_contact(&self, conversation_id: &str) -> Result<ReportView> {
        let Conversation::Person(contact) = self.resolve(conversation_id)? else {
            bail!("only a person can be reported");
        };
        let name = contact_display_name(&contact);
        let their_id = cruisemesh_core::format_user_id(contact.user_id.clone());
        let words = fingerprint_words(contact.user_id).join(" ");
        let my_id = cruisemesh_core::format_user_id(self.bootstrap.identity().user_id.clone());
        let subject = "CruiseMesh abuse report";
        let body = format!(
            "Reporting: {name}\nTheir ID: {their_id}\nTheir safety words: {words}\nMy ID: {my_id}\n\nWhat happened:\n"
        );
        Ok(ReportView {
            mailto: format!(
                "mailto:abuse@cruisemesh.app?subject={}&body={}",
                urlencoding(subject),
                urlencoding(&body)
            ),
            address: "abuse@cruisemesh.app".into(),
        })
    }

    pub async fn rename_group(&self, conversation_id: &str, name: String) -> Result<()> {
        let Conversation::Group(group) = self.resolve(conversation_id)? else {
            bail!("only a group can be renamed");
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            bail!("group name cannot be empty");
        }
        let result = self.bootstrap.store().author_group_metadata_update(
            self.bootstrap.identity().clone(),
            group.clone(),
            name,
            group.member_user_ids,
            now_ms(),
        )?;
        self.publish_group_update(&result.authored, &result.group.member_user_ids)
            .await;
        Ok(())
    }

    pub async fn add_group_members(
        &self,
        conversation_id: &str,
        member_ids: Vec<String>,
    ) -> Result<()> {
        let Conversation::Group(group) = self.resolve(conversation_id)? else {
            bail!("only a group can gain members");
        };
        let store = self.bootstrap.store();
        let mut additions = Vec::new();
        for id in member_ids {
            let user_id = decode_person_id(&id)?;
            if group
                .member_user_ids
                .iter()
                .any(|member| member == &user_id)
            {
                continue;
            }
            additions.push(
                store
                    .get_contact(user_id)?
                    .context("group member is not an accepted contact")?,
            );
        }
        if additions.is_empty() {
            bail!("select at least one new contact");
        }
        let mut all_ids = group.member_user_ids.clone();
        all_ids.extend(additions.iter().map(|contact| contact.user_id.clone()));
        let result = store.author_group_metadata_update(
            self.bootstrap.identity().clone(),
            group.clone(),
            group.name,
            all_ids,
            now_ms(),
        )?;
        for invite in store.queue_group_invites(
            self.bootstrap.identity().clone(),
            result.group.clone(),
            additions,
            now_ms(),
        )? {
            let recipient = invite.envelope.recipient_user_id.clone();
            self.send_authored_to(&recipient, invite).await;
        }
        self.publish_group_update(&result.authored, &result.group.member_user_ids)
            .await;
        Ok(())
    }

    pub fn share_contact(&self, conversation_id: &str) -> Result<ShareContactView> {
        let Conversation::Person(contact) = self.resolve(conversation_id)? else {
            bail!("only a person can be shared");
        };
        if !self.bootstrap.config().friends_of_friends {
            bail!("turn on friends of friends before sharing a contact");
        }
        let name = contact_display_name(&contact);
        let card = cruisemesh_core::FriendCard {
            name: contact.name,
            sign_pk: contact.sign_pk,
            agree_pk: contact.agree_pk,
            relay_url: contact.relay_url,
            relay_token: contact.relay_token,
            signature: None,
            roster_head_hash: None,
        };
        let shared = cruisemesh_core::create_shared_friend_card(
            self.bootstrap.identity().clone(),
            card,
            self.bootstrap.config().friends_of_friends_revision,
            now_ms(),
        )?;
        Ok(ShareContactView {
            name,
            code: cruisemesh_core::make_shared_contact_code(shared)?,
        })
    }

    pub async fn accept_pending_shared(&self, requester_id: &str) -> Result<String> {
        let user_id = decode_person_id(requester_id)?;
        let pending = self
            .bootstrap
            .store()
            .get_pending_shared_request(user_id.clone())?
            .context("that request is no longer waiting")?;
        let contact = Contact {
            user_id: pending.requester_user_id,
            name: pending.name,
            sign_pk: pending.sign_pk,
            agree_pk: pending.agree_pk,
            relay_url: pending.relay_url,
            relay_token: pending.relay_token,
            nickname: None,
        };
        let contact = self.bootstrap.store().upsert_imported_contact(contact)?;
        self.bootstrap
            .store()
            .delete_pending_shared_request(user_id)?;
        self.send_friend_request(&contact, None).await?;
        Ok(contact_display_name(&contact))
    }

    pub fn dismiss_pending_shared(&self, requester_id: &str, suppress: bool) -> Result<()> {
        let user_id = decode_person_id(requester_id)?;
        if suppress {
            self.bootstrap
                .store()
                .suppress_shared_requests(user_id.clone())?;
        } else {
            self.bootstrap
                .store()
                .record_shared_request_dismissal(user_id.clone())?;
        }
        self.bootstrap
            .store()
            .delete_pending_shared_request(user_id)?;
        Ok(())
    }

    pub async fn set_profile_photo(&self, data_base64: String) -> Result<ProfileView> {
        let bytes = if data_base64.is_empty() {
            Vec::new()
        } else {
            BASE64
                .decode(data_base64.as_bytes())
                .context("photo is not valid base64")?
        };
        if bytes.len() > 24 * 1024 {
            bail!("that photo is too large to use as a profile picture");
        }
        let config = self.bootstrap.save_avatar_bytes(&bytes)?;
        let payload = self.profile_sync_payload(&config)?;
        for contact in self.bootstrap.store().list_contacts()? {
            let authored = self.bootstrap.store().author_pairwise_message(
                self.bootstrap.identity().clone(),
                contact.clone(),
                KIND_PROFILE_SYNC,
                payload.clone(),
                None,
                now_ms(),
            )?;
            self.send_authored_to(&contact.user_id, authored).await;
        }
        self.profile_view()
    }

    fn profile_view(&self) -> Result<ProfileView> {
        let avatar = self.bootstrap.load_avatar_bytes();
        Ok(ProfileView {
            display_name: self.bootstrap.config().display_name,
            friend_link: self.bootstrap.friend_link()?,
            fingerprint_words: fingerprint_words(self.bootstrap.identity().user_id.clone()),
            avatar_base64: (!avatar.is_empty()).then(|| BASE64.encode(avatar)),
        })
    }

    fn profile_sync_payload(&self, config: &crate::config::NodeConfig) -> Result<Vec<u8>> {
        Ok(encode_profile_sync_content(ProfileSyncContent {
            avatar_epoch: config.avatar_epoch,
            name: config.display_name.clone(),
            avatar: self.bootstrap.load_avatar_bytes(),
            friends_of_friends_version: 1,
            friends_of_friends_enabled: config.friends_of_friends,
            friends_of_friends_revision: config.friends_of_friends_revision,
        })?)
    }

    fn contact_view(&self, contact: &Contact, connected: &[Vec<u8>]) -> Result<ContactView> {
        let id = person_id(&contact.user_id);
        let avatar = self
            .bootstrap
            .store()
            .contact_avatar(contact.user_id.clone())?;
        Ok(ContactView {
            id: id.clone(),
            display_name: contact_display_name(contact),
            connected_lan: connected.iter().any(|peer| peer == &contact.user_id),
            internet_delivery_configured: contact.relay_url.is_some()
                && contact.relay_token.is_some(),
            fingerprint_words: fingerprint_words(contact.user_id.clone()),
            nickname: contact.nickname.clone(),
            blocked: self
                .bootstrap
                .store()
                .is_user_blocked(contact.user_id.clone())?,
            muted: self.bootstrap.is_muted(&id),
            avatar_base64: avatar
                .filter(|bytes| !bytes.is_empty())
                .map(|bytes| BASE64.encode(bytes)),
        })
    }

    fn conversation_members(&self, conversation: &Conversation) -> Result<Vec<ConversationMember>> {
        let store = self.bootstrap.store();
        let own = self.bootstrap.identity().user_id.clone();
        let ids = match conversation {
            Conversation::Person(contact) => vec![own.clone(), contact.user_id.clone()],
            Conversation::Group(group) => group.member_user_ids.clone(),
        };
        let mut members = Vec::new();
        for id in ids {
            let own_member = id == own;
            let (name, avatar) = if own_member {
                (
                    self.bootstrap.config().display_name,
                    self.bootstrap.load_avatar_bytes(),
                )
            } else if let Some(contact) = store.get_contact(id.clone())? {
                (
                    contact_display_name(&contact),
                    store.contact_avatar(id.clone())?.unwrap_or_default(),
                )
            } else {
                ("Unknown".into(), Vec::new())
            };
            members.push(ConversationMember {
                id: if own_member {
                    person_id(&own)
                } else {
                    person_id(&id)
                },
                display_name: name,
                own: own_member,
                fingerprint_words: fingerprint_words(id),
                avatar_base64: (!avatar.is_empty()).then(|| BASE64.encode(avatar)),
            });
        }
        Ok(members)
    }

    fn pending_shared_views(&self) -> Result<Vec<PendingSharedView>> {
        let store = self.bootstrap.store();
        let mut rows = Vec::new();
        for request in store.list_pending_shared_requests(now_ms())? {
            let sharer_name = store
                .get_contact(request.sharer_user_id.clone())?
                .map(|contact| contact_display_name(&contact))
                .unwrap_or_else(|| "A friend".into());
            let dismissal =
                store.get_shared_request_dismissal(request.requester_user_id.clone())?;
            rows.push(PendingSharedView {
                id: person_id(&request.requester_user_id),
                name: request.name,
                fingerprint_words: fingerprint_words(request.requester_user_id),
                sharer_name,
                offer_dont_ask_again: dismissal.map(|row| row.count >= 1).unwrap_or(false),
            });
        }
        Ok(rows)
    }

    async fn send_friend_request(
        &self,
        contact: &Contact,
        shared: Option<cruisemesh_core::SharedFriendCard>,
    ) -> Result<()> {
        let card = self.bootstrap.friend_card_json()?;
        let payload = if let Some(shared) = shared {
            cruisemesh_core::make_shared_friend_request_payload(card, shared)?
        } else {
            card
        };
        let authored = self.bootstrap.store().author_friend_request(
            self.bootstrap.identity().clone(),
            contact.clone(),
            payload,
            now_ms(),
        )?;
        self.inbound
            .record_authored(authored.envelope.msg_id.clone());
        self.send_authored_to(&contact.user_id, authored).await;
        Ok(())
    }

    async fn publish_group_update(
        &self,
        authored: &cruisemesh_core::AuthoredEnvelope,
        members: &[Vec<u8>],
    ) {
        self.inbound
            .record_authored(authored.envelope.msg_id.clone());
        for member in members {
            if member != &self.bootstrap.identity().user_id {
                let _ = self.hub.send_to_peer(member, authored.frame.clone()).await;
            }
        }
        self.relay_nudge.notify_one();
    }

    fn list_conversations(
        &self,
        contacts: Vec<Contact>,
        connected: Vec<Vec<u8>>,
    ) -> Result<Vec<ConversationSummary>> {
        let store = self.bootstrap.store();
        let own = self.bootstrap.identity().user_id.clone();
        let mut rows = Vec::new();
        for contact in contacts {
            let preview = store.chat_preview(contact.user_id.clone(), own.clone())?;
            let id = person_id(&contact.user_id);
            let mut row = summary_from_preview(
                id.clone(),
                ConversationKind::Person,
                contact_display_name(&contact),
                2,
                connected.iter().any(|id| id == &contact.user_id),
                preview,
                &own,
            );
            row.muted = self.bootstrap.is_muted(&id);
            rows.push(row);
        }
        for group in store.list_groups()? {
            let preview = store.chat_preview(group.id.clone(), own.clone())?;
            let id = group_id(&group.id);
            let mut row = summary_from_preview(
                id.clone(),
                ConversationKind::Group,
                group.name,
                group.member_user_ids.len(),
                false,
                preview,
                &own,
            );
            row.muted = self.bootstrap.is_muted(&id);
            rows.push(row);
        }
        rows.sort_by(|left, right| {
            right
                .timestamp_ms
                .unwrap_or(i64::MIN)
                .cmp(&left.timestamp_ms.unwrap_or(i64::MIN))
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        });
        Ok(rows)
    }

    fn resolve(&self, id: &str) -> Result<Conversation> {
        let store = self.bootstrap.store();
        if id.starts_with("person:") {
            return store
                .get_contact(decode_person_id(id)?)?
                .map(Conversation::Person)
                .context("contact no longer exists");
        }
        if id.starts_with("group:") {
            return store
                .get_group(decode_group_id(id)?)?
                .map(Conversation::Group)
                .context("group no longer exists");
        }
        bail!("invalid conversation id")
    }

    async fn send_payload(
        &self,
        conversation_id: &str,
        kind: u8,
        payload: Vec<u8>,
        reply_to_msg_id: Option<Vec<u8>>,
    ) -> Result<()> {
        let store = self.bootstrap.store();
        match self.resolve(conversation_id)? {
            Conversation::Person(contact) => {
                let authored = store.author_pairwise_message(
                    self.bootstrap.identity().clone(),
                    contact.clone(),
                    kind,
                    payload,
                    reply_to_msg_id,
                    now_ms(),
                )?;
                self.send_authored_to(&contact.user_id, authored).await;
            }
            Conversation::Group(group) => {
                let authored = store.author_group_message(
                    self.bootstrap.identity().clone(),
                    group.clone(),
                    kind,
                    payload,
                    reply_to_msg_id,
                    now_ms(),
                )?;
                self.inbound
                    .record_authored(authored.envelope.msg_id.clone());
                for member in group.member_user_ids {
                    if member != self.bootstrap.identity().user_id {
                        let _ = self.hub.send_to_peer(&member, authored.frame.clone()).await;
                    }
                }
                self.relay_nudge.notify_one();
            }
        }
        Ok(())
    }

    async fn send_authored_to(&self, recipient: &[u8], authored: AuthoredEnvelope) {
        self.inbound
            .record_authored(authored.envelope.msg_id.clone());
        let _ = self.hub.send_to_peer(recipient, authored.frame).await;
        self.relay_nudge.notify_one();
    }

    async fn send_receipt(&self, contact: &Contact, authored: AuthoredReceipt) {
        self.inbound
            .record_authored(authored.envelope.msg_id.clone());
        let _ = self
            .hub
            .send_to_peer(&contact.user_id, authored.frame)
            .await;
        self.relay_nudge.notify_one();
    }

    fn last_own_message(&self, conversation_id: &str) -> Result<MessageView> {
        self.conversation(conversation_id)?
            .messages
            .into_iter()
            .rev()
            .find(|message| message.own)
            .context("authored message was not persisted")
    }

    #[allow(clippy::too_many_arguments)]
    fn message_view(
        &self,
        store: &MessageStore,
        chat_id: &[u8],
        message: StoredMessage,
        delivered: u64,
        read: u64,
        contacts: &HashMap<Vec<u8>, String>,
        reactions: &HashMap<(Vec<u8>, u64, u8), Vec<ReactionView>>,
    ) -> Result<MessageView> {
        let own = message.sender_user_id == self.bootstrap.identity().user_id;
        let reference = store.message_reference(
            chat_id.to_vec(),
            message.sender_user_id.clone(),
            message.lamport,
        )?;
        let (kind, text, attachment) = match message.kind {
            KIND_TEXT => (
                MessageKind::Text,
                Some(String::from_utf8_lossy(&message.payload).into_owned()),
                None,
            ),
            KIND_ATTACHMENT_MANIFEST => match decode_attachment_payload(message.payload.clone()) {
                Some(value) => {
                    let kind = match value.media_type {
                        AttachmentMediaType::Image => MessageKind::Image,
                        AttachmentMediaType::Audio => MessageKind::Audio,
                    };
                    (
                        kind,
                        None,
                        Some(AttachmentView {
                            mime_type: value.mime_type,
                            duration_ms: value.duration_ms,
                            data_base64: BASE64.encode(value.blob),
                            caption: value.caption,
                        }),
                    )
                }
                None => (
                    MessageKind::UnsupportedAttachment,
                    Some("This attachment could not be displayed.".into()),
                    None,
                ),
            },
            KIND_GROUP_INVITE => (MessageKind::GroupInvite, None, None),
            _ => bail!("non-visible message escaped the core visibility policy"),
        };
        let stable_id = reference
            .as_ref()
            .map(|value| hex(&value.msg_id))
            .unwrap_or_else(|| legacy_message_id(&message.sender_user_id, message.lamport));
        Ok(MessageView {
            id: stable_id,
            sender_id: hex(&message.sender_user_id),
            sender_name: if own {
                self.bootstrap.config().display_name.clone()
            } else {
                contacts
                    .get(&message.sender_user_id)
                    .cloned()
                    .unwrap_or_else(|| "Group member".into())
            },
            own,
            lamport: message.lamport,
            timestamp_ms: message.timestamp,
            kind,
            text,
            attachment,
            reply_to_id: reference
                .and_then(|value| value.reply_to_msg_id)
                .map(|value| hex(&value)),
            reactions: reactions
                .get(&(
                    message.sender_user_id.clone(),
                    message.lamport,
                    message.kind,
                ))
                .cloned()
                .unwrap_or_default(),
            tick: own.then(|| tick_view(core_tick_status_for(message.lamport, delivered, read))),
        })
    }
}

fn summary_from_preview(
    id: String,
    kind: ConversationKind,
    title: String,
    member_count: usize,
    connected_lan: bool,
    preview: cruisemesh_core::CoreChatPreview,
    own_user_id: &[u8],
) -> ConversationSummary {
    let (text, timestamp, tick) = preview
        .last_message
        .map(|message| {
            let tick = (message.sender_user_id == own_user_id).then(|| {
                tick_view(core_tick_status_for(
                    message.lamport,
                    preview.own_delivered_through,
                    preview.own_read_through,
                ))
            });
            (preview_text(&message), Some(message.timestamp), tick)
        })
        .unwrap_or((None, None, None));
    ConversationSummary {
        id,
        kind,
        title,
        member_count,
        connected_lan,
        unread_count: preview.unread_count,
        preview: text,
        timestamp_ms: timestamp,
        tick,
        muted: false,
    }
}

fn preview_text(message: &StoredMessage) -> Option<String> {
    match message.kind {
        KIND_TEXT => Some(String::from_utf8_lossy(&message.payload).into_owned()),
        KIND_ATTACHMENT_MANIFEST => Some(
            decode_attachment_payload(message.payload.clone())
                .map(|value| match value.media_type {
                    AttachmentMediaType::Image => "Photo".into(),
                    AttachmentMediaType::Audio => "Voice message".into(),
                })
                .unwrap_or_else(|| "Unsupported attachment".into()),
        ),
        KIND_GROUP_INVITE => Some("Group created".into()),
        _ => None,
    }
}

fn reaction_map(
    messages: &[StoredMessage],
    own_user_id: &[u8],
) -> HashMap<(Vec<u8>, u64, u8), Vec<ReactionView>> {
    core_reaction_summaries_by_target(messages.to_vec(), own_user_id.to_vec())
        .into_iter()
        .map(|summary| {
            (
                (
                    summary.target.sender_user_id,
                    summary.target.lamport,
                    summary.target.kind,
                ),
                summary
                    .reactions
                    .into_iter()
                    .map(|reaction| ReactionView {
                        emoji: reaction.emoji,
                        count: reaction.count,
                        own: reaction.reacted_by_own_user,
                    })
                    .collect(),
            )
        })
        .collect()
}

fn tick_view(value: CoreTickStatus) -> TickView {
    match value {
        CoreTickStatus::Sent => TickView::Sent,
        CoreTickStatus::Delivered => TickView::Delivered,
        CoreTickStatus::Read => TickView::Read,
    }
}

fn contact_display_name(contact: &Contact) -> String {
    contact
        .nickname
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| contact.name.clone())
}

fn conversation_chat_id(value: &Conversation) -> Vec<u8> {
    match value {
        Conversation::Person(contact) => contact.user_id.clone(),
        Conversation::Group(group) => group.id.clone(),
    }
}

fn person_id(bytes: &[u8]) -> String {
    format!("person:{}", hex(bytes))
}

fn group_id(bytes: &[u8]) -> String {
    format!("group:{}", hex(bytes))
}

fn decode_person_id(value: &str) -> Result<Vec<u8>> {
    decode_prefixed_id(value, "person:")
}

fn decode_group_id(value: &str) -> Result<Vec<u8>> {
    decode_prefixed_id(value, "group:")
}

fn decode_user_id(value: &str) -> Result<Vec<u8>> {
    let value = value.strip_prefix("person:").unwrap_or(value);
    let bytes = decode_hex(value)?;
    if bytes.len() != USER_ID_BYTES {
        bail!("invalid user id length");
    }
    Ok(bytes)
}

fn decode_prefixed_id(value: &str, prefix: &str) -> Result<Vec<u8>> {
    let encoded = value.strip_prefix(prefix).context("invalid id kind")?;
    let bytes = decode_hex(encoded)?;
    if bytes.len() != USER_ID_BYTES {
        bail!("invalid conversation id length");
    }
    Ok(bytes)
}

fn decode_optional_msg_id(value: Option<String>) -> Result<Option<Vec<u8>>> {
    value
        .map(|value| {
            if value.starts_with("legacy:") {
                bail!("legacy messages cannot be reply targets");
            }
            let bytes = decode_hex(&value)?;
            if bytes.len() != 16 {
                bail!("invalid message id length");
            }
            Ok(bytes)
        })
        .transpose()
}

fn legacy_message_id(sender: &[u8], lamport: u64) -> String {
    format!("legacy:{}:{lamport}", hex(sender))
}

fn hex(bytes: &[u8]) -> String {
    const CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(CHARS[(byte >> 4) as usize] as char);
        out.push(CHARS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("invalid hexadecimal id");
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0]).context("invalid hexadecimal id")?;
            let low = hex_nibble(pair[1]).context("invalid hexadecimal id")?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn urlencoding(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use cruisemesh_core::{
        generate_identity, make_friend_card, make_friend_link, CoreInboundDisposition,
        CoreInboundSource, Identity,
    };
    use tempfile::TempDir;

    use crate::{lan::endpoint_cache::EndpointCache, store_paths::AppPaths};

    fn service() -> (TempDir, Arc<BootstrapStore>, ChatService, Contact, Identity) {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(temp.path().join("CruiseMesh")).unwrap();
        let bootstrap = Arc::new(BootstrapStore::open(paths.clone()).unwrap());
        let friend = generate_identity();
        let card = make_friend_card("Emma".into(), friend.clone(), None, None).unwrap();
        let (contact, _) = bootstrap
            .import_friend(&make_friend_link(card).unwrap())
            .unwrap();
        let hub = Arc::new(PeerHub::new(bootstrap.identity()));
        let inbound = InboundExecutor::start(
            bootstrap.store(),
            bootstrap.identity().clone(),
            EndpointCache::new(paths.endpoint_cache),
        )
        .unwrap();
        let service = ChatService::new(
            bootstrap.clone(),
            hub,
            inbound,
            Arc::new(tokio::sync::Notify::new()),
            45_892,
        );
        (temp, bootstrap, service, contact, friend)
    }

    #[tokio::test]
    async fn text_is_core_authored_and_visible_in_the_read_model() {
        let (_temp, _bootstrap, service, contact, _friend) = service();
        let id = person_id(&contact.user_id);
        let sent = service
            .send_text(&id, "Hello from Windows".into(), None)
            .await
            .unwrap();
        assert!(sent.own);
        assert_eq!(sent.text.as_deref(), Some("Hello from Windows"));
        let conversation = service.conversation(&id).unwrap();
        assert_eq!(conversation.messages.len(), 1);
        assert!(matches!(
            conversation.messages[0].tick,
            Some(TickView::Sent)
        ));
    }

    #[test]
    fn snapshot_hides_the_public_user_id_and_reports_operational_details() {
        let (_temp, bootstrap, service, _contact, _friend) = service();
        let snapshot = service.snapshot().unwrap();
        let json = serde_json::to_value(&snapshot).unwrap();

        assert!(json["profile"].get("formatted_user_id").is_none());
        assert!(json["node"].get("user_id").is_none());
        assert_eq!(json["diagnostics"]["listening_port"], 45_892);
        assert_eq!(json["diagnostics"]["helper_version"], "0.1.0");
        assert_eq!(
            json["preferences"]["prevent_sleep_on_ac"],
            bootstrap.config().prevent_sleep_on_ac
        );
        let plan = cruisemesh_core::voice_capture_plan();
        assert_eq!(json["voice_min_duration_ms"], plan.min_duration_ms);
        assert_eq!(json["voice_max_duration_ms"], plan.max_duration_ms);
    }

    #[test]
    fn advanced_preferences_persist_and_are_returned_to_the_ui() {
        let (_temp, bootstrap, service, _contact, _friend) = service();
        let updated = service.update_preferences(false, false, None).unwrap();

        assert!(!updated.prevent_sleep_on_ac);
        assert!(!updated.share_online);
        assert!(!bootstrap.config().prevent_sleep_on_ac);
        assert!(!bootstrap.config().share_online);
        assert!(!service.snapshot().unwrap().preferences.share_online);
        assert!(service.snapshot().unwrap().preferences.friends_of_friends);
    }

    #[test]
    fn contact_management_and_older_page_surface_are_available() {
        let (_temp, _bootstrap, service, contact, _friend) = service();
        let id = person_id(&contact.user_id);
        service.set_nickname(&id, Some("Em".into())).unwrap();
        service.set_muted(&id, true).unwrap();
        let snapshot = service.snapshot().unwrap();
        let row = snapshot.contacts.iter().find(|row| row.id == id).unwrap();
        assert_eq!(row.nickname.as_deref(), Some("Em"));
        assert!(row.muted);
        let page = service.conversation_page(&id, Some(1)).unwrap();
        assert!(page.messages.is_empty());
        assert!(!page.has_older);
        service.delete_contact(&id).unwrap();
        assert!(service.snapshot().unwrap().contacts.is_empty());
    }

    #[test]
    fn hidden_traffic_and_a_bad_photo_cannot_blank_visible_history() {
        let (_temp, bootstrap, service, contact, _friend) = service();
        let store = bootstrap.store();
        store
            .insert_message(StoredMessage {
                chat_id: contact.user_id.clone(),
                sender_user_id: contact.user_id.clone(),
                lamport: 1,
                timestamp: 1,
                kind: KIND_TEXT,
                payload: b"history survives".to_vec(),
            })
            .unwrap();
        store
            .insert_message(StoredMessage {
                chat_id: contact.user_id.clone(),
                sender_user_id: contact.user_id.clone(),
                lamport: 2,
                timestamp: 2,
                kind: KIND_ATTACHMENT_MANIFEST,
                payload: b"not-an-attachment".to_vec(),
            })
            .unwrap();
        for lamport in 3..=80 {
            store
                .insert_message(StoredMessage {
                    chat_id: contact.user_id.clone(),
                    sender_user_id: contact.user_id.clone(),
                    lamport,
                    timestamp: lamport as i64,
                    kind: KIND_PROFILE_SYNC,
                    payload: Vec::new(),
                })
                .unwrap();
        }

        let conversation = service.conversation(&person_id(&contact.user_id)).unwrap();
        assert_eq!(conversation.messages.len(), 2);
        assert_eq!(
            conversation.messages[0].text.as_deref(),
            Some("history survives")
        );
        assert!(matches!(
            conversation.messages[1].kind,
            MessageKind::UnsupportedAttachment
        ));
    }

    #[tokio::test]
    async fn android_style_photo_survives_the_windows_inbound_and_read_model() {
        let (_temp, bootstrap, service, contact, phone) = service();
        let phone_store = MessageStore::open(":memory:".into()).unwrap();
        let windows = Contact {
            user_id: bootstrap.identity().user_id.clone(),
            name: "Cabin PC".into(),
            sign_pk: bootstrap.identity().sign_pk.clone(),
            agree_pk: bootstrap.identity().agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        };
        let expected_photo = vec![0xff, 0xd8, 0xff, 0xe0, 1, 2, 3, 0xff, 0xd9];
        let text = phone_store
            .author_pairwise_message(
                phone.clone(),
                windows.clone(),
                KIND_TEXT,
                b"before photo".to_vec(),
                None,
                1,
            )
            .unwrap();
        let photo = phone_store
            .author_pairwise_message(
                phone,
                windows,
                KIND_ATTACHMENT_MANIFEST,
                encode_attachment_payload(CoreAttachmentPayload {
                    media_type: AttachmentMediaType::Image,
                    mime_type: "image/jpeg".into(),
                    duration_ms: 0,
                    blob: expected_photo.clone(),
                    caption: "from Android".into(),
                })
                .unwrap(),
                None,
                2,
            )
            .unwrap();

        for authored in [text, photo] {
            let result = service
                .inbound
                .process(CoreInboundSource::Relay, authored.frame, 10)
                .await
                .unwrap();
            assert_eq!(result.disposition, CoreInboundDisposition::Consumed);
        }

        let conversation = service.conversation(&person_id(&contact.user_id)).unwrap();
        assert_eq!(conversation.messages.len(), 2);
        assert_eq!(
            conversation.messages[0].text.as_deref(),
            Some("before photo")
        );
        let image = &conversation.messages[1];
        assert!(matches!(image.kind, MessageKind::Image));
        let attachment = image.attachment.as_ref().unwrap();
        assert_eq!(attachment.mime_type, "image/jpeg");
        assert_eq!(attachment.caption, "from Android");
        assert_eq!(
            BASE64.decode(&attachment.data_base64).unwrap(),
            expected_photo
        );
    }

    #[tokio::test]
    async fn group_creation_uses_accepted_contacts_and_queues_invites() {
        let (_temp, bootstrap, service, contact, _friend) = service();
        let id = service
            .create_group("Family".into(), vec![person_id(&contact.user_id)])
            .await
            .unwrap();
        assert!(id.starts_with("group:"));
        let group = service.conversation(&id).unwrap();
        assert_eq!(group.title, "Family");
        assert_eq!(group.member_count, 2);
        assert_eq!(bootstrap.store().list_groups().unwrap().len(), 1);
    }

    #[test]
    fn typed_ids_reject_kind_confusion_and_bad_hex() {
        let id = person_id(&[7; USER_ID_BYTES]);
        assert_eq!(decode_person_id(&id).unwrap(), vec![7; USER_ID_BYTES]);
        assert!(decode_group_id(&id).is_err());
        assert!(decode_person_id("person:not-hex").is_err());
    }
}
