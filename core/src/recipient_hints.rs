//! Recipient-hint aggregation, shared by both platform shells (FA15
//! follow-up, core-first): "which `recipient_hint`s could this user/group
//! match right now" used to be computed independently in Kotlin
//! (`RecipientHints.kt`) and Swift (`MeshController` private helpers), with
//! the day windows mirrored by hand in three places. This module is now the
//! single source of truth; the shells call these exports instead of looping
//! [`compute_recipient_hint`] themselves.
//!
//! A `recipient_hint` is `BLAKE2b-8(UserID || day-number)` where the
//! day-number is the envelope's *creation* day (DESIGN.md §6.4); since
//! envelopes live `DEFAULT_EXPIRY_MS` (7 days), hashing an id against today
//! back through 7 days ago covers every day-salt a still-live envelope could
//! have used. Presence announcements are shorter-lived, hence the separate
//! 3-day window.

use std::collections::HashSet;

use crate::store::MessageStore;
use crate::{compute_recipient_hint, Contact, CoreError, Group, Identity, MS_PER_DAY};
use crate::{RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ};

/// DESIGN.md §5.3 carry window: also used by `engine.rs` (fan-out hint
/// recognition, digest spray) so every hint check in core and both shells
/// agrees on one window.
pub(crate) const CARRY_HINT_DAY_WINDOW_DAYS: i64 = 7;
pub(crate) const PRESENCE_HINT_DAY_WINDOW_DAYS: i64 = 3;

fn hints_over_window(user_id: &[u8], now_ms: i64, window_days: i64) -> Vec<Vec<u8>> {
    (0..=window_days)
        .map(|days_ago| compute_recipient_hint(user_id.to_vec(), now_ms - days_ago * MS_PER_DAY))
        .collect()
}

/// The `recipient_hint`s `user_id` could match for a still-carriable envelope
/// (today back through [`CARRY_HINT_DAY_WINDOW_DAYS`] days).
#[uniffi::export]
pub fn recent_hints_for(user_id: Vec<u8>, now_ms: i64) -> Vec<Vec<u8>> {
    hints_over_window(&user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS)
}

/// Presence-announcement variant of [`recent_hints_for`] over the shorter
/// [`PRESENCE_HINT_DAY_WINDOW_DAYS`] window.
#[uniffi::export]
pub fn recent_presence_hints_for(user_id: Vec<u8>, now_ms: i64) -> Vec<Vec<u8>> {
    hints_over_window(&user_id, now_ms, PRESENCE_HINT_DAY_WINDOW_DAYS)
}

/// Order-preserving content dedupe: a contact hint can coincide with a group
/// hint (or another contact's) on the same day; there's no reason to fetch
/// the same relay page twice.
#[uniffi::export]
pub fn dedupe_hints(hints: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut seen: HashSet<Vec<u8>> = HashSet::with_capacity(hints.len());
    hints.into_iter().filter(|h| seen.insert(h.clone())).collect()
}

#[uniffi::export]
impl MessageStore {
    /// Mail addressed to us: our own hints, plus every imported group we
    /// belong to (DESIGN.md §6.5). NOT deduped -- callers that combine this
    /// with other sets go through [`relay_fetch_hints`] / [`dedupe_hints`].
    /// This narrower set is what the relay *push* subscription uses on iOS
    /// (deliberately without proxy hints -- see `MeshController`'s
    /// `relayPushHints` doc for that platform decision).
    pub fn relay_self_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut hints = hints_over_window(&own_user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS);
        for group in self.list_groups()? {
            if group.member_user_ids.iter().any(|m| *m == own_user_id) {
                hints.extend(hints_over_window(&group.id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS));
            }
        }
        Ok(hints)
    }

    /// Relay proxy-polling hints: the recent-day hints of every contact that
    /// isn't us, so an internet-connected phone in a BLE-only contact's
    /// cluster can fetch mail addressed to *them* out of the shared
    /// family-token partition and mule it the rest of the way. Cost scales
    /// linearly with contact-list size -- fine at family scale.
    pub fn relay_proxy_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut hints = Vec::new();
        for contact in self.list_contacts()? {
            if contact.user_id == own_user_id {
                continue;
            }
            hints.extend(hints_over_window(&contact.user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS));
        }
        Ok(hints)
    }

    /// The full deduped hint set a relay mailbox poll fetches: self + groups
    /// ([`relay_self_hints`]) plus proxy ([`relay_proxy_hints`]).
    pub fn relay_fetch_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut hints = self.relay_self_hints(own_user_id.clone(), now_ms)?;
        hints.extend(self.relay_proxy_hints(own_user_id, now_ms)?);
        Ok(dedupe_hints(hints))
    }

    /// `recipient_hint`s the peer can open: their own userId over recent
    /// days, plus every imported group they belong to (DESIGN.md §6.5:
    /// members mule for the whole group). Drives the HELLO-time carry drain.
    pub fn delivery_hints_for_peer(
        &self,
        peer_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut hints = hints_over_window(&peer_user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS);
        for group in self.list_groups()? {
            if group.member_user_ids.iter().any(|m| *m == peer_user_id) {
                hints.extend(hints_over_window(&group.id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS));
            }
        }
        Ok(hints)
    }

    /// True if `hint` matches a known contact or imported group -- the
    /// family-vs-foreign classification the carry queue's eviction policy
    /// keys on (DESIGN.md §5.3).
    pub fn hint_matches_known_target(
        &self,
        hint: Vec<u8>,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        for contact in self.list_contacts()? {
            if hints_over_window(&contact.user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS)
                .iter()
                .any(|h| *h == hint)
            {
                return Ok(true);
            }
        }
        Ok(self.group_matching_hint(hint, now_ms)?.is_some())
    }

    /// The contact whose recent-day hints include `hint`; failing that, for a
    /// group-addressed hint, the first group member who is a contact (group
    /// carries upload via any member's relay config).
    pub fn contact_matching_hint(
        &self,
        hint: Vec<u8>,
        now_ms: i64,
    ) -> Result<Option<Contact>, CoreError> {
        let contacts = self.list_contacts()?;
        for contact in &contacts {
            if hints_over_window(&contact.user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS)
                .iter()
                .any(|h| *h == hint)
            {
                return Ok(Some(contact.clone()));
            }
        }
        if let Some(group) = self.group_matching_hint(hint, now_ms)? {
            for member_id in &group.member_user_ids {
                if let Some(contact) = contacts.iter().find(|c| c.user_id == *member_id) {
                    return Ok(Some(contact.clone()));
                }
            }
        }
        Ok(None)
    }

    /// The imported group whose recent-day hints include `hint`, if any --
    /// used by the group fan-out upload path
    /// (specs/group-relay-durability.md §4.2), which needs the member list.
    pub fn group_matching_hint(
        &self,
        hint: Vec<u8>,
        now_ms: i64,
    ) -> Result<Option<Group>, CoreError> {
        Ok(self.groups_matching_hint(hint, now_ms)?.into_iter().next())
    }

    /// Every imported group whose recent-day hints include `hint`, in
    /// [`MessageStore::list_groups`] order -- the group-open candidates an
    /// inbound sealed envelope is tried against (a hint collision between two
    /// groups is unlikely but not impossible, so callers try each).
    pub fn groups_matching_hint(
        &self,
        hint: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Group>, CoreError> {
        let mut matches = Vec::new();
        for group in self.list_groups()? {
            if hints_over_window(&group.id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS)
                .iter()
                .any(|h| *h == hint)
            {
                matches.push(group);
            }
        }
        Ok(matches)
    }

    /// Pre-upload receipt backfill for a relay sync pass: for every contact,
    /// refresh the durable relay-uploadable receipt envelope for the current
    /// DELIVERED and READ watermarks (skipping empty streams), exactly the
    /// loop both shells previously ran one `ensure_authored_receipt` call at
    /// a time. Returns the affected envelopes' `msg_id`s so the shell can
    /// record them in its in-memory seen-set (the same reason the shells'
    /// own receipt authoring records there: our own receipt envelope coming
    /// back off the relay must dedupe, not get re-carried as foreign mail).
    pub fn backfill_outgoing_receipt_envelopes(
        &self,
        identity: Identity,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut msg_ids = Vec::new();
        for contact in self.list_contacts()? {
            for receipt_type in [RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ] {
                let through = self.outgoing_receipt_through(
                    contact.user_id.clone(),
                    contact.user_id.clone(),
                    receipt_type,
                )?;
                if through == 0 {
                    continue;
                }
                let authored = self.ensure_authored_receipt(
                    identity.clone(),
                    contact.clone(),
                    contact.user_id.clone(),
                    receipt_type,
                    through,
                    now_ms,
                )?;
                msg_ids.push(authored.envelope.msg_id);
            }
        }
        Ok(msg_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_identity;

    fn contact_for(identity: &Identity, name: &str) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: name.to_string(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn recent_hints_cover_the_full_carry_window_per_day() {
        let hints = recent_hints_for(b"user".to_vec(), NOW);
        assert_eq!(hints.len(), 8);
        for (days_ago, hint) in hints.iter().enumerate() {
            assert_eq!(
                *hint,
                compute_recipient_hint(b"user".to_vec(), NOW - days_ago as i64 * MS_PER_DAY)
            );
        }
    }

    #[test]
    fn presence_hints_use_the_shorter_window() {
        assert_eq!(recent_presence_hints_for(b"user".to_vec(), NOW).len(), 4);
    }

    #[test]
    fn dedupe_preserves_first_occurrence_order() {
        let deduped = dedupe_hints(vec![b"b".to_vec(), b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        assert_eq!(deduped, vec![b"b".to_vec(), b"a".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn relay_fetch_hints_cover_self_groups_and_contacts_without_dupes() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let friend = generate_identity();
        store.upsert_contact(contact_for(&friend, "Friend")).unwrap();
        let group = Group {
            id: b"group-id-0123456".to_vec(),
            name: "Fam".to_string(),
            key: vec![7u8; 32],
            member_user_ids: vec![me.user_id.clone(), friend.user_id.clone()],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        };
        store.upsert_group(group).unwrap();

        let hints = store.relay_fetch_hints(me.user_id.clone(), NOW).unwrap();
        // self (8) + member group (8) + one contact (8), all distinct inputs.
        assert_eq!(hints.len(), 24);
        let self_today = compute_recipient_hint(me.user_id.clone(), NOW);
        let group_today = compute_recipient_hint(b"group-id-0123456".to_vec(), NOW);
        let friend_today = compute_recipient_hint(friend.user_id.clone(), NOW);
        assert!(hints.contains(&self_today));
        assert!(hints.contains(&group_today));
        assert!(hints.contains(&friend_today));
        // A group we are NOT a member of contributes nothing to self hints.
        let outsider = store.relay_self_hints(friend.user_id.clone(), NOW).unwrap();
        assert_eq!(outsider.len(), 16); // friend + the group they're in
    }

    #[test]
    fn proxy_hints_exclude_a_contact_entry_for_ourselves() {
        // Some users import their own card as a contact; proxy polling must
        // not double-fetch our own mailbox (was RelayProxyHintsTests.swift).
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let friend = generate_identity();
        store.upsert_contact(contact_for(&me, "Me")).unwrap();
        store.upsert_contact(contact_for(&friend, "Friend")).unwrap();

        let proxy = store.relay_proxy_hints(me.user_id.clone(), NOW).unwrap();
        assert_eq!(proxy.len(), 8); // friend only
        let own_today = compute_recipient_hint(me.user_id.clone(), NOW);
        assert!(!proxy.contains(&own_today));
    }

    #[test]
    fn known_target_and_matching_lookups_agree_on_the_window_edge() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let friend = generate_identity();
        store.upsert_contact(contact_for(&friend, "Friend")).unwrap();

        let oldest_valid =
            compute_recipient_hint(friend.user_id.clone(), NOW - CARRY_HINT_DAY_WINDOW_DAYS * MS_PER_DAY);
        let expired =
            compute_recipient_hint(friend.user_id.clone(), NOW - (CARRY_HINT_DAY_WINDOW_DAYS + 1) * MS_PER_DAY);
        assert!(store.hint_matches_known_target(oldest_valid.clone(), NOW).unwrap());
        assert!(!store.hint_matches_known_target(expired.clone(), NOW).unwrap());
        assert_eq!(
            store.contact_matching_hint(oldest_valid, NOW).unwrap().map(|c| c.user_id),
            Some(friend.user_id.clone()),
        );
        assert!(store.contact_matching_hint(expired, NOW).unwrap().is_none());
    }

    #[test]
    fn group_hint_resolves_group_and_falls_back_to_member_contact() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let member = generate_identity();
        store.upsert_contact(contact_for(&member, "Member")).unwrap();
        let group = Group {
            id: b"group-id-0123456".to_vec(),
            name: "Fam".to_string(),
            key: vec![7u8; 32],
            member_user_ids: vec![member.user_id.clone(), b"stranger-0123456".to_vec()],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        };
        store.upsert_group(group.clone()).unwrap();

        let group_hint = compute_recipient_hint(group.id.clone(), NOW - 2 * MS_PER_DAY);
        assert_eq!(
            store.group_matching_hint(group_hint.clone(), NOW).unwrap().map(|g| g.id),
            Some(group.id.clone()),
        );
        assert_eq!(store.groups_matching_hint(group_hint.clone(), NOW).unwrap().len(), 1);
        assert!(store.hint_matches_known_target(group_hint.clone(), NOW).unwrap());
        // Group-addressed hint resolves to a member contact for config lookup.
        assert_eq!(
            store.contact_matching_hint(group_hint, NOW).unwrap().map(|c| c.user_id),
            Some(member.user_id),
        );
    }

    #[test]
    fn backfill_skips_empty_streams_and_reuses_stable_msg_ids() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let friend = generate_identity();
        store.upsert_contact(contact_for(&friend, "Friend")).unwrap();

        // Nothing recorded yet: nothing authored.
        assert!(store
            .backfill_outgoing_receipt_envelopes(me.clone(), NOW)
            .unwrap()
            .is_empty());

        store
            .record_outgoing_receipt(
                friend.user_id.clone(),
                friend.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                3,
            )
            .unwrap();
        let first = store.backfill_outgoing_receipt_envelopes(me.clone(), NOW).unwrap();
        assert_eq!(first.len(), 1); // DELIVERED only; READ stream is still empty.

        // Same watermark on a later pass reuses the stored envelope
        // byte-for-byte (stable msg_id), so re-posts dedupe server-side.
        let second = store.backfill_outgoing_receipt_envelopes(me, NOW + 60_000).unwrap();
        assert_eq!(first, second);
    }
}
