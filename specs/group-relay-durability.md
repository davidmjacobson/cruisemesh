# Group relay durability

Status: **approved 2026-07-17** (DTN_TODOS.md D6 / finding N1). Option (d)
per-member fan-out, with the §7 questions resolved in the future-proof
direction: fan-out includes a self-addressed row (multi-device readiness),
the attachment storage multiplier is accepted (kind=17 external chunks
remain the eventual scaling path), and the legacy never-ack transition rule
is unconditional.

## 1. Problem

The relay keeps one mailbox per `family_token`, one row per `(family_token,
msg_id)`, deleted on ack. A group message is uploaded as a **single row**
whose `recipient_hint` is derived from the group id, and **every member
fetches by that same hint** — so the row is a shared resource, but the ack
protocol treats it like a 1:1 letter:

- The first member to fetch-and-open a group envelope from the relay gets
  `CONSUMED`, and `core_should_ack_inbound` acks it (`core/src/engine.rs`).
  The row is deleted for every other member.
- Members with **internet-only connectivity** (no BLE path to the family —
  someone who stayed home, or is off-ship) then have no way to ever receive
  that message. The consuming member does queue a BLE mule copy
  (`carryRelayEnvelope`), but a mule copy can't cross the internet.

D1 (consumed-SEEN acks) deliberately did **not** widen this: a group row
that comes back `SEEN` is never acked. This spec addresses the remaining
`CONSUMED`-path hole, which exists on master and was carried into the
engine unchanged.

Secondary symptom, same root cause: a member who received the group message
over BLE first re-downloads the un-ackable relay row on every poll pass
until it expires (the group-shaped tail of audit finding F3, exempted from
D1's fix for safety).

## 2. Goals / non-goals

Goals:

1. Every group member — including internet-only members — can fetch a group
   message from the relay until its expiry, regardless of who fetched first.
2. Relay rows are still deleted once they have done their job (no
   poll-churn regression, no storage pile-up beyond what expiry allows).
3. No new privacy surface: the relay must not learn group membership,
   member identities, or per-device activity beyond what hints already leak.
4. Old clients remain safe during a short mixed-fleet window (the fleet
   updates together, per project norms).

Non-goals:

- Late-join history sync (deliberately deferred, DESIGN.md §13).
- Changing BLE/LAN gossip, group crypto, or key rotation.
- Relay federation or per-device accounts.

## 3. Options considered

### (a) Never ack group-addressed rows (client-only)

Treat a consumed group fetch like `CARRIED`: leave the row; expiry (7-day
client default, 30-day server clamp) is the only deletion.

- ✅ Trivial; no server or protocol change; fixes goal 1 outright.
- ❌ Recreates F3's churn *permanently* for all group mail: every member
  re-fetches every live group row on every 60-second poll pass (and every
  WS-triggered pass) for up to a week. That is exactly the metered-Wi-Fi
  waste D1 existed to stop. A persistent fetch cursor could mask it, but
  that is a separate mechanism with its own state/restore edge cases.
- ❌ Rows occupy the D7 family quota for their full lifetime even when
  every member has the message.

Kept only as the **legacy-row rule** during migration (§5.5), not as the
design.

### (b) Server-side per-member ack tracking

Relay deletes a row after "all members" ack. The relay has no concept of
members — the family shares one token — so this needs either client-supplied
member counts (gameable, goes stale on membership change) or per-device
identifiers so acks are idempotent per member.

- ❌ New privacy surface: the relay learns device count and per-device
  activity patterns (violates goal 3 and DESIGN.md §9's "deliberately dumb
  mailbox").
- ❌ New server state machine + migration; breaks on membership change;
  the "K acks" invariant is unverifiable by the server.

Rejected.

### (c) Member-count-aware acks (client tells relay "delete after K")

A lighter (b): the uploader stamps the row with K = member count. Same
fundamental flaws — K goes stale, double-acks from one device (successive
poll passes) are indistinguishable from two members without device
identity — so it degrades into (b) or into wrong deletions.

Rejected.

### (d) Per-member fan-out at upload time — **recommended**

Stop making group rows shared. The uploader (author, or any member muling
the envelope — both know the member list from the group config) posts **one
relay row per member, including itself** (the self-addressed row is the
multi-device future-proofing resolved in §7.2 — today the uploader consumes
and acks its own row on the next poll, a trivial cost), each addressed by
that member's own daily `recipient_hint` — exactly the shape 1:1 mail
already has. Each
member then fetches it with the self-hints they already poll with, opens it
(group-sealed body, unchanged), gets `CONSUMED`, and acks **their own row**.
Delete-on-ack becomes correct again because each row has exactly one
intended reader.

- ✅ Goal 1: every member has their own durable copy until they fetch it or
  it expires. Internet-only members are served by the normal path.
- ✅ Goal 2: rows are acked away as members fetch them; the BLE-consumed
  member's row is also acked (its fan-out `msg_id` was never seen over BLE,
  so it gates as `Dispatch`, opens, dedupes in the message store, and acks
  as a normal duplicate-consume) — this *also* eliminates the group SEEN
  refetch churn without touching D1's safety rule.
- ✅ Goal 3: the relay sees N rows with N rotating hints — indistinguishable
  from N ordinary 1:1 messages. It already sees member hints (members poll
  with them); nothing new is learnable beyond upload fan-out timing.
- ✅ Proxy-polling and BLE muling keep working: a proxying phone fetches a
  contact-member's hints, gets `CARRIED` (never acked), and hands the copy
  over BLE by hint match, same as 1:1 proxy mail today.
- ➖ Storage/upload multiply by (members − 1). Bounded and acceptable: text
  is ~KB; the worst case, a 180 KiB attachment in a 6-member group, is
  ~0.9 MB against the 256 MiB D7 quota, and rows delete on fetch.

## 4. Detailed design (option d)

### 4.1 Fan-out row identity

Relay dedupe is keyed `(family_token, msg_id)`, and re-posting the same
`msg_id` never rewrites the hint — so per-member rows need per-member ids,
derived deterministically so re-uploads (author retry, multiple member
mules) dedupe server-side with **no server change**:

```
fanout_msg_id(member) = BLAKE2b-16( "cruisemesh group fanout v1"
                                    || original_msg_id
                                    || member_user_id )
```

Implemented in core (single implementation, exposed over UniFFI). The row's
`recipient_hint` = `compute_recipient_hint(member_user_id, envelope
timestamp)` — the same daily rotation 1:1 mail uses, inside the fetch
windows members already poll.

`hop_ttl`, `expiry`, and the sealed bytes are copied from the original
envelope unchanged. The sealed body still opens only with the group key and
still verifies the author's signature; fan-out changes addressing, not
crypto.

### 4.2 Upload path

`upload…Outbound/FamilyCarried` (both platforms; logic in core where
possible): when an envelope's `recipient_hint` matches a group this device
has imported, post one row per member (including self, per §3d) using §4.1
identities, instead of one group-hint row. Mark the source envelope
relay-posted only after **all** member rows post successfully — a partial
failure retries the whole set, and the derived ids make retries dedupe. A non-member mule can't decompose (it can't
recognize the hint as a group) and keeps uploading the single group-hint
row — that legacy-shaped row is covered by §5.5.

Idempotency: derived ids make re-posting a no-op (`ON CONFLICT` keeps the
row, takes the max expiry), so author + several member mules uploading the
same message converge on the same N rows.

### 4.3 Fetch/consume path

No fetch-side changes: members already poll (and WS-subscribe) with their
own self-hints. A fetched fan-out row opens as a group message via the
existing group-open path and dispositions `CONSUMED` → acked, per the
existing engine rule. The D1 consumed-SEEN rule is untouched.

**One required rule change:** a group message that arrives from the relay
addressed to *our own* hint (a fan-out copy) must **not** be re-injected
into gossip or the carry queue (today's group branch floods + force-carries
every relay-sourced group envelope). The relay fan-out already addresses
every member durably; re-flooding under the fan-out `msg_id` would give the
same content a second flood identity and duplicate carried copies. The
mesh flood of the *original* `msg_id` still happens from the author's BLE
side, unchanged. (Duplicate arrival across the two identities is already
harmless: the message store dedupes by `(chat, sender, lamport)` and
renders once — mesh_sim must pin this.)

### 4.4 Receipts, ticks, seen-set

Unchanged. Group receipts remain local-only (D9's scope). The seen-set
records fan-out ids like any other envelope id under D4's
check-then-record. `seedSeenIdsFromOwnHistory` needs no change: fan-out
ids never identify locally-authored outbound rows.

### 4.5 Membership changes

Fan-out targets the member list *at upload time* — identical semantics to
today's "membership change ⇒ key rotation" model (DESIGN.md §6.5):

- Member added later: does not receive pre-join mail (no history sync —
  unchanged, deferred).
- Member removed: their already-posted rows linger until fetch or expiry;
  they could read pre-rotation mail they were addressed — exactly what the
  key-rotation model already permits. Post-rotation mail is neither
  addressed to them nor decryptable by them. No new exposure.

## 5. Migration & compatibility

1. Fleet updates together (pre-release), so the mixed window is short.
2. New clients **stop acking group-hint rows entirely** (fold option (a)
   in for legacy rows: a `CONSUMED` group-hint fetch is treated as
   `CARRIED` for ack purposes). Legacy rows become durable-until-expiry;
   the refetch churn this admits is bounded to mail authored by
   not-yet-updated clients during the transition.
3. Old clients still receive fan-out mail with zero changes: they already
   poll their own self-hints, and their unconditional `CONSUMED` ack is
   now *correct* because the row is theirs alone.
4. Old clients still ack legacy group-hint rows on first fetch (the
   original hole) until updated — accepted for the transition window.
5. relayd: **no changes required.** (Optional later: a `fanout=1` metrics
   flag; explicitly out of scope.)
6. No wire-format change on the mesh; no new frame types.

## 6. Validation plan

- Rust: unit tests for `fanout_msg_id` determinism/derivation; upload
  fan-out row-set construction (members-minus-self, dedupe on re-post);
  legacy group-hint fetch no longer ackable; fan-out fetch ackable.
- mesh_sim: (1) author → relay → internet-only member B and BLE-first
  member C: B receives even when C fetched first; C's row acks after C's
  BLE consume without SEEN churn; (2) render-once when the same group
  message arrives via BLE (original id) and relay (fan-out id); (3) no
  gossip re-injection of fan-out copies.
- relayd e2e: N fan-out posts dedupe idempotently; quota accounting counts
  N rows; per-row ack leaves siblings intact.
- Device check: 3-member group, one phone in airplane-mode-except-Wi-Fi,
  confirm delivery + tick behavior; metered-data sanity (no group refetch
  loop).

## 7. Review resolutions (approved 2026-07-17)

1. **Storage multiplier: accepted.** The members× multiplier is fine at
   family scale under D7's quota; `kind=17` external content-addressed
   chunks remain the eventual scaling path for large media and are out of
   scope here.
2. **Self-addressed row: yes.** Fan-out covers every member *including*
   the uploader, so a future second device of the same identity can fetch
   its copy. Today the uploader consumes and acks its own row on the next
   poll pass — one small row per message, deleted within a minute.
3. **Transition rule: unconditional.** No config escape hatch; legacy
   group-hint rows are simply never acked and age out within their 7-day
   expiry.
