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
//! envelopes live at most `DEFAULT_EXPIRY_MS` (7 days), hashing an id against
//! today back through [`CARRY_HINT_DAY_WINDOW_DAYS`] days ago covers every
//! day-salt a still-live envelope could have used, with one day of slack for
//! clock skew between the authoring and fetching devices. Presence
//! announcements are shorter-lived, hence the separate 3-day window.

use std::collections::HashSet;

use crate::store::MessageStore;
use crate::{compute_recipient_hint, Contact, CoreError, Group, Identity, MS_PER_DAY};
use crate::{RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ};

/// DESIGN.md §5.3 carry window: also used by `engine.rs` (fan-out hint
/// recognition, digest spray) so every hint check in core and both shells
/// agrees on one window.
///
/// Derived from the longest lifetime core ever authors
/// ([`crate::DEFAULT_EXPIRY_MS`], 7 days) plus [`CARRY_HINT_SKEW_DAYS`],
/// rather than written as a bare 7. A `recipient_hint` is salted with the
/// envelope's *creation* day, so a window of exactly the lifetime covers it
/// with no slack at all: a row in the last hours of its life is findable only
/// if the fetching device's UTC day number agrees exactly with the authoring
/// device's, and nothing makes it. The forward side of the push window
/// already buys a day for precisely this reason ([`PUSH_HINT_FORWARD_DAYS`]);
/// the backward side bought none, so a row could sit on a relay, unexpired
/// and undelivered, addressed to a day-salt no device was still asking for.
///
/// The resulting 8 days is also exactly relayd's deposit-class retention
/// ceiling (`MAX_DEPOSIT_RETENTION_MS`), which is the same argument made from
/// the server's side: the honest client's 7-day envelope life, plus one day
/// to absorb clock skew. relayd's
/// `deposit_retention_matches_the_core_carry_hint_window` asserts the two
/// stay equal, so neither can be moved alone.
///
/// Deliberately NOT the 30-day ceilings ([`crate::MAX_CARRY_FUTURE_MS`],
/// relayd's `MAX_RETENTION_MS`). Those bound hostile input; they do not
/// describe a horizon anything authors, since every envelope core creates
/// gets [`crate::DEFAULT_EXPIRY_MS`] or less
/// (`crate::outbound_retirement::authored_delivery_lifetime_ms`). Buying that
/// width costs 31 hints per routing id, so the worst-case family the budget
/// tests below pin — 18 routing ids — would ask for 558 hints (576 on the
/// push window) against [`RELAY_MAX_FETCH_HINTS`] = 256, and
/// [`clamp_hint_groups`] would shed every proxy contact and most groups to
/// cover a lifetime no writer produces. One skew day costs 18 more hints and
/// still leaves 7 spare routing ids.
pub const CARRY_HINT_DAY_WINDOW_DAYS: i64 =
    crate::DEFAULT_EXPIRY_MS / MS_PER_DAY + CARRY_HINT_SKEW_DAYS;

/// The one day [`CARRY_HINT_DAY_WINDOW_DAYS`] adds on top of an envelope's
/// authored lifetime, mirroring [`PUSH_HINT_FORWARD_DAYS`] on the other side
/// of the window. Named rather than inlined so the derivation reads as the
/// argument it is.
const CARRY_HINT_SKEW_DAYS: i64 = 1;
pub(crate) const PRESENCE_HINT_DAY_WINDOW_DAYS: i64 = 3;

/// How far ahead of `now_ms` a relay *push-subscription* hint set reaches --
/// see [`hints_over_range`]. One day covers the UTC day rollover (a socket
/// opened earlier today is still subscribed after midnight) plus modest
/// clock skew; it must stay small since relayd's `MAX_FETCH_HINTS` bounds the
/// subscribed set (see `relay_self_push_hints` / `relay_fetch_push_hints`
/// doc for the budget math).
pub(crate) const PUSH_HINT_FORWARD_DAYS: i64 = 1;

/// relayd's `MAX_FETCH_HINTS` (`relayd/src/lib.rs`), mirrored here because the
/// hint set is *built* in core while the budget it has to fit inside is a
/// server rule. The copy cannot drift silently: relayd's own test suite
/// asserts the two constants are equal.
pub const RELAY_MAX_FETCH_HINTS: usize = 256;
/// Hints one routing id contributes to a fetch or carry set: today, plus
/// [`CARRY_HINT_DAY_WINDOW_DAYS`] days back.
pub const HINTS_PER_ID_FETCH: usize = CARRY_HINT_DAY_WINDOW_DAYS as usize + 1;
/// Hints one routing id contributes to a *push subscription*:
/// [`HINTS_PER_ID_FETCH`] plus [`PUSH_HINT_FORWARD_DAYS`]. The larger of the
/// two, so it is the number every budget argument has to be made against.
pub const HINTS_PER_ID_PUSH: usize = HINTS_PER_ID_FETCH + PUSH_HINT_FORWARD_DAYS as usize;

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

/// [`hints_over_range`] against one device's routing namespace (§7) rather
/// than a bare person or group id. Shared by [`recent_device_hints_for`] and,
/// with a forward day, by the push-subscription budget the tests pin.
pub(crate) fn device_hints_over_range(
    person_id: &[u8],
    device_id: &[u8],
    now_ms: i64,
    forward_days: i64,
) -> Vec<Vec<u8>> {
    hints_over_range(
        &crate::core_device_namespace_id(person_id.to_vec(), device_id.to_vec()),
        now_ms,
        CARRY_HINT_DAY_WINDOW_DAYS,
        forward_days,
    )
}

/// [`recent_hints_for`] for one device of a person (§7): the `recipient_hint`s
/// a still-carriable relay row addressed to that *device* could match.
///
/// A legacy device id — and the absent field §5 maps to it — yields exactly
/// [`recent_hints_for`]'s person hints, because
/// [`core_device_namespace_id`](crate::core_device_namespace_id) falls back to
/// the person namespace there. That is what keeps a mixed fleet's fetch set a
/// superset of today's: nothing a v1 peer can address stops being fetched.
#[uniffi::export]
pub fn recent_device_hints_for(
    person_id: Vec<u8>,
    device_id: Vec<u8>,
    now_ms: i64,
) -> Vec<Vec<u8>> {
    device_hints_over_range(&person_id, &device_id, now_ms, 0)
}

/// Where a fetched relay row's `recipient_hint` falls relative to this
/// device's own fleet (§7) — the terms the ACK-MD rules are stated in, and the
/// only thing about a row's addressing an ack decision may read.
///
/// Every variant is about *this* person's mail. A row addressed to a contact
/// or to a group is [`FleetHint::Foreign`] here and keeps being judged by the
/// rules that already own it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FleetHint {
    /// A row in this device's own namespace: the one namespace it may delete
    /// from (ACK-MD-1). On an install that has never linked this is the bare
    /// person hint, because that device *is* the person's only endpoint.
    OwnDevice,
    /// The bare person hint of a fleet that holds more than one device: the
    /// single row a legacy sender uploads, which the siblings still need
    /// (ACK-MD-2).
    PersonShared,
    /// A sibling device's namespace. Openable here — §6's inbox key is
    /// person-scoped — which is exactly why the refusal must be by namespace
    /// and cannot be left to whether the seal happened to open (ACK-MD-1).
    Sibling,
    /// Anything else: a proxy-polled contact's hint, a group's, a row fetched
    /// under a hint this device no longer recognizes.
    Foreign,
}

/// This device's fleet expressed as hint sets, built once per ack pass.
///
/// Sets rather than repeated derivations: a pass classifies every fetched row
/// against all three, and re-deriving `HINTS_PER_ID_FETCH` hashes per device
/// per row would make a max-cap fleet's poll quadratic in the page size.
pub(crate) struct FleetHints {
    own: HashSet<Vec<u8>>,
    person_shared: HashSet<Vec<u8>>,
    sibling: HashSet<Vec<u8>>,
}

impl FleetHints {
    /// The own namespace is tested first, always. The sets are disjoint in
    /// practice, but the order is the rule: no later test may pull a row out
    /// of the one namespace this device is allowed to delete from, and none
    /// may push a row into it.
    pub(crate) fn classify(&self, hint: &[u8]) -> FleetHint {
        if self.own.contains(hint) {
            FleetHint::OwnDevice
        } else if self.sibling.contains(hint) {
            FleetHint::Sibling
        } else if self.person_shared.contains(hint) {
            FleetHint::PersonShared
        } else {
            FleetHint::Foreign
        }
    }
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

    /// The routing namespace of THIS device (§7) — at most one id, and none at
    /// all until §9's activation has told this store which device it is.
    ///
    /// **Own namespace only, deliberately (design decision, WP2 review).** An
    /// earlier draft subscribed to every sibling's namespace as well, on the
    /// analogy with [`Self::proxy_contact_ids`]: a relay-reachable device
    /// proxying for one that is not. That analogy does not survive §6. The
    /// inbox key is person-scoped, so a sibling's row *opens* here — fetching it
    /// therefore manufactures a second delivery of the same logical message on
    /// this device, and does it on every device of the fleet, multiplying relay
    /// traffic and the family's byte budget by the fleet size. It also walks
    /// straight into the hazard [`FleetHint::Sibling`] exists to defend against,
    /// paying a real cost to create a situation the ack planner then has to
    /// refuse. A sibling's row is fetched by the sibling; the *content*
    /// converges by §8 self-sync, which is WP4's job, not the fetch set's.
    ///
    /// [`FleetHint::Sibling`] stays in the planner regardless: a sibling row can
    /// still arrive by other paths (a carried copy, a cursor reset, a hint
    /// collision), and it must never be acked when it does.
    ///
    /// Empty on every install that has never linked, so every hint set built
    /// from it is byte-identical to the one built before §7 existed.
    fn own_device_namespace_ids(&self, own_user_id: &[u8]) -> Result<Vec<Vec<u8>>, CoreError> {
        Ok(self
            .own_device_fleet()?
            .own_device_id
            .map(|device_id| crate::core_device_namespace_id(own_user_id.to_vec(), device_id))
            .into_iter()
            .collect())
    }

    /// Every id this device's relay fetch hints are derived from, unsalted:
    /// our own, this device's own §7 namespace, the groups we belong to, and
    /// the contacts we proxy-poll for.
    ///
    /// The hint builders below salt slices of this same set by day;
    /// [`MessageStore::note_relay_hint_sources`] digests the whole of it raw to
    /// decide when a remembered frontier has gone stale. Enumerating the
    /// sources in one place is what keeps those two answers in step — a hint
    /// source added to the builders later cannot silently escape the digest
    /// and leave the mail it unlocks sitting invisibly below a frontier.
    /// Linking a device is exactly such an addition, which is why this device's
    /// own namespace is listed here and not only in the builders: once §9's
    /// activation names this device, its rows are addressed under an id this
    /// store has never fetched under, so the remembered frontier has to move
    /// back for them.
    ///
    /// Pinned consequence of the own-namespace-only decision (see
    /// [`Self::own_device_namespace_ids`]): on an install with no stored fleet
    /// this returns exactly `[own_user_id]` plus groups and proxy contacts —
    /// byte-identical to the pre-§7 list, so nothing in the field invalidates a
    /// remembered frontier merely by taking this build.
    /// `an_unlinked_install_fetches_exactly_todays_hints` pins that.
    pub(crate) fn relay_hint_source_ids(
        &self,
        own_user_id: &[u8],
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut ids = vec![own_user_id.to_vec()];
        ids.extend(self.own_device_namespace_ids(own_user_id)?);
        ids.extend(self.member_group_ids(own_user_id)?);
        ids.extend(self.proxy_contact_ids(own_user_id)?);
        Ok(ids)
    }

    /// This device's own fleet (§7) resolved into the three hint sets
    /// [`FleetHints::classify`] answers from.
    ///
    /// The window is the BACKWARD-only [`CARRY_HINT_DAY_WINDOW_DAYS`] one, for
    /// the reason [`MessageStore::core_relay_ack_ids_with_consumed`] states at
    /// length: these are claims about envelopes that already exist, and an
    /// envelope is only ever created with a backward-looking hint. The
    /// forward-looking push variants belong to subscribing, never to judging.
    ///
    /// An install that has never linked reads
    /// [`crate::OwnDeviceFleet::default`], whose own device id is `None` and
    /// therefore resolves through
    /// [`core_device_namespace_id`](crate::core_device_namespace_id)'s legacy
    /// fallback to the bare person hints — the same set
    /// [`crate::core_is_own_fanout_hint`] matches, with no sibling or shared
    /// set to withhold anything. That is what makes today's fleet's behaviour
    /// byte-identical after this rule lands.
    ///
    /// The person's own hints only become [`FleetHint::PersonShared`] once a
    /// second device is actually held; below that they belong to
    /// [`FleetHint::OwnDevice`], linked or not. A single-device person IS the
    /// sole true consumer of a person-addressed row, so withholding its ack
    /// would leave every legacy sender's row sitting until expiry for no one;
    /// ACK-MD-2 is about leaving a copy for a sibling, and a fleet of one has
    /// no sibling. The window in which this device could be wrong about that
    /// is closed by §9's two-phase activation rather than by luck: the
    /// approving device writes the new fleet when it signs the roster, and the
    /// joining device may not act at all until it has imported that roster and
    /// confirmed it — so no device is ever unaware of a sibling that is
    /// already able to receive.
    pub(crate) fn own_fleet_hints(
        &self,
        own_person_id: &[u8],
        now_ms: i64,
    ) -> Result<FleetHints, CoreError> {
        let fleet = self.own_device_fleet()?;
        let own_device_id = fleet
            .own_device_id
            .unwrap_or_else(|| crate::LEGACY_DEVICE_ID.to_vec());
        let mut own: HashSet<Vec<u8>> =
            device_hints_over_range(own_person_id, &own_device_id, now_ms, 0)
                .into_iter()
                .collect();
        if fleet.device_ids.len() < 2 {
            // A fleet of one owns the person's hints outright, linked or not:
            // it is the person's only endpoint, so a person-addressed row has
            // exactly one true consumer and every rule below should see it as
            // this device's own. (Unlinked, the extend is a no-op -- the legacy
            // fallback already derived these same hints.)
            own.extend(hints_over_window(
                own_person_id,
                now_ms,
                CARRY_HINT_DAY_WINDOW_DAYS,
            ));
            return Ok(FleetHints {
                own,
                person_shared: HashSet::new(),
                sibling: HashSet::new(),
            });
        }
        Ok(FleetHints {
            own,
            person_shared: hints_over_window(own_person_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS)
                .into_iter()
                .collect(),
            sibling: fleet
                .device_ids
                .iter()
                .filter(|device_id| **device_id != own_device_id)
                .flat_map(|device_id| device_hints_over_range(own_person_id, device_id, now_ms, 0))
                .collect(),
        })
    }

    /// Shared by [`Self::relay_self_hints`] (fetch/carry, `forward_days: 0`)
    /// and [`Self::relay_self_push_hints`] (push subscription, `forward_days:
    /// `[`PUSH_HINT_FORWARD_DAYS`]``) so the "own id + this device's namespace
    /// + member groups" id set is computed in exactly one place.
    ///
    /// The person's own hints stay in the set on a linked fleet as well as an
    /// unlinked one, and must: a legacy sender uploads exactly one
    /// person-addressed row for the whole person (ACK-MD-2), and a device that
    /// stopped fetching under the person's hints would never see it at all.
    /// §7's namespace is added beside them, never instead of them — and only
    /// THIS device's (see [`Self::own_device_namespace_ids`] for why no
    /// sibling's namespace is subscribed to).
    ///
    /// Clamped, in the priority order [`Self::relay_hint_groups`] documents.
    fn self_hints_with_forward(
        &self,
        own_user_id: &[u8],
        now_ms: i64,
        forward_days: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        Ok(clamp_hint_groups(self.relay_hint_groups(
            own_user_id,
            now_ms,
            forward_days,
            false,
        )?))
    }

    /// Shared by [`Self::relay_proxy_hints`] and the proxy leg of
    /// [`Self::relay_fetch_push_hints`].
    fn proxy_hints_with_forward(
        &self,
        own_user_id: &[u8],
        now_ms: i64,
        forward_days: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        // §9.4: muling for contacts is mesh work like any other.
        if !self.link_gate_allows(crate::device_link::activation::CoreLinkGatedAction::Advertise)? {
            return Ok(Vec::new());
        }
        let mut groups = Vec::new();
        for contact_id in self.proxy_contact_ids(own_user_id)? {
            groups.push(hints_over_range(
                &contact_id,
                now_ms,
                CARRY_HINT_DAY_WINDOW_DAYS,
                forward_days,
            ));
        }
        Ok(clamp_hint_groups(groups))
    }

    /// Every routing id this device fetches under, resolved to its day-salted
    /// hints and grouped one Vec per id, in **priority order**:
    ///
    /// 1. the person's own id — a legacy sender's single row is only findable
    ///    here (ACK-MD-2), and losing it loses mail no other id can recover;
    /// 2. this device's own §7 namespace — the rows this device is the sole
    ///    true consumer of, and the only rows it is allowed to delete;
    /// 3. the groups this device is a member of — shared mail, still reachable
    ///    through any other member's fan-out row if this one is shed;
    /// 4. proxy-polled contacts — a courtesy fetch on someone else's behalf,
    ///    and the only class whose loss costs this device's own user nothing.
    ///
    /// The order is the shedding order [`clamp_hint_groups`] applies when the
    /// combined set would exceed [`RELAY_MAX_FETCH_HINTS`]. It is a safety net,
    /// not a working regime: the budget tests assert a realistic worst case
    /// fits with room to spare, and the clamp exists so that a store which
    /// somehow grows past the cap degrades by dropping the least valuable
    /// subscriptions rather than by having the whole request rejected by relayd.
    fn relay_hint_groups(
        &self,
        own_user_id: &[u8],
        now_ms: i64,
        forward_days: i64,
        include_proxy: bool,
    ) -> Result<Vec<Vec<Vec<u8>>>, CoreError> {
        // §9.4: "invisible on the mesh", at the one place every relay hint set
        // is built. A hint set is how this device tells a relay which mailboxes
        // are its business — to fetch from, to subscribe to, to proxy for — so
        // a device that has not finished being adopted publishes none of them
        // and asks for nothing. Emptiness, not an error: a poll pass with no
        // hints is a pass that fetches nothing, which is exactly the intent.
        if !self.link_gate_allows(crate::device_link::activation::CoreLinkGatedAction::Advertise)? {
            return Ok(Vec::new());
        }
        let mut groups = vec![hints_over_range(
            own_user_id,
            now_ms,
            CARRY_HINT_DAY_WINDOW_DAYS,
            forward_days,
        )];
        if let Some(device_id) = self.own_device_fleet()?.own_device_id {
            groups.push(device_hints_over_range(
                own_user_id,
                &device_id,
                now_ms,
                forward_days,
            ));
        }
        for group_id in self.member_group_ids(own_user_id)? {
            groups.push(hints_over_range(
                &group_id,
                now_ms,
                CARRY_HINT_DAY_WINDOW_DAYS,
                forward_days,
            ));
        }
        if include_proxy {
            for contact_id in self.proxy_contact_ids(own_user_id)? {
                groups.push(hints_over_range(
                    &contact_id,
                    now_ms,
                    CARRY_HINT_DAY_WINDOW_DAYS,
                    forward_days,
                ));
            }
        }
        Ok(groups)
    }
}

/// Flatten per-id hint groups, dropping whole ids from the tail once the next
/// one would take the set past [`RELAY_MAX_FETCH_HINTS`].
///
/// Whole ids, never partial ones: half an id's day window is a subscription
/// that silently misses mail authored on the missing days, which is worse than
/// not subscribing at all. Counted on the raw hints rather than the deduped
/// ones so the answer does not depend on an accidental hint collision between
/// two ids; the deduped set a caller finally submits can only be smaller.
///
/// Stopping at the first id that does not fit *is* "shed the lowest priority
/// first": every id costs the same number of hints, so nothing later could have
/// fitted where an earlier one did not.
fn clamp_hint_groups(groups: Vec<Vec<Vec<u8>>>) -> Vec<Vec<u8>> {
    let mut hints: Vec<Vec<u8>> = Vec::new();
    for group in groups {
        if hints.len() + group.len() > RELAY_MAX_FETCH_HINTS {
            break;
        }
        hints.extend(group);
    }
    hints
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
    ///
    /// **Known gap, stated rather than hidden: proxy hints are PERSON-level
    /// only.** A contact's §7 per-device rows are addressed to
    /// [`crate::core_device_namespace_id`] namespaces this set says nothing
    /// about, so in WP2 a proxy cannot fetch or mule them; only the contact's
    /// own devices can. That is deliberate for three reasons. It keeps this
    /// cost linear in contacts rather than in contacts × devices, which is
    /// what the [`RELAY_MAX_FETCH_HINTS`] budget has room for. It keeps a
    /// proxy from becoming a second reader of a row whose whole point is
    /// having exactly one. And it costs nothing today, because no production
    /// writer creates a fleet larger than one device yet (WP3's linking
    /// ceremony), so no per-device row exists in the field to be missed.
    /// Extending proxy polling to a contact's device namespaces belongs to
    /// WP4/WP5, when real fleets exist and the budget can be re-argued
    /// against real contact-list sizes; §7's fan-out is unaffected either way.
    pub fn relay_proxy_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        self.proxy_hints_with_forward(&own_user_id, now_ms, 0)
    }

    /// The full deduped hint set a relay mailbox poll fetches: self + groups
    /// ([`relay_self_hints`]) plus proxy ([`relay_proxy_hints`]).
    ///
    /// Built from one clamped id list rather than by concatenating two
    /// separately clamped ones, so the [`RELAY_MAX_FETCH_HINTS`] budget is
    /// argued about the set that is actually submitted.
    pub fn relay_fetch_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        Ok(dedupe_hints(clamp_hint_groups(self.relay_hint_groups(
            &own_user_id,
            now_ms,
            0,
            true,
        )?)))
    }

    /// [`Self::relay_fetch_hints`] plus one day ahead
    /// ([`PUSH_HINT_FORWARD_DAYS`]) for every id -- the hint set Android's
    /// relay push subscription uses (unlike iOS, Android's push subscription
    /// includes proxy hints, matching its existing `relayFetchHints`-based
    /// fetch; see [`Self::relay_self_push_hints`] for why the forward day is
    /// safe).
    ///
    /// Budget: each id contributes [`HINTS_PER_ID_PUSH`] = 10 hints against
    /// relayd's [`RELAY_MAX_FETCH_HINTS`] = 256, so this stays under the cap
    /// for up to 25 combined ids -- comfortably above family scale.
    /// `specs/multi-device-v1.md` §7 spends exactly ONE of those ids: a
    /// device subscribes to its own namespace and to no sibling's (see
    /// [`MessageStore::own_device_namespace_ids`]), whatever the fleet's size,
    /// which leaves 23 for groups and proxy-polled contacts.
    /// `the_combined_fetch_budget_of_a_worst_case_family_fits` pins the
    /// arithmetic through these shipped builders; this doc is only its summary.
    pub fn relay_fetch_push_hints(
        &self,
        own_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        Ok(dedupe_hints(clamp_hint_groups(self.relay_hint_groups(
            &own_user_id,
            now_ms,
            PUSH_HINT_FORWARD_DAYS,
            true,
        )?)))
    }

    /// `recipient_hint`s the peer can open: their own userId over recent
    /// days, plus every imported group they belong to (DESIGN.md §6.5:
    /// members mule for the whole group). Drives the HELLO-time carry drain.
    pub fn delivery_hints_for_peer(
        &self,
        peer_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        // §9.4: this set drives the HELLO-time carry drain — offering mail to a
        // peer that just met us. A device still being adopted offers nothing,
        // because it should not have been in that encounter at all.
        if !self.link_gate_allows(crate::device_link::activation::CoreLinkGatedAction::Advertise)? {
            return Ok(Vec::new());
        }
        let mut hints = hints_over_window(&peer_user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS);
        for group in self.list_groups()? {
            if group.member_user_ids.contains(&peer_user_id) {
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
    ///
    /// **Person-level, and blind to §7 device namespaces.** This and its
    /// siblings ([`Self::contact_matching_hint`],
    /// [`Self::group_open_candidates`], and the relay pass's hint resolution)
    /// resolve a hint by hashing known PERSON and GROUP ids; a hint derived
    /// from [`crate::core_device_namespace_id`] matches none of them, so a
    /// contact's per-device row read as a carried frame classifies as foreign
    /// traffic and is evicted before family traffic would be. That is
    /// harmless in WP2 — no production writer creates a fleet larger than one
    /// device, so no such row exists in the field — and
    /// `a_contacts_device_namespaced_hint_is_still_invisible_here` pins the
    /// blindness so it is a discovered fact rather than a surprise. TODO
    /// (WP3/WP4): once §9's linking ceremony makes real fleets and §8's
    /// self-sync makes rows worth muling, resolve a contact's device
    /// namespaces here too, budgeting the extra hashes per contact.
    pub fn hint_matches_known_target(&self, hint: Vec<u8>, now_ms: i64) -> Result<bool, CoreError> {
        for contact in self.list_contacts()? {
            if hints_over_window(&contact.user_id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS)
                .contains(&hint)
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
                .contains(&hint)
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
            if hints_over_window(&group.id, now_ms, CARRY_HINT_DAY_WINDOW_DAYS).contains(&hint) {
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
        let own_user_id = identity.user_id.clone();
        for group in self.list_groups()? {
            if !group.member_user_ids.iter().any(|m| m == &own_user_id) {
                continue;
            }
            for member_id in group.member_user_ids {
                if member_id == own_user_id {
                    continue;
                }
                let Some(author) = self.get_contact(member_id.clone())? else {
                    continue;
                };
                for receipt_type in [RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ] {
                    let through = self.outgoing_receipt_through(
                        group.id.clone(),
                        author.user_id.clone(),
                        receipt_type,
                    )?;
                    if through == 0 {
                        continue;
                    }
                    let authored = self.ensure_authored_group_receipt(
                        identity.clone(),
                        author.clone(),
                        group.id.clone(),
                        receipt_type,
                        through,
                        now_ms,
                    )?;
                    msg_ids.push(authored.envelope.msg_id);
                }
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

    /// The two constants that must never drift apart: how long an envelope
    /// lives, and how far back a fetch/carry/classification sweep still asks
    /// for it. If the lifetime ever grows past the window — or the window is
    /// trimmed back to the lifetime — mail sits on a relay, unexpired and
    /// undelivered, addressed to a day-salt nothing is asking for.
    #[test]
    fn the_carry_hint_window_outlasts_every_envelope_core_authors() {
        let longest_authored_days = crate::DEFAULT_EXPIRY_MS / MS_PER_DAY;
        assert!(
            CARRY_HINT_DAY_WINDOW_DAYS > longest_authored_days,
            "the hint window ({CARRY_HINT_DAY_WINDOW_DAYS} days) has to outlast \
             the longest authored envelope ({longest_authored_days} days) by at \
             least a day, because the authoring and fetching devices need not \
             agree on the UTC day number"
        );
        // The numbers themselves, so moving either is a deliberate act.
        assert_eq!(longest_authored_days, 7);
        assert_eq!(CARRY_HINT_DAY_WINDOW_DAYS, 8);

        // That lifetime really is the horizon: no kind authors longer than
        // the default, and the one that differs is deliberately shorter.
        for kind in [
            crate::KIND_TEXT,
            crate::KIND_RECEIPT,
            crate::KIND_FRIEND_REQUEST,
            crate::KIND_GROUP_INVITE,
            crate::KIND_ROSTER_GOSSIP,
            crate::KIND_LAN_ENDPOINT_HINT,
        ] {
            assert!(
                crate::authored_delivery_lifetime_ms(kind) <= crate::DEFAULT_EXPIRY_MS,
                "kind {kind} authors a longer life than the hint window covers"
            );
        }

        // And behaviourally, not just arithmetically: an envelope authored a
        // whole lifetime ago — the last instant it can still be on a relay —
        // is still asked for, and so is one a further skew day older.
        let id = b"user".to_vec();
        let hints = recent_hints_for(id.clone(), NOW);
        for age_ms in [
            crate::DEFAULT_EXPIRY_MS,
            crate::DEFAULT_EXPIRY_MS + MS_PER_DAY,
        ] {
            assert!(
                hints.contains(&compute_recipient_hint(id.clone(), NOW - age_ms)),
                "a hint {age_ms} ms old is no longer asked for"
            );
        }
    }

    #[test]
    fn recent_hints_cover_the_full_carry_window_per_day() {
        let hints = recent_hints_for(b"user".to_vec(), NOW);
        assert_eq!(hints.len(), HINTS_PER_ID_FETCH);
        assert_eq!(hints.len(), 9);
        for (days_ago, hint) in hints.iter().enumerate() {
            assert_eq!(
                *hint,
                compute_recipient_hint(b"user".to_vec(), NOW - days_ago as i64 * MS_PER_DAY)
            );
        }
    }

    // -- per-device namespaces (specs/multi-device-v1.md §7) ---------------

    #[test]
    fn device_hints_cover_the_same_window_in_their_own_namespace() {
        let person = vec![0x01; 16];
        let device = vec![0x02; 16];
        let hints = recent_device_hints_for(person.clone(), device.clone(), NOW);
        assert_eq!(hints.len(), HINTS_PER_ID_FETCH);
        let namespace = crate::core_device_namespace_id(person.clone(), device);
        for (days_ago, hint) in hints.iter().enumerate() {
            assert_eq!(
                *hint,
                compute_recipient_hint(namespace.clone(), NOW - days_ago as i64 * MS_PER_DAY)
            );
        }
        // A device's rows are not findable under the bare person hint, which
        // is the whole point of ACK-MD-1 having a namespace to name.
        assert!(hints
            .iter()
            .all(|hint| !recent_hints_for(person.clone(), NOW).contains(hint)));
    }

    #[test]
    fn each_device_of_a_person_gets_its_own_hints() {
        let person = vec![0x01; 16];
        let a = recent_device_hints_for(person.clone(), vec![0x02; 16], NOW);
        let b = recent_device_hints_for(person, vec![0x03; 16], NOW);
        assert!(a.iter().all(|hint| !b.contains(hint)));
    }

    /// §5 / ACK-MD-2: the legacy device id resolves to the person namespace,
    /// so a v1 sender's single person-addressed row is still found by exactly
    /// the hints today's fetch already carries.
    #[test]
    fn a_legacy_device_id_resolves_to_the_person_hints() {
        let person = vec![0x01; 16];
        assert_eq!(
            recent_device_hints_for(person.clone(), crate::LEGACY_DEVICE_ID.to_vec(), NOW),
            recent_hints_for(person.clone(), NOW),
        );
        assert_eq!(
            recent_device_hints_for(person.clone(), Vec::new(), NOW),
            recent_hints_for(person, NOW),
        );
    }

    /// A store at the worst case a family can realistically reach, built the
    /// way a phone builds it: a device of a max-cap (§14.3) fleet, in a
    /// family-scale contact list, in several groups.
    ///
    /// `contacts` and `groups` are the two dimensions that actually grow; the
    /// fleet contributes ONE id however large it is, which is the whole point
    /// of the own-namespace-only decision.
    fn worst_case_store(person: &Identity, contacts: usize, groups: usize) -> MessageStore {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let device_ids: Vec<Vec<u8>> = (0..crate::DEVICE_HARD_CAP)
            .map(|index| vec![index as u8 + 1; crate::DEVICE_ID_LEN])
            .collect();
        store
            .set_own_device_fleet(crate::OwnDeviceFleet {
                own_device_id: device_ids.first().cloned(),
                device_ids,
                projected_from: crate::RosterVersion {
                    recovery_epoch: 0,
                    seq: 1,
                },
            })
            .unwrap();
        for index in 0..contacts {
            let friend = generate_identity();
            store
                .upsert_contact(contact_for(&friend, &format!("Friend {index}")))
                .unwrap();
        }
        for index in 0..groups {
            let mut id = b"group-id-00000000".to_vec();
            id.truncate(16);
            id[15] = index as u8;
            store
                .upsert_group(Group {
                    id,
                    name: format!("Group {index}"),
                    key: vec![7u8; 32],
                    member_user_ids: vec![person.user_id.clone()],
                    metadata_revision: 0,
                    metadata_changed_by: Vec::new(),
                })
                .unwrap();
        }
        store
    }

    /// §13's WP2 gate, argued about the COMBINED set a phone submits rather
    /// than about the fleet in isolation: a max-cap fleet plus a family-scale
    /// contact list plus groups stays under relayd's `MAX_FETCH_HINTS`.
    ///
    /// Push is the larger of the two windows, so the budget is argued against
    /// it, and the numbers are asserted exactly rather than as a bound: this
    /// test's job is to fail loudly the day a day-window, a cap, or the fan-out
    /// widens, not to quietly absorb it.
    ///
    /// It also pins the two halves §7 owes the fetch set: the person's own
    /// hints are still there (a legacy sender's one row is only findable under
    /// them, ACK-MD-2), and this device's own namespace is there beside them —
    /// and no sibling's is, which is the own-namespace-only decision as an
    /// assertion rather than as a comment.
    #[test]
    fn the_combined_fetch_budget_of_a_worst_case_family_fits() {
        let person = generate_identity();
        // Family scale, chosen high rather than typical: 12 contacts is a large
        // family plus friends-of-friends, and 4 groups is more than the app's
        // own onboarding creates.
        let contacts = 12;
        let groups = 4;
        let store = worst_case_store(&person, contacts, groups);

        let hints = store
            .relay_fetch_push_hints(person.user_id.clone(), NOW)
            .unwrap();

        // 1 person + 1 own device namespace + 4 groups + 12 proxy contacts.
        let routing_ids = 1 + 1 + groups + contacts;
        assert_eq!(HINTS_PER_ID_PUSH, 10);
        assert_eq!(routing_ids, 18);
        assert_eq!(hints.len(), routing_ids * HINTS_PER_ID_PUSH);
        assert_eq!(hints.len(), 180);
        assert!(hints.len() <= RELAY_MAX_FETCH_HINTS);
        // Headroom, stated so a later work package can see what it is
        // spending: 7 more routing ids fit beside this worst case.
        assert_eq!((RELAY_MAX_FETCH_HINTS - hints.len()) / HINTS_PER_ID_PUSH, 7);

        for hint in hints_over_range(
            &person.user_id,
            NOW,
            CARRY_HINT_DAY_WINDOW_DAYS,
            PUSH_HINT_FORWARD_DAYS,
        ) {
            assert!(hints.contains(&hint), "the person's own hints stay");
        }
        let own_device_id = vec![1u8; crate::DEVICE_ID_LEN];
        for hint in
            device_hints_over_range(&person.user_id, &own_device_id, NOW, PUSH_HINT_FORWARD_DAYS)
        {
            assert!(hints.contains(&hint), "this device's own namespace");
        }
        // A sibling's namespace is NOT subscribed to: its rows are the
        // sibling's to fetch, and §8 self-sync (WP4) is what converges the
        // content. Fetching them here would duplicate delivery on every device
        // of the fleet and multiply the family's relay bill by the fleet size.
        for device_index in 1..crate::DEVICE_HARD_CAP {
            let sibling = vec![device_index as u8 + 1; crate::DEVICE_ID_LEN];
            for hint in
                device_hints_over_range(&person.user_id, &sibling, NOW, PUSH_HINT_FORWARD_DAYS)
            {
                assert!(!hints.contains(&hint), "no sibling namespace is fetched");
            }
        }

        // And the frontier stays honest: activating this device adds a routing
        // id this store has never fetched under, so the remembered frontier has
        // to move back for it rather than hiding its mail below it.
        assert_eq!(
            store.relay_hint_source_ids(&person.user_id).unwrap().len(),
            routing_ids
        );
    }

    /// The clamp is a safety net, so it is tested by deliberately overrunning
    /// the cap: it must shed from the bottom of
    /// [`MessageStore::relay_hint_groups`]'s priority order, never from the top.
    #[test]
    fn the_hint_budget_clamp_sheds_the_lowest_priority_ids_first() {
        let person = generate_identity();
        // 1 person + 1 own device + 4 groups + 40 contacts = 46 ids × 10 =
        // 460, comfortably past the 256 cap.
        let store = worst_case_store(&person, 40, 4);

        let hints = store
            .relay_fetch_push_hints(person.user_id.clone(), NOW)
            .unwrap();
        assert!(hints.len() <= RELAY_MAX_FETCH_HINTS);
        // Whole ids only: 25 of them fit, and the 26th is shed entire.
        assert_eq!(hints.len(), 25 * HINTS_PER_ID_PUSH);
        assert_eq!(hints.len(), 250);

        // The two ids that must never be shed, and the groups above the
        // proxy contacts.
        for hint in hints_over_range(
            &person.user_id,
            NOW,
            CARRY_HINT_DAY_WINDOW_DAYS,
            PUSH_HINT_FORWARD_DAYS,
        ) {
            assert!(hints.contains(&hint), "the person's own id is never shed");
        }
        for hint in device_hints_over_range(
            &person.user_id,
            &[1u8; crate::DEVICE_ID_LEN],
            NOW,
            PUSH_HINT_FORWARD_DAYS,
        ) {
            assert!(
                hints.contains(&hint),
                "this device's namespace is never shed"
            );
        }
        for index in 0..4u8 {
            let mut id = b"group-id-00000000".to_vec();
            id.truncate(16);
            id[15] = index;
            assert!(
                hints.contains(&compute_recipient_hint(id, NOW)),
                "member groups outrank proxy contacts"
            );
        }
        // And the shedding really happened: some proxy contact lost its
        // subscription, which is the cost the priority order chooses to pay.
        let proxied = store
            .list_contacts()
            .unwrap()
            .into_iter()
            .filter(|contact| hints.contains(&compute_recipient_hint(contact.user_id.clone(), NOW)))
            .count();
        assert_eq!(proxied, 25 - 1 - 1 - 4);
    }

    /// The fetch window and the classification window, pinned together: every
    /// hint `relay_self_hints` returns on a linked fleet is one this device's
    /// own fleet classification recognizes as its own or its person's. A
    /// `Foreign` hint in the fetch set would be a row this device downloads and
    /// then judges by rules that were never written about it.
    ///
    /// Groups are deliberately absent from the store: a member group's hints
    /// ARE `Foreign` to the fleet classification, correctly — they are judged
    /// by the legacy shared-row rule instead — so including one would make this
    /// claim false without saying anything about §7.
    #[test]
    fn every_self_hint_of_a_linked_fleet_classifies_as_own_or_person_shared() {
        let person = generate_identity();
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own = vec![0xA1; crate::DEVICE_ID_LEN];
        let sibling = vec![0xB2; crate::DEVICE_ID_LEN];
        store
            .set_own_device_fleet(crate::OwnDeviceFleet {
                own_device_id: Some(own.clone()),
                device_ids: vec![own.clone(), sibling],
                projected_from: crate::RosterVersion {
                    recovery_epoch: 0,
                    seq: 1,
                },
            })
            .unwrap();

        let fleet_hints = store.own_fleet_hints(&person.user_id, NOW).unwrap();
        let fetched = store.relay_self_hints(person.user_id.clone(), NOW).unwrap();
        assert_eq!(fetched.len(), 2 * HINTS_PER_ID_FETCH);
        for hint in &fetched {
            assert!(
                matches!(
                    fleet_hints.classify(hint),
                    FleetHint::OwnDevice | FleetHint::PersonShared
                ),
                "a fetched self hint must be one the fleet rules recognize"
            );
        }
        // Both classes are actually present, so the assertion above is not
        // passing on an empty or one-sided set.
        assert!(fetched
            .iter()
            .any(|hint| fleet_hints.classify(hint) == FleetHint::OwnDevice));
        assert!(fetched
            .iter()
            .any(|hint| fleet_hints.classify(hint) == FleetHint::PersonShared));
    }

    /// The compatibility pin for the fetch half: an install that has never
    /// linked subscribes to precisely what it did before §7 existed.
    #[test]
    fn an_unlinked_install_fetches_exactly_todays_hints() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let person = generate_identity();
        assert_eq!(
            store.relay_self_hints(person.user_id.clone(), NOW).unwrap(),
            hints_over_window(&person.user_id, NOW, CARRY_HINT_DAY_WINDOW_DAYS),
        );
        assert_eq!(
            store
                .relay_fetch_push_hints(person.user_id.clone(), NOW)
                .unwrap(),
            hints_over_range(
                &person.user_id,
                NOW,
                CARRY_HINT_DAY_WINDOW_DAYS,
                PUSH_HINT_FORWARD_DAYS
            ),
        );
        assert_eq!(
            store.relay_hint_source_ids(&person.user_id).unwrap(),
            vec![person.user_id.clone()]
        );
        // And the FRONTIER half of that compatibility claim, stated as the
        // digest rather than as the id list: an empty fleet digests to exactly
        // what a pre-§7 build digested, so shipping this work package does not
        // invalidate a single remembered relay frontier in the field. Every
        // phone would otherwise re-walk its whole mailbox on first launch.
        assert_eq!(
            crate::relay_hint_source_digest(store.relay_hint_source_ids(&person.user_id).unwrap()),
            crate::relay_hint_source_digest(vec![person.user_id.clone()]),
        );
    }

    /// The tripwire for the person-level hint resolution documented on
    /// [`MessageStore::hint_matches_known_target`]: a contact's §7 device
    /// namespace resolves to nothing today. Harmless in WP2 (no such row
    /// exists in the field), pinned so WP3/WP4 changes it deliberately.
    #[test]
    fn a_contacts_device_namespaced_hint_is_still_invisible_here() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let friend = generate_identity();
        store
            .upsert_contact(contact_for(&friend, "Friend"))
            .unwrap();

        let device_hint = compute_recipient_hint(
            crate::core_device_namespace_id(
                friend.user_id.clone(),
                vec![0xC3; crate::DEVICE_ID_LEN],
            ),
            NOW,
        );
        assert!(!store
            .hint_matches_known_target(device_hint.clone(), NOW)
            .unwrap());
        assert!(store
            .contact_matching_hint(device_hint.clone(), NOW)
            .unwrap()
            .is_none());
        // The person's own hint still resolves, so this is about the namespace
        // and not about the contact being unknown.
        assert!(store
            .hint_matches_known_target(compute_recipient_hint(friend.user_id.clone(), NOW), NOW)
            .unwrap());
        // And a device-namespaced hint never widens the group-open search.
        assert!(store
            .group_open_candidates(device_hint, friend.user_id, NOW)
            .unwrap()
            .is_empty());
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
        // self (9) + member group (9) + one contact (9), all distinct inputs.
        assert_eq!(hints.len(), 3 * HINTS_PER_ID_FETCH);
        assert_eq!(hints.len(), 27);
        let self_today = compute_recipient_hint(me.user_id.clone(), NOW);
        let group_today = compute_recipient_hint(b"group-id-0123456".to_vec(), NOW);
        let friend_today = compute_recipient_hint(friend.user_id.clone(), NOW);
        assert!(hints.contains(&self_today));
        assert!(hints.contains(&group_today));
        assert!(hints.contains(&friend_today));
        // A group we are NOT a member of contributes nothing to self hints.
        let outsider = store.relay_self_hints(friend.user_id.clone(), NOW).unwrap();
        assert_eq!(outsider.len(), 2 * HINTS_PER_ID_FETCH); // friend + their group
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
        assert_eq!(proxy.len(), HINTS_PER_ID_FETCH); // friend only
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
