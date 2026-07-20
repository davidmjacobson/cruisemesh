# T17 — Connection-request inbox (and relay presence Phase 2)

Status: spike/spec. **Any relay-server change here ships as one reviewed deploy
with David's explicit go-ahead** — nothing in this doc is a green light to touch
relayd. This spec deliberately groups every pending relay-server change so they
are reasoned about together.

## The reshaped goal

The original T17 idea was "open relay messaging" — publish agree keys so
strangers can message you. That breaks the trust model and invites spam, so it
is **dropped** (see backlog "Dropped"). This replaces it with a
**connection-request inbox** that reaches the same onboarding win without opening
message flow to strangers.

**Onboarding win:** one person shares a single QR/link with a big group (a table
on a cruise, a tour) and accepts requests as they arrive — no N×N mutual
scanning — while the trust model stays intact.

## Model

A stranger on the same relay can send a **connection request**, not a message:

- Payload: the sender's FriendCard + a short, size-capped note ("Hi, met you at
  dinner — Dana").
- It lands in a **requests inbox**. **Nothing else flows** until the recipient
  accepts.
- Accepting runs the **normal friending path** (import the card, author the
  reciprocal friend request). Declining/ignoring drops it.

So a connection request is strictly weaker than a message: it can only ever put a
card + note in front of a human, who must act before any channel opens.

## Addressing without the family token

Today relay mailboxes are gated by a shared family token. A stranger doesn't
have it, so connection requests need a different address:

- **Invite-token variant (preferred).** The shared QR/link carries a
  single-purpose **invite token** distinct from the family token. It authorizes
  *only* posting a connection request to the inviter's request inbox — never
  reading mail, never posting messages. The inviter can rotate/revoke it
  without touching the family token. This keeps the family token secret (it is
  never in a broadly-shared QR).
- The invite token is rate-limited and quota'd per token (below), so a leaked
  invite link is a bounded nuisance, not an account compromise.

## Abuse limits (reuse T4-09 admission machinery)

Connection requests are the spammiest surface we'd add, so reuse the existing
relay admission/quota controls from T4-09 rather than inventing new ones:

- Per-invite-token **rate limit** (requests/min) and **daily quota**.
- Per-sender-identity rate limit (a sender is identified by their card's signing
  key) to stop one actor flooding many invite tokens.
- **Size cap** on the note (small, e.g. ≤ 140 chars) and the whole request;
  validate with the T4-10 bounded-decoder rules.
- Inbox **depth cap** per recipient with oldest-eviction, so a flood can't grow
  storage unbounded.
- Requests **expire** (e.g. 7 days) like carried envelopes.

## Inbox UX

- A "Requests" surface (badge on the friend/add screen). Each row: avatar, name,
  note, and the **safety-word verification** affordance (reuse T10's "Verify
  contact" pattern) before accepting.
- Actions: **Accept** (→ normal friending path), **Ignore** (drop), **Block**
  (drop + suppress future requests from that signing key).
- No message content is ever shown pre-accept — there is none; only card + note.

## Interaction with consent-based friend introductions (cc21573)

Introductions and connection requests may be the **same surface**: an
introduction is a request that arrives with an introducer's provenance
(`ContactProvenance.source = introduced`, `introducerUserId`), while a
connection request arrives with `source = direct` (or a new `source =
invite-link`). Unify them into one Requests inbox where each row shows its
provenance ("Introduced by Sam" vs "Scanned your invite link"). Reuse the
existing friend-suggestion store and `reconcileOnImport` path so accept behaves
identically.

## Relay presence "Phase 2" (folded in from CONNECTIVITY_INDICATOR.md)

Phase 1 shipped local reachability dots (PR #22). Phase 2 — showing whether a
contact is **currently online via the relay** — was deferred because it touches
the live relay server. It is grouped here so the relay-server work deploys once:

- relayd would expose a **presence signal**: last-seen / currently-connected per
  identity, readable only by contacts who share the family token (never by
  strangers, never by invite-token holders).
- Privacy: presence is coarse (online / recently-online / unknown), opt-out per
  the profile's "Share when I'm online" toggle (already exists in ProfileView),
  and never leaks to non-contacts.
- This and the connection-request inbox are the two relay-server changes; spec
  both, deploy both together, behind David's go-ahead.

## Open questions for David

1. OK to introduce an **invite token** distinct from the family token in the
   shared QR/link?
2. Presence granularity + default (opt-in vs opt-out)?
3. One combined relayd deploy for inbox + presence, or inbox first?

## Disposition

Spec only. No relayd code until (1)-(3) are answered and David approves a single
combined deploy.
