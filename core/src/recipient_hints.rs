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

/// How far ahead of `now_ms` a relay *push-subscription* hint set reaches --
/// see [`hints_over_range`]. One day covers the UTC day rollover (a socket
/// opened earlier today is still subscribed after midnight) plus modest
/// clock skew; it must stay small since relayd's `MAX_FETCH_HINTS` bounds the
/// subscribed set (see `relay_self_push_hints` / `relay_fetch_push_hints`
/// doc for the budget math).
pub(crate) const PUSH_HINT_FORWARD_DAYS: i64 = 1;

fn hints_over_window(user_id: &[u8], now_ms: i64, window_days: i64) -> Vec<Vec<u8>> {
    hints_over_range(user_id, now_ms, window_days, 0)
}

/// [`hints_over_window`] extended `forward_days` days into the future.
///
/// Envelopes must NEVER be created with a hint from this forward range (see
/// `causal_order.rs`'s module doc: routing time only ever looks backwards) --
/// this exists solely for hint sets that *subscribe* to a relay-push topic,
/// where matching a not-yet-used future hint is harmless (it simply matches
/// nothing until the day rolls over).
fn hints_over_range(
    user_id: &[u8],
    now_ms: i64,
    window_days: i64,
    forward_days: i64,
) -> Vec<Vec<u8>> {
    (-forward_days..=window_days)
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
    hints
        .into_iter()
        .filter(|h| seen.insert(h.clone()))
        .collect()
}

impl MessageStore {
    /// The groups whose mail is addressed to the group id and therefore
    /// arrives under *our* self hints: every group we are a member of.
    fn member_group_ids(&self, own_user_id: &[u8]) -> Result<Vec<Vec<u8>>, CoreError> {
        Ok(self
            .list_groups()?
            .into_iter()
            .filter(|group| group.member_user_ids.iter().any(|m| m == own_user_id))
            .map(|group| group.id)
            .collect())
    }

    /// The ids we proxy-poll a relay for: every contact but ourselves.
    fn proxy_contact_ids(&self, own_user_id: &[u8]) -> Result<Vec<Vec<u8>>, CoreError> {
        Ok(self
            .list_contacts()?
            .into_iter()
            .filter(|contact| contact.user_id != own_user_id)
            .map(|contact| contact.user_id)
            .collect())
    }

    /// Every id this device's relay fetch hints are derived from, unsalted:
    /// our own, the groups we belong to, and the contacts we proxy-poll for.
    ///
    /// The hint builders below salt slices of this same set by day;
    /// [`MessageStore::note_relay_hint_sources`] digests the whole of it raw to
    /// decide when a remembered frontier has gone stale. Enumerating the
    /// sources in one place is what keeps those two answers in step — a hint
    /// source added to the builders later cannot silently escape the digest
    /// and leave the mail it unlocks sitting invisibly below a frontier.
    pub(crate) fn relay_hint_source_ids(
        &self,
        own_user_id: &[u8],
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut ids = vec![own_user_id.to_vec()];
        ids.extend(self.member_group_ids(own_user_id)?);
        ids.extend(self.proxy_contact_ids(own_user_id)?);
        Ok(ids)
    }

    /// Shared by [`Self::relay_self_hints`] (fetch/carry, `forward_days: 0`)
    /// and [`Self::relay_self_push_hints`] (push subscription, `forward_days:
    /// `[`PUSH_HINT_FORWARD_DAYS`]``) so the "own id + member groups" id set
    /// is computed in exactly one place.
    fn self_hints_with_forward(
        &self,
        own_user_id: &[u8],
        now_ms: i64,
        forward_days: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut hints = hints_over_range(
            own_user_id,
            now_ms,
            CARRY_HINT_DAY_WINDOW_DAYS,
            forward_days,
        );
        for group_id in self.member_group_ids(own_user_id)? {
            hints.extend(hints_over_range(
                &group_id,
                now_ms,
                CARRY_HINT_DAY_WINDOW_DAYS,
                forward_days,
            ));
        }
        Ok(hints)
    }

    /// Shared by [`Self::relay_proxy_hints`] and the proxy leg of
    /// [`Self::relay_fetch_push_hints`].
    fn proxy_hints_with_forward(
        &self,
        own_user_id: &[u8],
        now_ms: i64,
        forward_days: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut hints = Vec::new();
        for contact_id in self.proxy_contact_ids(own_user_id)? {
            hints.extend(hints_over_range(
                &contact_id,
                now_ms,
                CARRY_HINT_DAY_WINDOW_DAYS,
                forward_days,
            ));
        }
        Ok(hints)
    }
}

#[uniffi::export]
impl MessageStore {
    /// Mail addressed to us: our own hints, plus every imported group we
    /// belong to (DESIGN.md §6.5). NOT deduped -- callers that combine this
    /// with other sets go through [`relay_fetch_hints`] / [`dedupe_hints`].
    /// This narrower set is what the relay *push* subscription uses on iOS
    /// (deliberately without proxy hints -- see `MeshController`'s
    /// `relayPushHints` doc for that platform decision). For the push
    /// subscription itself, use [`Self::relay_self_push_hints`] instead --
    /// this function's hints must never gain a forward-looking day (see that
    /// function's doc and `causal_order.rs`).
    pub fn relay_self_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        self.self_hints_with_forward(&own_user_id, now_ms, 0)
    }

    /// [`Self::relay_self_hints`] plus one day *ahead* of `now_ms`
    /// ([`PUSH_HINT_FORWARD_DAYS`]) for the same ids -- the hint set the
    /// relay push subscription (not fetch, not carry) should subscribe to.
    ///
    /// Why: `hints_over_window`'s day-salt rotates on the UTC day boundary,
    /// but a push subscription is computed once per socket connect and the
    /// socket then stays open indefinitely (relayd pings keep it alive). A
    /// socket opened at, say, 6pm US time is still open after the UTC
    /// rollover a few hours later, subscribed only to hints that no longer
    /// match anything relayd pushes -- new envelopes silently fall back to
    /// the periodic poll until the next reconnect. Subscribing one day ahead
    /// is safe because it only widens what the *subscription* matches;
    /// envelopes are still ever created with a backward-looking hint (see
    /// `causal_order.rs`'s module doc), so there is nothing for the extra
    /// hint to match until the day actually rolls over.
    pub fn relay_self_push_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        self.self_hints_with_forward(&own_user_id, now_ms, PUSH_HINT_FORWARD_DAYS)
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
        self.proxy_hints_with_forward(&own_user_id, now_ms, 0)
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

    /// [`Self::relay_fetch_hints`] plus one day ahead
    /// ([`PUSH_HINT_FORWARD_DAYS`]) for every id -- the hint set Android's
    /// relay push subscription uses (unlike iOS, Android's push subscription
    /// includes proxy hints, matching its existing `relayFetchHints`-based
    /// fetch; see [`Self::relay_self_push_hints`] for why the forward day is
    /// safe).
    ///
    /// Budget: each id contributes `CARRY_HINT_DAY_WINDOW_DAYS + 1 +
    /// PUSH_HINT_FORWARD_DAYS` = 9 hints (was 8 pre-fix) against relayd's
    /// `MAX_FETCH_HINTS` = 256, so this stays under the cap for up to ~28
    /// combined self/group/contact ids -- comfortably above family scale.
    pub fn relay_fetch_push_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut hints =
            self.self_hints_with_forward(&own_user_id, now_ms, PUSH_HINT_FORWARD_DAYS)?;
        hints.extend(self.proxy_hints_with_forward(
            &own_user_id,
            now_ms,
            PUSH_HINT_FORWARD_DAYS,
        )?);
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
                hints.extend(hints_over_window(
                    &group.id,
                    now_ms,
                    CARRY_HINT_DAY_WINDOW_DAYS,
                ));
            }
        }
        Ok(hints)
    }

    /// True if `hint` matches a known contact or imported group -- the
    /// family-vs-foreign classification the carry queue's eviction policy
    /// keys on (DESIGN.md §5.3).
    pub fn hint_matches_known_target(&self, hint: Vec<u8>, now_ms: i64) -> Result<bool, CoreError> {
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

    /// Group-open candidates for an inbound sealed envelope that failed
    /// pairwise open: [`Self::groups_matching_hint`] plus -- when `hint` is
    /// one of our OWN recent hints -- every imported group. A per-member
    /// relay fan-out row (specs/group-relay-durability.md §4.1) is addressed
    /// to the *member's* hint, not the group's, and nothing outside the
    /// sealed body says which group it belongs to, so an own-hinted envelope
    /// must be tried against every group key this device holds. Both shells'
    /// `tryOpenGroupMessage` call this instead of `groups_matching_hint`;
    /// hint-matching groups stay first so collision-window behavior is
    /// unchanged.
    pub fn group_open_candidates(
        &self,
        hint: Vec<u8>,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Group>, CoreError> {
        let mut candidates = self.groups_matching_hint(hint.clone(), now_ms)?;
        if crate::engine::core_is_own_fanout_hint(hint, own_user_id, now_ms) {
            for group in self.list_groups()? {
                if !candidates.iter().any(|g| g.id == group.id) {
                    candidates.push(group);
                }
            }
        }
        Ok(candidates)
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
        let deduped = dedupe_hints(vec![
            b"b".to_vec(),
            b"a".to_vec(),
            b"b".to_vec(),
            b"c".to_vec(),
        ]);
        assert_eq!(deduped, vec![b"b".to_vec(), b"a".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn relay_fetch_hints_cover_self_groups_and_contacts_without_dupes() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let friend = generate_identity();
        store
            .upsert_contact(contact_for(&friend, "Friend"))
            .unwrap();
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
        store
            .upsert_contact(contact_for(&friend, "Friend"))
            .unwrap();

        let proxy = store.relay_proxy_hints(me.user_id.clone(), NOW).unwrap();
        assert_eq!(proxy.len(), 8); // friend only
        let own_today = compute_recipient_hint(me.user_id.clone(), NOW);
        assert!(!proxy.contains(&own_today));
    }

    #[test]
    fn known_target_and_matching_lookups_agree_on_the_window_edge() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let friend = generate_identity();
        store
            .upsert_contact(contact_for(&friend, "Friend"))
            .unwrap();

        let oldest_valid = compute_recipient_hint(
            friend.user_id.clone(),
            NOW - CARRY_HINT_DAY_WINDOW_DAYS * MS_PER_DAY,
        );
        let expired = compute_recipient_hint(
            friend.user_id.clone(),
            NOW - (CARRY_HINT_DAY_WINDOW_DAYS + 1) * MS_PER_DAY,
        );
        assert!(store
            .hint_matches_known_target(oldest_valid.clone(), NOW)
            .unwrap());
        assert!(!store
            .hint_matches_known_target(expired.clone(), NOW)
            .unwrap());
        assert_eq!(
            store
                .contact_matching_hint(oldest_valid, NOW)
                .unwrap()
                .map(|c| c.user_id),
            Some(friend.user_id.clone()),
        );
        assert!(store.contact_matching_hint(expired, NOW).unwrap().is_none());
    }

    #[test]
    fn group_hint_resolves_group_and_falls_back_to_member_contact() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let member = generate_identity();
        store
            .upsert_contact(contact_for(&member, "Member"))
            .unwrap();
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
            store
                .group_matching_hint(group_hint.clone(), NOW)
                .unwrap()
                .map(|g| g.id),
            Some(group.id.clone()),
        );
        assert_eq!(
            store
                .groups_matching_hint(group_hint.clone(), NOW)
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .hint_matches_known_target(group_hint.clone(), NOW)
            .unwrap());
        // Group-addressed hint resolves to a member contact for config lookup.
        assert_eq!(
            store
                .contact_matching_hint(group_hint, NOW)
                .unwrap()
                .map(|c| c.user_id),
            Some(member.user_id),
        );
    }

    #[test]
    fn group_open_candidates_cover_all_groups_for_an_own_fanout_hint() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let group = |id: &[u8; 16]| Group {
            id: id.to_vec(),
            name: "Fam".to_string(),
            key: vec![7u8; 32],
            member_user_ids: vec![me.user_id.clone()],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        };
        store.upsert_group(group(b"group-id-aaaaaaa")).unwrap();
        store.upsert_group(group(b"group-id-bbbbbbb")).unwrap();

        // A fan-out row is addressed to OUR hint (spec §4.1): no group-id
        // hint matches, but every imported group key must be tried.
        let own_hint = compute_recipient_hint(me.user_id.clone(), NOW - 2 * MS_PER_DAY);
        let candidates = store
            .group_open_candidates(own_hint, me.user_id.clone(), NOW)
            .unwrap();
        assert_eq!(candidates.len(), 2);

        // A group-addressed hint keeps today's behavior: that group alone.
        let group_hint = compute_recipient_hint(b"group-id-aaaaaaa".to_vec(), NOW);
        let candidates = store
            .group_open_candidates(group_hint, me.user_id.clone(), NOW)
            .unwrap();
        assert_eq!(
            candidates.iter().map(|g| g.id.clone()).collect::<Vec<_>>(),
            vec![b"group-id-aaaaaaa".to_vec()],
        );

        // A proxy-fetched contact's hint stays foreign: no candidates.
        let other = generate_identity();
        assert!(store
            .group_open_candidates(
                compute_recipient_hint(other.user_id.clone(), NOW),
                me.user_id.clone(),
                NOW,
            )
            .unwrap()
            .is_empty());

        // Own hint beyond the carry window no longer widens the search.
        let expired = compute_recipient_hint(
            me.user_id.clone(),
            NOW - (CARRY_HINT_DAY_WINDOW_DAYS + 1) * MS_PER_DAY,
        );
        assert!(store
            .group_open_candidates(expired, me.user_id, NOW)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn self_push_hints_add_tomorrow_for_own_id_and_member_groups_only() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let friend = generate_identity();
        store
            .upsert_contact(contact_for(&friend, "Friend"))
            .unwrap();
        let group = Group {
            id: b"group-id-0123456".to_vec(),
            name: "Fam".to_string(),
            key: vec![7u8; 32],
            member_user_ids: vec![me.user_id.clone(), friend.user_id.clone()],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        };
        store.upsert_group(group).unwrap();

        let plain = store.relay_self_hints(me.user_id.clone(), NOW).unwrap();
        let push = store
            .relay_self_push_hints(me.user_id.clone(), NOW)
            .unwrap();
        // One extra hint (tomorrow) per id: self + the one member group.
        assert_eq!(push.len(), plain.len() + 2);

        let self_tomorrow = compute_recipient_hint(me.user_id.clone(), NOW + MS_PER_DAY);
        let group_tomorrow = compute_recipient_hint(b"group-id-0123456".to_vec(), NOW + MS_PER_DAY);
        assert!(push.contains(&self_tomorrow));
        assert!(push.contains(&group_tomorrow));
        // The backward-looking window itself is unchanged for callers that
        // never switched to the push variant.
        for hint in &plain {
            assert!(push.contains(hint));
        }
        assert!(!plain.contains(&self_tomorrow));

        // A contact we're not proxying for contributes nothing here: the
        // push subscription for self hints deliberately excludes proxy
        // hints, same as the pre-existing (non-push) relay_self_hints.
        let friend_tomorrow = compute_recipient_hint(friend.user_id.clone(), NOW + MS_PER_DAY);
        assert!(!push.contains(&friend_tomorrow));
    }

    #[test]
    fn fetch_push_hints_add_tomorrow_for_self_groups_and_proxy_contacts() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let friend = generate_identity();
        store
            .upsert_contact(contact_for(&friend, "Friend"))
            .unwrap();

        let plain = store.relay_fetch_hints(me.user_id.clone(), NOW).unwrap();
        let push = store
            .relay_fetch_push_hints(me.user_id.clone(), NOW)
            .unwrap();
        // self + friend, one extra (tomorrow) hint each, deduped like the
        // plain fetch set.
        assert_eq!(push.len(), plain.len() + 2);

        let self_tomorrow = compute_recipient_hint(me.user_id.clone(), NOW + MS_PER_DAY);
        let friend_tomorrow = compute_recipient_hint(friend.user_id.clone(), NOW + MS_PER_DAY);
        assert!(push.contains(&self_tomorrow));
        assert!(push.contains(&friend_tomorrow));
        for hint in &plain {
            assert!(push.contains(hint));
        }

        // Still deduped: no id contributes the same hint twice even though
        // two windows (backward + forward) are merged.
        let mut sorted = push.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), push.len());
    }

    #[test]
    fn backfill_skips_empty_streams_and_reuses_stable_msg_ids() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let me = generate_identity();
        let friend = generate_identity();
        store
            .upsert_contact(contact_for(&friend, "Friend"))
            .unwrap();

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
        let first = store
            .backfill_outgoing_receipt_envelopes(me.clone(), NOW)
            .unwrap();
        assert_eq!(first.len(), 1); // DELIVERED only; READ stream is still empty.

        // Same watermark on a later pass reuses the stored envelope
        // byte-for-byte (stable msg_id), so re-posts dedupe server-side.
        let second = store
            .backfill_outgoing_receipt_envelopes(me, NOW + 60_000)
            .unwrap();
        assert_eq!(first, second);
    }
}
