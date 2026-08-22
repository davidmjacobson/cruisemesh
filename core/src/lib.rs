//! CruiseMesh core: identity, message/group crypto, and persistent protocol
//! primitives. See DESIGN.md §6.* for the scheme this implements.

uniffi::setup_scaffolding!("cruisemesh_core");

mod authoring;
mod backup;
mod causal_order;
mod connection_health;
mod contact_relay_health;
mod contact_safety;
mod content;
mod crypto;
mod deep_link;
mod device_link;
mod device_roster;
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
// The blob plane (specs/media-two-plane.md). Mostly dark, and deliberately so:
// phase 1 exports the authoring seam, and the `pub use media` block below is
// the whole list of what a shell can reach. Chunk transfer — the pull and
// serve state machines, the partial-transfer store, the wire frames — is
// reachable from no dispatch, carry, or framing path and from no binding,
// which is what keeps the plane separation structural rather than a rule
// someone has to remember.
pub mod media;
mod outbound_retirement;
mod protocol;
mod protocol_event;
mod recipient_hints;
mod relay_cursor;
mod relay_rotation;
mod relay_setup;
mod relay_status;
mod relay_wire;
mod revocation;
mod roster_gossip;
mod roster_store;
mod sail_checklist;
mod semantic;
mod session;
mod ship_wifi;
mod spray_policy;
mod store;
mod sync_outbound;
mod sync_record;
mod sync_store;
mod sync_stream;
mod transport_policy;
mod voice;

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
pub use contact_safety::{
    core_roster_safety_changes, ContactSafetyChange, ContactSafetyFact, ContactSafetyReason,
    CONTACT_SAFETY_FACT_PAGE,
};
pub use content::{
    attachment_max_blob_bytes, decode_attachment_payload, decode_reaction_payload,
    encode_attachment_payload, encode_reaction_payload, AttachmentMediaType, CoreAttachmentPayload,
    CoreMessageTarget, CoreReactionPayload,
};
pub use crypto::{open_message, seal_message, OpenedMessage};
pub use deep_link::{deep_link_route, DeepLinkRoute};
pub use device_link::activation::{
    core_link_activation_ack, core_link_activation_gate, core_link_device_offer,
    core_link_genesis_roster, core_link_open_activation_ack, core_link_open_device_offer,
    core_link_recovery_roster, core_link_sign_new_device_roster, CoreLinkActivation,
    CoreLinkActivationStage, CoreLinkBootstrapImport, CoreLinkGateReason, CoreLinkGateVerdict,
    CoreLinkGatedAction, CoreLinkImportReadiness, LinkActivationAck, LinkDeviceOffer,
    LinkRosterUpdate,
};
pub use device_link::bootstrap::{
    core_link_bootstrap_chunks, core_link_bootstrap_decode, core_link_bootstrap_encode,
    core_link_bootstrap_join, core_link_bootstrap_verify, core_link_catch_up_plan, CoreLinkCatchUp,
    LinkBootstrap, LinkBootstrapContact, LinkBootstrapPerson, LinkBootstrapProfile,
    LINK_BOOTSTRAP_DEFAULT_LIFETIME_MS, LINK_BOOTSTRAP_HISTORY_HEAD_PER_CHAT,
    LINK_BOOTSTRAP_MAX_BYTES, LINK_BOOTSTRAP_MAX_MESSAGE_BYTES, LINK_BOOTSTRAP_VERSION,
};
pub use device_link::ceremony::{
    core_link_default_budgets, core_link_sas, CoreLinkAction, CoreLinkActionKind,
    CoreLinkApprovingDevice, CoreLinkNewDevice, CoreLinkOutcome, CoreLinkPhase, CoreLinkRole,
    CoreLinkSummary, LinkBudgets, LINK_CHANNEL_MAX_PLAINTEXT_BYTES, LINK_SAS_DIGITS,
};
pub use device_link::qr::{
    core_build_link_qr, core_link_qr_url, core_link_rendezvous_id, core_link_rendezvous_lane,
    core_parse_link_qr, CoreLinkLane, LinkRendezvous, DEVICE_LINK_PREFIX,
    LINK_QR_DEFAULT_LIFETIME_MS, LINK_QR_MAX_BYTES, LINK_QR_MAX_HINTS, LINK_RENDEZVOUS_ID_LEN,
};
pub use device_link::restore::{
    core_backup_restore_plan, core_backup_restore_plans, CoreRestoreIntent, CoreRestorePlan,
};
pub use device_roster::{
    core_decode_device_keypair, core_decode_roster, core_derive_device_id, core_device_add_outcome,
    core_device_namespace_id, core_device_sign, core_device_stream_id, core_device_verify,
    core_encode_device_keypair, core_encode_roster, core_legacy_device_id, core_own_identity_peer,
    core_roster_accept, core_roster_device_ids, core_roster_head_hash, core_roster_validate,
    core_sign_device_cert, core_sign_roster, core_verify_device_cert, generate_device_keypair,
    CoreOwnIdentityPeer, DeviceAddOutcome, DeviceCert, DeviceKeypair, DeviceSigningDomain,
    DeviceTombstone, OwnDeviceFleet, Roster, RosterRejection, RosterUpdateDecision,
    RosterUpdateOutcome, RosterUpdateReason, RosterVersion, DEVICE_CERT_FLAG_ROSTER_SIGNING,
    DEVICE_HARD_CAP, DEVICE_ID_LEN, DEVICE_SOFT_CAP, LEGACY_DEVICE_ID, ROSTER_HEAD_HASH_LEN,
    ROSTER_MAX_VERSION_JUMP,
};
pub use engine::{
    core_consumed_seen_is_ackable, core_consumed_seen_is_ackable_with_hidden,
    core_device_fanout_rows, core_group_fanout_rows, core_hello_identity_matches,
    core_inbound_gate, core_is_own_fanout_hint, core_pairwise_sender_authorized,
    core_relay_ack_ids, core_should_ack_inbound, CoreDigestSprayPlan, CoreGroupFanoutRow,
    CoreInboundDisposition, CoreInboundGate, CoreRelayEnvelopeDisposition, MAX_CARRY_FUTURE_MS,
    RELAY_FAMILY_QUOTA_BYTES, RELAY_MAX_ENVELOPE_SEALED_BYTES, RELAY_RATE_BYTES_PER_MIN,
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
    create_shared_friend_card, fingerprint_words, format_user_id, friend_card_match,
    friend_card_user_id, generate_identity, make_friend_card, make_friend_link,
    make_shared_contact_code, make_shared_friend_request_payload, parse_friend_card,
    parse_friend_import, parse_friend_request_content, parse_friend_text, shared_card_expired,
    verify_shared_friend_card, CoreError, FriendCard, FriendCardMatch, FriendImport,
    FriendRequestContent, Identity, SharedFriendCard,
};
pub use lan_session::{
    lan_default_tcp_port, lan_max_frame_size, lan_service_type, LanNoiseSession,
    LAN_DEFAULT_TCP_PORT, LAN_MAX_FRAME_SIZE, LAN_SERVICE_TYPE,
};
pub use lan_util::{
    core_format_lan_endpoint, core_lan_network_id_for_components, core_lan_network_id_for_ipv4,
    core_make_lan_endpoint_link, core_parse_lan_endpoint, core_parse_lan_endpoint_link,
    core_subnet_24_hosts, lan_endpoint_cache_is_fresh, lan_endpoint_host_is_local,
    lan_hosts_are_same_address, lan_hosts_share_local_network, should_resend_lan_endpoint,
    CoreLanEndpoint,
};
pub use late_arrival::{
    core_late_arrival_flags, late_arrival_flags, LateArrivalInput, LATE_ARRIVAL_MIN_DELAY_MS,
};
pub use limits::{MAX_ENVELOPE_SEALED_BYTES, MAX_P2P_FRAME_BYTES};
pub use link_detect::{
    core_detect_links, core_link_openable_scheme, CoreDetectedLink, CoreLinkScheme,
};
// The blob plane's boundary and nothing else of it: what is named here is
// exactly what crosses UniFFI. The pull and serve state machines, the wire
// frames and the partial-transfer store stay `media::…`, reachable from the
// crate and its tests and from no shell.
pub use media::{
    blob_transfer_permitted, core_media_manifest_body, core_media_recognize_manifest,
    core_media_seal_blob, media_blob_max_bytes, media_manifest_kind, media_manifest_max_bytes,
    media_thumbnail_max_bytes, peer_speaks_blob_plane, sanitize_media_filename, BlobTransferSource,
    BlobTransferVerdict, CoreMediaKind, CoreMediaManifest, CoreSealedMediaBlob,
};
// Plain Rust policy, deliberately not `#[uniffi::export]`: the shells never
// decide any of this. The store executes it, and `core/tests` asserts it under
// `QUEUE-01`.
pub use outbound_retirement::{
    authored_delivery_lifetime_ms, authored_expiry, covered_by_delivered_watermark,
    supersedes_queued_generations, LAN_ENDPOINT_HINT_EXPIRY_MS,
};
pub use protocol::{
    compute_recipient_hint, core_is_hidden_spray_kind, core_is_sync_record_kind,
    core_kind_persists_msg_id_row, core_own_capabilities, create_introduction_ticket,
    decode_extended_message_body, decode_friend_directory_content,
    decode_introduced_friend_request, decode_lan_endpoint_content, decode_message_body,
    decode_profile_sync_content, decode_receipt_content, decode_relay_update_content,
    default_expiry, device_fanout_msg_id, encode_digest, encode_envelope_frame,
    encode_friend_directory_content, encode_hello, encode_hello2, encode_introduced_friend_request,
    encode_lan_endpoint, encode_lan_endpoint_content, encode_message_body,
    encode_message_body_extended, encode_message_body_with_reply, encode_own_roster,
    encode_profile_sync_content, encode_receipt_content, encode_relay_update_content,
    encode_transport_probe, fanout_msg_id, generate_msg_id, parse_frame,
    verify_introduction_ticket, ExtendedMessageBody, Frame, FriendDirectoryContent,
    FriendDirectoryEntry, IntroducedFriendRequest, IntroductionTicket, LanEndpointContent,
    MessageBody, ProfileSyncContent, ReceiptContent, RelayUpdateContent, SuggestedFriendCard,
    CAP_ACKS_HIDDEN_KINDS, CAP_MEDIA_BLOB, CAP_MULTI_DEVICE, CAP_OWN_ROSTER_NOTICE,
    CAP_RELAY_UPDATE, CAP_ROSTER_GOSSIP, DEFAULT_EXPIRY_MS, DEFAULT_HOP_TTL, GROUP_ID_LEN,
    HIDDEN_SPRAY_KINDS, KIND_ATTACHMENT_CHUNK, KIND_ATTACHMENT_MANIFEST, KIND_FRIEND_DIRECTORY,
    KIND_FRIEND_REQUEST, KIND_GROUP_INVITE, KIND_GROUP_METADATA_UPDATE,
    KIND_INTRODUCED_FRIEND_REQUEST, KIND_LAN_ENDPOINT_HINT, KIND_PROFILE_SYNC, KIND_REACTION,
    KIND_RECEIPT, KIND_RELAY_UPDATE, KIND_ROSTER_GOSSIP, KIND_SYNC_CONTACTS, KIND_SYNC_DIGEST,
    KIND_SYNC_GROUPS, KIND_SYNC_HISTORY, KIND_SYNC_OWN_ROSTER, KIND_SYNC_SETTINGS,
    KIND_SYNC_WATERMARK, KIND_TEXT, MS_PER_DAY, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};
// Plain Rust, deliberately not `#[uniffi::export]` beyond the store methods
// below: no shell composes an event. Core decision points emit them and the
// store persists them, so redaction stays a property of the type rather than
// of every call site that might one day cross the boundary.
pub use protocol_event::{
    is_known_invariant, protocol_event_codes, redaction_defect, replay, validate, ProtocolEvent,
    ProtocolEventArchive, ProtocolEventCode, ProtocolEventDefect, ProtocolEventHeader,
    ReplaySummary, PROTOCOL_EVENT_ARCHIVE_STEM, PROTOCOL_EVENT_HEADER_KEYS,
    PROTOCOL_EVENT_MAX_BYTES, PROTOCOL_EVENT_MAX_RECORDS, PROTOCOL_EVENT_RECORD_KEYS,
    PROTOCOL_EVENT_SCHEMA, PROTOCOL_INVARIANT_IDS,
};
pub use recipient_hints::{
    dedupe_hints, recent_device_hints_for, recent_hints_for, recent_presence_hints_for,
    HINTS_PER_ID_FETCH, HINTS_PER_ID_PUSH, RELAY_MAX_FETCH_HINTS,
};
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
pub use relay_rotation::{
    core_mint_relay_member_token, core_plan_relay_rotation, RelayRotationCommit, RelayRotationPlan,
    RELAY_EPOCH_MAX_SKEW_MS, RELAY_MEMBER_TOKEN_PREFIX, SYNC_RELAY_CREDENTIAL_SETTING_KEY,
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
    relay_decode_presence_page, relay_decode_rotate_response, relay_deposit_token_for,
    relay_encode_ack_request, relay_encode_post_envelope, relay_encode_presence_request,
    relay_encode_rotate_request, relay_fetch_batch_limit, relay_fetch_shrunk_limit,
    relay_max_response_bytes, relay_rotate_path, relay_token_is_deposit,
    resolved_contact_delivery_poll_relay, resolved_contact_delivery_relay,
    resolved_contact_poll_relay, resolved_contact_relay, CoreRelayFetchPage,
    CoreRelayFetchedEnvelope, CoreRelayPresence, CoreRelayPresencePage, CoreRelayRotation,
    GroupRelayMember, RelayEndpoint,
};
pub use revocation::{
    core_recovery_revoke_roster, core_revoke_devices_roster, core_roster_newly_revoked,
    PendingRevocation, RevocationAdoption, RevocationAdoptionOutcome, RevocationCommit,
    RevocationHandoff, RevocationPath, RevocationUpdate,
};
pub use roster_gossip::RosterGossipAnnouncement;
pub use roster_store::{ContactDeviceState, ContactRosterState};
pub use sail_checklist::{
    core_sail_checklist, CoreSailChecklistInput, CoreSailChecklistItem, CoreSailChecklistItemId,
    CoreSailChecklistReport, CoreSailPermission, CoreSailPermissionRow,
};
pub use semantic::{
    core_group_tick_status_for, core_is_visible_chat_kind, core_last_visible_message,
    core_reaction_summaries_by_target, core_reactors_for_reaction, core_tick_status_for,
    core_unread_count, core_visible_chat_messages, core_visible_gap_indices, CoreReactionSummary,
    CoreReactionTargetSummary, CoreReplyMetadata, CoreTickStatus,
};
pub use ship_wifi::{
    core_ship_wifi_build_report, core_ship_wifi_evidence_strength, core_ship_wifi_forbidden_keys,
    core_ship_wifi_generate_nonce, core_ship_wifi_parse_report, core_ship_wifi_reduce,
    core_ship_wifi_report_file_name, core_ship_wifi_report_max_bytes,
    core_ship_wifi_schema_version, core_ship_wifi_serialize_report, ShipWifiAuthorization,
    ShipWifiCompletedSweep, ShipWifiConsent, ShipWifiDirectionsAttempted, ShipWifiDiscoverySource,
    ShipWifiEndpointSource, ShipWifiEvidenceSnapshot, ShipWifiEvidenceStrength,
    ShipWifiFailureClass, ShipWifiLatencyBucket, ShipWifiLocalPermission, ShipWifiNetworkContext,
    ShipWifiObservation, ShipWifiObservationEvent, ShipWifiOrigin, ShipWifiPeriod,
    ShipWifiPeriodPrecision, ShipWifiPlatform, ShipWifiProbeDirection, ShipWifiReport,
    ShipWifiReportAttribution, ShipWifiReportError, ShipWifiReportingClient, ShipWifiResult,
    ShipWifiSeparation, ShipWifiShip, ShipWifiSweepVerdict, ShipWifiVerdict, ShipWifiVpnReadiness,
    SHIP_WIFI_CONSENT_POLICY_VERSION, SHIP_WIFI_FORBIDDEN_KEYS, SHIP_WIFI_NONCE_BYTES,
    SHIP_WIFI_REPORT_FILE_NAME, SHIP_WIFI_REPORT_MAX_BYTES, SHIP_WIFI_REPORT_SCHEMA_VERSION,
};
// Package C0, driven from both shells by package C1. Each shell reaches it
// only behind a whole-encounter engine selection that defaults to the legacy
// sequencer, so the exported surface below is built and testable on both
// platforms while the field default is unchanged.
pub use session::mesh_meet::{
    core_plan_mesh_hello_frames, plan_mesh_hello_frames, CoreMeetOutcome, CoreMeetRequest,
    CoreMeetWork,
};
pub use session::mesh_receive::{
    CoreDeliveryVerdict, CoreDiscoveryPolicyState, CoreInboundCommit, CoreInboundDelivery,
    CoreInboundOutcome, CoreInboundSource, CoreInboundWork, CoreLanEndpointIntent,
};
pub use session::relay_pass::{
    core_relay_adapter_vectors, core_relay_pass_default_budgets, CoreRelayAction,
    CoreRelayActionKind, CoreRelayAdapterVector, CoreRelayContactConfig, CoreRelayContinuation,
    CoreRelayEndpointConfig, CoreRelayHeader, CoreRelayHttpRequest, CoreRelayHttpResult,
    CoreRelayOperation, CoreRelayPass, CoreRelayPassBudgets, CoreRelayPassOutcome,
    CoreRelayPassPlan, CoreRelayPassSummary, CoreRelayProgressReason, CoreRelayStage,
    CoreRelayTransportError, RELAY_CROSS_FAMILY_PRESENCE_MIN_INTERVAL_MS, RELAY_PASS_DEADLINE_MS,
    RELAY_PASS_MAX_AUTHORED_UPLOADS, RELAY_PASS_MAX_CARRIED_UPLOADS, RELAY_PASS_MAX_ENVELOPES,
    RELAY_PASS_MAX_PRESENCE_PROBES, RELAY_PASS_MAX_RECEIPT_UPLOADS, RELAY_PASS_MAX_REQUESTS,
    RELAY_PASS_MAX_RESPONSE_BYTES, RELAY_PRESENCE_RECENCY_ACTIVE, RELAY_PRESENCE_RECENCY_DAY,
    RELAY_PRESENCE_RECENCY_OLDER, RELAY_PRESENCE_RECENCY_RECENT,
};
pub use session::relay_policy::{
    core_family_relay_backoff_cap_ms, core_family_relay_backoff_delay_ms,
    core_family_relay_backoff_vectors, core_family_relay_health_vectors,
    core_family_relay_jitter_ms, core_family_relay_jitter_vectors, core_family_relay_pacer_vectors,
    core_family_relay_rerun_vectors, core_relay_network_permitted, core_relay_pass_health,
    core_relay_rerun_action, core_worse_relay_fault, CoreFamilyRelayBackoff, CoreFamilyRelayPacer,
    CoreRelayBackoffVector, CoreRelayHealthVector, CoreRelayJitterVector, CoreRelayNetworkVerdict,
    CoreRelayPacerVector, CoreRelayPassHealth, CoreRelayRerunAction, CoreRelayRerunVector,
    CoreRelayRoaming, FAMILY_RELAY_BACKOFF_BASE_MS, FAMILY_RELAY_BACKOFF_CAP_MS,
    FAMILY_RELAY_JITTER_WINDOW_MS, FAMILY_RELAY_REQUEST_INTERVAL_MS,
};
// Test support only, and named so at every call site: the incident fixtures
// made executable through a platform driver. No app code on either shell
// calls any of this; the only callers are the two adapter test suites and
// `core/tests/relay_fixture_transcript.rs`. It is exported over UniFFI rather
// than kept in a Rust test because the whole point is that a JVM test and an
// XCTest can run the same scenario and compare against the same expected
// transcript instead of each writing one down.
pub use session::relay_fixture::{
    core_relay_fixture_expected_transcript, core_relay_fixture_ideal_observation,
    core_relay_fixture_names, core_relay_fixture_plan, core_relay_fixture_reply,
    core_relay_fixture_scenario, core_relay_fixture_seed_store,
    core_relay_fixture_violated_invariants, CoreRelayFixtureEndpoint,
    CoreRelayFixtureObservedRequest, CoreRelayFixturePassSpec, CoreRelayFixtureReply,
    CoreRelayFixtureScenario, CoreRelayFixtureTranscript,
};
// The migration canary. Pure comparison over captured values, and removed
// with the legacy engine it exists to check.
pub use session::relay_shadow::{
    core_relay_shadow_compare, core_relay_shadow_max_rows, core_relay_shadow_max_skips,
    core_relay_shadow_sample, CoreRelayShadowCapture, CoreRelayShadowLane, CoreRelayShadowMismatch,
    CoreRelayShadowMismatchKind, CoreRelayShadowReport, CoreRelayShadowSample,
    CoreRelayShadowSampler, CoreRelayShadowStep, RELAY_SHADOW_MAX_ROWS,
    RELAY_SHADOW_MAX_SAMPLES_PER_DAY, RELAY_SHADOW_MAX_SKIPS, RELAY_SHADOW_MIN_INTERVAL_MS,
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
    core_carried_page_max_rows, core_peer_transport_for_arrival, core_peer_transport_is_observed,
    inspect_restored_message_store, sanitize_restored_message_store,
    sanitize_restored_message_store_with_options, BackupContentOptions, BackupInventory,
    BackupSanitizationReport, CarriedEnvelope, ConsumedHiddenLamport, Contact,
    ContactDiscoveryPolicy, ContactProvenance, ContactRelayRejection, ContactRelayUnreachable,
    CoreCarriedCursor, CoreCarriedSyncPage, CoreChatPreview, CoreMessageReceivedAt,
    CoreRecipientDeliveryStatus, DigestEntry, FriendSuggestion, GroupMemberReceipt,
    GroupReceiptState, IncomingMessageInsertOutcome, MessageArrival, MessageConflictSummary,
    MessageOrigin, MessageReference, MessageStore, OutboundEnvelope, OutgoingReceiptEnvelope,
    PeerConnectionEvent, PeerConnectionEventKind, PeerConnectionSummary, PeerConnectionTransport,
    PendingSharedRequest, RelayFetchCursor, StoredMessage, DEFAULT_CARRIED_PAGE_MAX_ROWS,
};
pub use sync_outbound::{
    core_sync_draft_key, OutboundAuthorClaim, OutboundAuthorDecision, SYNC_OUTBOUND_DEDUP_WINDOW_MS,
};
pub use sync_record::{
    core_decode_sync_contacts, core_decode_sync_groups, core_decode_sync_history,
    core_decode_sync_own_roster, core_decode_sync_record, core_decode_sync_settings,
    core_decode_sync_watermarks, core_device_sync_identity, core_encode_sync_contacts,
    core_encode_sync_groups, core_encode_sync_history, core_encode_sync_own_roster,
    core_encode_sync_record, core_encode_sync_settings, core_encode_sync_watermarks,
    core_mint_inbox_key, core_open_sync_handoff, core_open_sync_record, core_rotate_inbox_key,
    core_seal_sync_handoff, core_seal_sync_record, core_sign_sync_record, core_sync_handoff_admit,
    core_sync_kind_is_stream, core_sync_record_admit, core_sync_record_id,
    core_sync_record_kind_of, core_sync_record_kind_wire, core_sync_seal_is_current,
    core_verify_sync_record, InboxKey, SealedSyncRecord, SyncContactEntry, SyncContactsPayload,
    SyncGroupsPayload, SyncHistoryDirection, SyncHistoryEntry, SyncHistoryPayload,
    SyncOwnRosterPayload, SyncRecord, SyncRecordKind, SyncRecordRejection, SyncSettingEntry,
    SyncSettingsPayload, SyncWatermarkEntry, SyncWatermarkPayload, SYNC_RECORD_MAX_ENTRIES,
    SYNC_RECORD_MAX_PAYLOAD_BYTES,
};
pub use sync_store::{
    OwnSyncContext, StoredSyncRecord, SyncApplyOutcome, SyncApplyResult, SYNC_BLOCKED_SETTING_KEY,
};
pub use sync_stream::{
    core_decode_sync_digest, core_encode_sync_digest, core_plan_sync_backfill,
    core_sync_digest_gaps, SyncBackfillAction, SyncBackfillOffer, SyncBackfillPlan,
    SyncBackfillStep, SyncDigest, SyncGap, SyncStreamDigest, SYNC_DIGEST_MAX_STREAMS,
};
pub use transport_policy::{
    core_carried_offer_epoch_ms, core_transport_send_plan, digest_is_expected_chat_id,
    digest_is_shared_group, digest_through_lamport_for_sender, may_start_carried_offer,
    CoreCarriedLane, CoreCarriedOfferGate, CoreCarriedOfferReservation, CoreIdentifiedRoute,
    CoreLanHealthAction, CoreLanHealthDecision, CoreLanHealthTracker, CoreMeshRouterState,
    CoreReconnectBackoffTracker, CoreTransport, CoreTransportRoute, CARRIED_OFFER_EPOCH_MS,
    CARRIED_REWALK_MIN_INTERVAL_MS, DEFAULT_INITIAL_BACKOFF_MS, DEFAULT_LAN_HEALTH_MAX_TIMEOUTS,
    DEFAULT_LAN_HEALTH_TIMEOUT_MS, DEFAULT_MAX_BACKOFF_MS, DEFAULT_MAX_CONSECUTIVE_FAILURES,
    MAX_CONCURRENT_CARRIED_OFFERS,
};
pub use voice::{
    voice_capture_bytes, voice_capture_cancel, voice_capture_drag, voice_capture_elapsed,
    voice_capture_finish, voice_capture_idle_state, voice_capture_plan, voice_capture_press,
    voice_capture_release, voice_capture_start_hands_free, CoreVoiceCapturePlan,
    CoreVoiceCaptureState, CoreVoiceCaptureStep, VoiceCaptureEffect, VoiceCapturePhase,
};
