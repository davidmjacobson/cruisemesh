//! CruiseMesh core: identity, keys, and friend-card encoding.
//! See DESIGN.md §6.2 (Identity) for the scheme this implements.

uniffi::setup_scaffolding!("cruisemesh_core");

mod crypto;
mod identity;
mod protocol;
mod store;

pub use crypto::{open_message, seal_message, OpenedMessage};
pub use identity::{
    fingerprint_words, generate_identity, make_friend_card, parse_friend_card, Identity,
    FriendCard, CoreError,
};
pub use protocol::{
    decode_message_body, decode_receipt_content, encode_envelope_frame, encode_hello,
    encode_message_body, encode_receipt_content, parse_frame, Frame, MessageBody, ReceiptContent,
    KIND_RECEIPT, KIND_TEXT, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};
pub use store::{Contact, MessageStore, StoredMessage};
