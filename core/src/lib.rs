//! CruiseMesh core: identity, keys, and friend-card encoding.
//! See DESIGN.md §6.2 (Identity) for the scheme this implements.

uniffi::setup_scaffolding!("cruisemesh_core");

mod crypto;
mod gossip;
mod identity;
mod protocol;
mod store;

pub use crypto::{open_message, seal_message, OpenedMessage};
pub use gossip::SeenIds;
pub use identity::{
    fingerprint_words, generate_identity, make_friend_card, parse_friend_card, Identity,
    FriendCard, CoreError,
};
pub use protocol::{
    compute_recipient_hint, decode_message_body, decode_receipt_content, default_expiry,
    encode_digest, encode_envelope_frame, encode_hello, encode_message_body,
    encode_receipt_content, generate_msg_id, parse_frame, Frame, MessageBody, ReceiptContent,
    DEFAULT_EXPIRY_MS, DEFAULT_HOP_TTL, KIND_FRIEND_REQUEST, KIND_RECEIPT, KIND_TEXT,
    RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};
pub use store::{
    CarriedEnvelope, Contact, DigestEntry, MessageStore, OutboundEnvelope,
    OutgoingReceiptEnvelope, StoredMessage,
};
