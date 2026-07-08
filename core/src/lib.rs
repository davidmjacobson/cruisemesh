//! CruiseMesh core: identity, keys, and friend-card encoding.
//! See DESIGN.md §6.2 (Identity) for the scheme this implements.

uniffi::setup_scaffolding!("cruisemesh_core");

mod crypto;
mod identity;
mod store;

pub use crypto::{open_message, seal_message, OpenedMessage};
pub use identity::{
    fingerprint_words, generate_identity, make_friend_card, parse_friend_card, Identity,
    FriendCard, CoreError,
};
pub use store::{MessageStore, StoredMessage};
