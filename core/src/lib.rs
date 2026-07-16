//! CruiseMesh core: identity, message/group crypto, and persistent protocol
//! primitives. See DESIGN.md §6.* for the scheme this implements.

uniffi::setup_scaffolding!("cruisemesh_core");

mod crypto;
mod gossip;
mod groups;
mod identity;
mod lan_session;
mod protocol;
mod store;

pub use crypto::{open_message, seal_message, OpenedMessage};
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
    KIND_PROFILE_SYNC, KIND_REACTION, KIND_RECEIPT, KIND_TEXT, RECEIPT_TYPE_DELIVERED,
    RECEIPT_TYPE_READ,
};
pub use store::{
    CarriedEnvelope, Contact, ContactDiscoveryPolicy, ContactProvenance, DigestEntry,
    FriendSuggestion, MessageArrival, MessageReference, MessageStore, OutboundEnvelope,
    OutgoingReceiptEnvelope, StoredMessage,
};
