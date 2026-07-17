//! CruiseMesh core: identity, message/group crypto, and persistent protocol
//! primitives. See DESIGN.md §6.* for the scheme this implements.

uniffi::setup_scaffolding!("cruisemesh_core");

mod authoring;
mod backup;
mod content;
mod crypto;
mod engine;
mod framing;
mod gossip;
mod groups;
mod identity;
mod lan_session;
mod lan_util;
mod protocol;
mod relay_wire;
mod semantic;
mod store;
mod transport_policy;

pub use authoring::{AuthoredEnvelope, AuthoredReceipt};
pub use backup::{
    backup_min_passphrase_length, backup_passphrase_strength, decode_identity_bytes,
    encode_identity_bytes, open_backup, seal_backup, BackupPassphraseStrength, CoreBackupError,
    CoreBackupPayload,
};
pub use content::{
    attachment_max_blob_bytes, decode_attachment_payload, decode_reaction_payload,
    encode_attachment_payload, encode_reaction_payload, AttachmentMediaType, CoreAttachmentPayload,
    CoreMessageTarget, CoreReactionPayload,
};
pub use crypto::{open_message, seal_message, OpenedMessage};
pub use engine::{
    core_hello_identity_matches, core_inbound_gate, core_relay_ack_ids, core_should_ack_inbound,
    CoreDigestSprayPlan, CoreInboundDisposition, CoreInboundGate, CoreRelayEnvelopeDisposition,
};
pub use framing::{
    ble_att_header_overhead, ble_default_att_mtu, ble_max_att_value_len, fragment_ble_frame,
    BleFrameReassembler,
};
pub use gossip::SeenIds;
pub use groups::{
    create_group, decode_group_invite_content, encode_group_invite_content, open_group_message,
    rotate_group, seal_group_message, Group,
};
pub use identity::{
    fingerprint_words, generate_identity, make_friend_card, make_friend_link, parse_friend_card,
    parse_friend_text, CoreError, FriendCard, Identity,
};
pub use lan_session::{
    lan_default_tcp_port, lan_max_frame_size, lan_service_type, LanNoiseSession,
    LAN_DEFAULT_TCP_PORT, LAN_MAX_FRAME_SIZE, LAN_SERVICE_TYPE,
};
pub use lan_util::{
    core_format_lan_endpoint, core_lan_network_id_for_components, core_lan_network_id_for_ipv4,
    core_make_lan_endpoint_link, core_parse_lan_endpoint, core_parse_lan_endpoint_link,
    core_subnet_24_hosts, lan_endpoint_cache_is_fresh, should_resend_lan_endpoint, CoreLanEndpoint,
};
pub use protocol::{
    compute_recipient_hint, create_introduction_ticket, decode_extended_message_body,
    decode_friend_directory_content, decode_introduced_friend_request, decode_lan_endpoint_content,
    decode_message_body, decode_profile_sync_content, decode_receipt_content, default_expiry,
    encode_digest, encode_envelope_frame, encode_friend_directory_content, encode_hello,
    encode_introduced_friend_request, encode_lan_endpoint, encode_lan_endpoint_content,
    encode_message_body, encode_message_body_with_reply, encode_profile_sync_content,
    encode_receipt_content, encode_transport_probe, generate_msg_id, parse_frame,
    verify_introduction_ticket, ExtendedMessageBody, Frame, FriendDirectoryContent,
    FriendDirectoryEntry, IntroducedFriendRequest, IntroductionTicket, LanEndpointContent,
    MessageBody, ProfileSyncContent, ReceiptContent, SuggestedFriendCard, DEFAULT_EXPIRY_MS,
    DEFAULT_HOP_TTL, KIND_ATTACHMENT_CHUNK, KIND_ATTACHMENT_MANIFEST, KIND_FRIEND_DIRECTORY,
    KIND_FRIEND_REQUEST, KIND_GROUP_INVITE, KIND_INTRODUCED_FRIEND_REQUEST, KIND_LAN_ENDPOINT_HINT,
    KIND_PROFILE_SYNC, KIND_REACTION, KIND_RECEIPT, KIND_TEXT, MS_PER_DAY, RECEIPT_TYPE_DELIVERED,
    RECEIPT_TYPE_READ,
};
pub use relay_wire::{
    normalize_relay_url, relay_build_fetch_path, relay_decode_fetch_page,
    relay_decode_post_response, relay_decode_presence_page, relay_encode_ack_request,
    relay_encode_post_envelope, relay_encode_presence_request, CoreRelayFetchPage,
    CoreRelayFetchedEnvelope, CoreRelayPresence, CoreRelayPresencePage,
};
pub use semantic::{
    core_is_visible_chat_kind, core_last_visible_message, core_reaction_summaries_by_target,
    core_tick_status_for, core_unread_count, core_visible_chat_messages, core_visible_gap_indices,
    CoreReactionSummary, CoreReactionTargetSummary, CoreReplyMetadata, CoreTickStatus,
};
pub use store::{
    CarriedEnvelope, Contact, ContactDiscoveryPolicy, ContactProvenance, DigestEntry,
    FriendSuggestion, MessageArrival, MessageOrigin, MessageReference, MessageStore,
    OutboundEnvelope, OutgoingReceiptEnvelope, StoredMessage,
};
pub use transport_policy::{
    core_transport_send_plan, digest_is_expected_chat_id, digest_through_lamport_for_sender,
    CoreIdentifiedRoute, CoreLanHealthAction, CoreLanHealthDecision, CoreLanHealthTracker,
    CoreMeshRouterState, CoreReconnectBackoffTracker, CoreTransport, CoreTransportRoute,
    DEFAULT_INITIAL_BACKOFF_MS, DEFAULT_LAN_HEALTH_MAX_TIMEOUTS, DEFAULT_LAN_HEALTH_TIMEOUT_MS,
    DEFAULT_MAX_BACKOFF_MS, DEFAULT_MAX_CONSECUTIVE_FAILURES, SMALL_FRAME_RACE_MAX_BYTES,
};
