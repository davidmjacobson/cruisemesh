//! CruiseMesh core: identity, message/group crypto, and persistent protocol
//! primitives. See DESIGN.md §6.* for the scheme this implements.

uniffi::setup_scaffolding!("cruisemesh_core");

mod authoring;
mod backup;
mod causal_order;
mod connection_health;
mod contact_relay_health;
mod content;
mod crypto;
mod deep_link;
mod engine;
mod framing;
mod gossip;
mod groups;
mod identity;
mod lan_session;
mod lan_util;
mod late_arrival;
mod limits;
mod link_detect;
mod outbound_retirement;
mod protocol;
mod protocol_event;
mod recipient_hints;
mod relay_cursor;
mod relay_setup;
mod relay_status;
mod relay_wire;
mod semantic;
mod session;
mod spray_policy;
mod store;
mod transport_policy;

pub use authoring::{AuthoredEnvelope, AuthoredGroupMetadataUpdate, AuthoredReceipt};
pub use backup::{
    backup_max_file_bytes, backup_min_passphrase_length, backup_passphrase_strength,
    decode_identity_bytes, encode_identity_bytes, open_backup, seal_backup,
    BackupPassphraseStrength, CoreBackupError, CoreBackupPayload,
};
pub use causal_order::{causal_display_timestamp, CAUSAL_ORDER_MAX_SKEW_MS};
pub use connection_health::{
    core_classify_connection_health, core_classify_delivery_line, core_classify_recipient_delivery,
    core_connection_check_pending, core_connection_checking_expired, core_contact_endpoint_resting,
    core_contact_route_usable, core_group_people, core_person_attention_rank,
    core_person_best_route, core_person_is_reachable_now, core_person_reach,
    core_relay_queue_reflects_delivery, CoreConnectionEvidence, CoreConnectionHealth,
    CoreConnectionHealthInput, CoreConnectionHealthReport, CoreDeliveryBlockedReason,
    CoreDeliveryLine, CoreDeliveryLineInput, CoreDeliveryState, CoreDirectLink,
    CoreDirectPathState, CoreHealthAction, CoreHealthReason, CoreMeshRuntime, CorePeopleGroups,
    CorePersonAttention, CorePersonGroup, CorePersonHealthInput, CorePersonPlacement,
    CorePersonReach, CorePersonRoute, CoreRecipientDeliveryInput, CoreRelayPathState,
    CONNECTION_CHECKING_TIMEOUT_MS, CONNECTION_PRESENCE_ONLINE_WINDOW_MS,
    RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
};
pub use contact_relay_health::{
    contact_relay_fault_is_authoritative, core_contact_relay_endpoint_usable,
    core_contact_relay_is_stale, core_contact_relay_recheck_due, core_contact_relay_streak_delta,
    core_contact_relay_unreachable_delta, core_contact_relay_unreachable_endpoint_usable,
    core_contact_relay_unreachable_is_stale, CONTACT_RELAY_RECHECK_MS, CONTACT_RELAY_STALE_STREAK,
    CONTACT_RELAY_UNREACHABLE_REST_MS, CONTACT_RELAY_UNREACHABLE_STALE_STREAK,
    CONTACT_RELAY_UNREACHABLE_STREAK,
};
pub use content::{
    attachment_max_blob_bytes, decode_attachment_payload, decode_reaction_payload,
    encode_attachment_payload, encode_reaction_payload, AttachmentMediaType, CoreAttachmentPayload,
    CoreMessageTarget, CoreReactionPayload,
};
pub use crypto::{open_message, seal_message, OpenedMessage};
pub use deep_link::{deep_link_route, DeepLinkRoute};
pub use engine::{
    core_consumed_seen_is_ackable, core_consumed_seen_is_ackable_with_hidden,
    core_group_fanout_rows, core_hello_identity_matches, core_inbound_gate,
    core_is_own_fanout_hint, core_pairwise_sender_authorized, core_relay_ack_ids,
    core_should_ack_inbound, CoreDigestSprayPlan, CoreGroupFanoutRow, CoreInboundDisposition,
    CoreInboundGate, CoreRelayEnvelopeDisposition, MAX_CARRY_FUTURE_MS,
};
pub use framing::{
    ble_att_header_overhead, ble_default_att_mtu, ble_max_att_value_len, fragment_ble_frame,
    BleFrameReassembler,
};
pub use gossip::SeenIds;
pub use groups::{
    apply_group_metadata_update, create_group, create_group_metadata_update,
    decode_group_invite_content, decode_group_metadata_update, encode_group_invite_content,
    encode_group_metadata_update, open_group_message, rotate_group, seal_group_message, Group,
    GroupMetadataUpdate,
};
pub use identity::{
    fingerprint_words, friend_card_match, generate_identity, make_friend_card, make_friend_link,
    parse_friend_card, parse_friend_text, CoreError, FriendCard, FriendCardMatch, Identity,
};
pub use lan_session::{
    lan_default_tcp_port, lan_max_frame_size, lan_service_type, LanNoiseSession,
    LAN_DEFAULT_TCP_PORT, LAN_MAX_FRAME_SIZE, LAN_SERVICE_TYPE,
};
pub use lan_util::{
    core_format_lan_endpoint, core_lan_network_id_for_components, core_lan_network_id_for_ipv4,
    core_make_lan_endpoint_link, core_parse_lan_endpoint, core_parse_lan_endpoint_link,
    core_subnet_24_hosts, lan_endpoint_cache_is_fresh, lan_endpoint_host_is_local,
    should_resend_lan_endpoint, CoreLanEndpoint,
};
pub use late_arrival::{
    core_late_arrival_flags, late_arrival_flags, LateArrivalInput, LATE_ARRIVAL_MIN_DELAY_MS,
};
pub use limits::{MAX_ENVELOPE_SEALED_BYTES, MAX_P2P_FRAME_BYTES};
pub use link_detect::{
    core_detect_links, core_link_openable_scheme, CoreDetectedLink, CoreLinkScheme,
};
// Plain Rust policy, deliberately not `#[uniffi::export]`: the shells never
// decide any of this. The store executes it, and `core/tests` asserts it under
// `QUEUE-01`.
pub use outbound_retirement::{
    authored_delivery_lifetime_ms, authored_expiry, covered_by_delivered_watermark,
    supersedes_queued_generations, LAN_ENDPOINT_HINT_EXPIRY_MS,
};
pub use protocol::{
    compute_recipient_hint, core_is_hidden_spray_kind, core_kind_persists_msg_id_row,
    core_own_capabilities, create_introduction_ticket, decode_extended_message_body,
    decode_friend_directory_content, decode_introduced_friend_request, decode_lan_endpoint_content,
    decode_message_body, decode_profile_sync_content, decode_receipt_content,
    decode_relay_update_content, default_expiry, encode_digest, encode_envelope_frame,
    encode_friend_directory_content, encode_hello, encode_hello2, encode_introduced_friend_request,
    encode_lan_endpoint, encode_lan_endpoint_content, encode_message_body,
    encode_message_body_with_reply, encode_profile_sync_content, encode_receipt_content,
    encode_relay_update_content, encode_transport_probe, fanout_msg_id, generate_msg_id,
    parse_frame, verify_introduction_ticket, ExtendedMessageBody, Frame, FriendDirectoryContent,
    FriendDirectoryEntry, IntroducedFriendRequest, IntroductionTicket, LanEndpointContent,
    MessageBody, ProfileSyncContent, ReceiptContent, RelayUpdateContent, SuggestedFriendCard,
    CAP_ACKS_HIDDEN_KINDS, CAP_RELAY_UPDATE, DEFAULT_EXPIRY_MS, DEFAULT_HOP_TTL,
    KIND_ATTACHMENT_CHUNK, KIND_ATTACHMENT_MANIFEST, KIND_FRIEND_DIRECTORY, KIND_FRIEND_REQUEST,
    KIND_GROUP_INVITE, KIND_GROUP_METADATA_UPDATE, KIND_INTRODUCED_FRIEND_REQUEST,
    KIND_LAN_ENDPOINT_HINT, KIND_PROFILE_SYNC, KIND_REACTION, KIND_RECEIPT, KIND_RELAY_UPDATE,
    KIND_TEXT, MS_PER_DAY, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};
// Plain Rust, deliberately not `#[uniffi::export]` beyond the store methods
// below: no shell composes an event. Core decision points emit them and the
// store persists them, so redaction stays a property of the type rather than
// of every call site that might one day cross the boundary.
pub use protocol_event::{
    is_known_invariant, protocol_event_codes, redaction_defect, replay, validate, ProtocolEvent,
    ProtocolEventArchive, ProtocolEventCode, ProtocolEventDefect, ProtocolEventHeader,
    ReplaySummary, PROTOCOL_EVENT_ARCHIVE_STEM, PROTOCOL_EVENT_MAX_BYTES,
    PROTOCOL_EVENT_MAX_RECORDS, PROTOCOL_EVENT_SCHEMA, PROTOCOL_INVARIANT_IDS,
};
pub use recipient_hints::{dedupe_hints, recent_hints_for, recent_presence_hints_for};
pub use relay_cursor::{
    relay_cursor_advance, relay_cursor_key, relay_fetch_walk_continues,
    relay_frontier_after_completed_sweep, relay_hint_source_digest,
    relay_mailbox_continuation_delay_ms, relay_mailbox_max_envelopes_per_pass,
    relay_mailbox_max_pages_per_pass, relay_mailbox_walk_action, relay_pass_start_cursor,
    relay_sweep_due, relay_sweep_interval_ms, relay_sweep_restart_from_zero,
    RelayMailboxWalkAction, RELAY_MAILBOX_CONTINUATION_DELAY_MS,
    RELAY_MAILBOX_MAX_ENVELOPES_PER_PASS, RELAY_MAILBOX_MAX_PAGES_PER_PASS,
    RELAY_SWEEP_INTERVAL_MS,
};
pub use relay_setup::{
    make_relay_setup_card, parse_relay_setup_text, relay_setup_is_official, RelaySetup,
};
pub use relay_status::{
    relay_classify_http_error, relay_fault_is_transient, relay_fault_rank, relay_retry_after_ms,
    CoreRelayFault,
};
pub use relay_wire::{
    core_group_fanout_relay_target, normalize_relay_url, relay_build_fetch_path,
    relay_contact_shares_own_family, relay_decode_fetch_page, relay_decode_post_response,
    relay_decode_presence_page, relay_deposit_token_for, relay_encode_ack_request,
    relay_encode_post_envelope, relay_encode_presence_request, relay_fetch_batch_limit,
    relay_fetch_shrunk_limit, relay_max_response_bytes, relay_token_is_deposit,
    resolved_contact_delivery_poll_relay, resolved_contact_delivery_relay,
    resolved_contact_poll_relay, resolved_contact_relay, CoreRelayFetchPage,
    CoreRelayFetchedEnvelope, CoreRelayPresence, CoreRelayPresencePage, GroupRelayMember,
    RelayEndpoint,
};
pub use semantic::{
    core_is_visible_chat_kind, core_last_visible_message, core_reaction_summaries_by_target,
    core_tick_status_for, core_unread_count, core_visible_chat_messages, core_visible_gap_indices,
    CoreReactionSummary, CoreReactionTargetSummary, CoreReplyMetadata, CoreTickStatus,
};
pub use session::relay_policy::{
    core_family_relay_backoff_cap_ms, core_family_relay_backoff_delay_ms,
    core_family_relay_backoff_vectors, core_family_relay_health_vectors,
    core_family_relay_jitter_ms, core_family_relay_jitter_vectors, core_family_relay_pacer_vectors,
    core_family_relay_rerun_vectors, core_relay_pass_health, core_relay_rerun_action,
    core_worse_relay_fault, CoreFamilyRelayBackoff, CoreFamilyRelayPacer, CoreRelayBackoffVector,
    CoreRelayHealthVector, CoreRelayJitterVector, CoreRelayPacerVector, CoreRelayPassHealth,
    CoreRelayRerunAction, CoreRelayRerunVector, FAMILY_RELAY_BACKOFF_BASE_MS,
    FAMILY_RELAY_BACKOFF_CAP_MS, FAMILY_RELAY_JITTER_WINDOW_MS, FAMILY_RELAY_REQUEST_INTERVAL_MS,
};
pub use spray_policy::{
    core_spray_retry_arm_max_ms, CoreSprayAdmission, CoreSprayAdmissionReason, CoreSprayGate,
    CoreSprayGateReason, CoreSprayLanePlan, CoreSprayPlanShape, CoreSprayPolicy, CoreSprayTrigger,
    CARRIED_SPRAY_BUDGET_BYTES, FIRST_CONTACT_LAPSE_MS, IDENTICAL_SET_REOFFER_INTERVAL_MS,
    LINK_BURST_BYTES, LINK_DRAIN_BYTES_PER_SEC, MAX_SPRAY_INTERVAL_MS, MIN_USEFUL_BURST_BYTES,
    OWN_OUTBOUND_SPRAY_BUDGET_BYTES, OWN_RECEIPT_SPRAY_BUDGET_BYTES, RECEIPT_QUIET_MAX_SHIFT,
    RECONNECT_SPRAY_MIN_INTERVAL_MS, SPRAY_EXCHANGE_WINDOW_MS, SPRAY_RETRY_ARM_MAX_MS,
    SPRAY_STATE_RETENTION_MS, TOTAL_ENCOUNTER_BUDGET_BYTES,
};
pub use store::{
    core_peer_transport_for_arrival, core_peer_transport_is_observed,
    inspect_restored_message_store, sanitize_restored_message_store,
    sanitize_restored_message_store_with_options, BackupContentOptions, BackupInventory,
    BackupSanitizationReport, CarriedEnvelope, ConsumedHiddenLamport, Contact,
    ContactDiscoveryPolicy, ContactProvenance, ContactRelayRejection, ContactRelayUnreachable,
    CoreCarriedCursor, CoreCarriedSyncPage, CoreChatPreview, CoreMessageReceivedAt,
    CoreRecipientDeliveryStatus, DigestEntry, FriendSuggestion, IncomingMessageInsertOutcome,
    MessageArrival, MessageConflictSummary, MessageOrigin, MessageReference, MessageStore,
    OutboundEnvelope, OutgoingReceiptEnvelope, PeerConnectionEvent, PeerConnectionEventKind,
    PeerConnectionSummary, PeerConnectionTransport, RelayFetchCursor, StoredMessage,
};
pub use transport_policy::{
    core_transport_send_plan, digest_is_expected_chat_id, digest_through_lamport_for_sender,
    may_start_carried_offer, CoreCarriedLane, CoreIdentifiedRoute, CoreLanHealthAction,
    CoreLanHealthDecision, CoreLanHealthTracker, CoreMeshRouterState, CoreReconnectBackoffTracker,
    CoreTransport, CoreTransportRoute, CARRIED_REWALK_MIN_INTERVAL_MS, DEFAULT_INITIAL_BACKOFF_MS,
    DEFAULT_LAN_HEALTH_MAX_TIMEOUTS, DEFAULT_LAN_HEALTH_TIMEOUT_MS, DEFAULT_MAX_BACKOFF_MS,
    DEFAULT_MAX_CONSECUTIVE_FAILURES, MAX_CONCURRENT_CARRIED_OFFERS,
};
