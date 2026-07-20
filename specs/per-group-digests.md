# D9 — Per-group digests and wire group receipts (mini-spec)

Status: spec (no code yet). D9 is the largest DTN item; per the plan it should
be the only thing in flight while core lands. Sequenced **after D6/D8**: it
builds on the receipt `via_transport` column (T6/V2) and the periodic re-digest
tick (D8).

## Problem

Group sync today is a blunt instrument. On a digest from a peer,
`resendGroupOutboundToPeer` re-offers **every** outbound group envelope we
authored for groups that peer is in, keyed at lamport 0 (a full resend; inserts
are idempotent so it's safe but wasteful). And there are **no group receipts on
the wire** — a group message shows ✓ (sent) locally and never advances to
✓✓/read, because we never learn which members received it. At family scale the
full resend is tolerable; past a handful of members and messages it is not, and
the missing receipts mean group Message-info can't show delivery at all.

## Goals

1. **Per-group digests** keyed by the group `chat_id`, replacing the lamport-0
   full resend with "send only what this peer is missing for this group."
2. **Wire group receipts**: per-member delivered/read watermarks, so a sender
   can show per-member state and an aggregate ("✓✓ = all current members have
   it").
3. **Backward/forward compatible** with the T4-10 hardened decoders: old clients
   must ignore the new frames, and the new frames must pass the T4-10 validation
   rules (bounded counts/sizes, no unbounded allocation).

## Non-goals

- Changing the 1:1 digest/receipt path (unchanged).
- Group key rotation / membership consensus (that's group-management-v1).

## Wire design

### Group digest frame

Reuse the existing DIGEST frame type but scope it to a group by setting the
digest `chat_id` to the **group id** instead of a user id. The current 1:1 rule
"digest `chat_id` must equal the userId learned from this link's HELLO"
(`digest_is_expected_chat_id`) is the discriminator: a digest whose `chat_id` is
a *group id* (never equal to a peer's user id) is a group digest.

- Entries: per **member** `sender_user_id → through_lamport` for that member's
  authored stream **within this group** (mirror of `chat_digest`, but the
  `messages` rows are keyed by `chat_id = group_id`). Bound the entry count to
  the group's member count; apply the T4-10 max-entries cap.
- An old client that receives a group-scoped digest fails
  `digest_is_expected_chat_id` (chat_id ≠ its HELLO user id) and drops it —
  exactly today's behaviour, so **old clients are safe**.

### Group receipt frame

Group receipts must name **which group** and **which member is acking whom**.
Options, in preference order:

1. **Extend the sealed receipt body** with an optional `group_id` field
   (receipts are already sealed pairwise envelopes today). A receipt with
   `group_id` present means "I (envelope sender) have delivered/read messages
   authored by `receipt.sender_user_id` **in group `group_id`** through
   `receipt.lamport`." Old clients that don't understand the field ignore the
   receipt (they only track 1:1 receipts) — acceptable degradation.
2. If the receipt body can't be extended compatibly, add a new receipt kind
   scoped to groups; old clients drop the unknown kind.

Decoders must enforce (T4-10): `group_id` length == the fixed id width;
`receipt_type ∈ {delivered, read}`; `through_lamport` within the sqlite u64
bound (reuse `validate_receipt_watermark`).

## Store design

New table (or extend `receipts`) for **per-member group watermarks**:

```
CREATE TABLE group_receipts (
    group_id       BLOB NOT NULL,   -- the group chat_id
    author_user_id BLOB NOT NULL,   -- whose messages are being acked
    member_user_id BLOB NOT NULL,   -- which member is acking
    receipt_type   INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    via_transport  INTEGER,         -- T6 parity
    PRIMARY KEY(group_id, author_user_id, member_user_id, receipt_type)
);
```

- `record_group_receipt(group_id, author, member, type, through, via)`:
  monotonic upsert, identical semantics to `record_receipt` (never regress;
  `via_transport` only on watermark advance — reuse the exact CASE expression).
- Aggregation accessor `group_receipt_state(group_id, author, type,
  member_ids) -> GroupReceiptState { min_through, per_member }`: the aggregate
  tick is **✓✓ iff every *current* member's `through_lamport ≥ message.lamport`**.
  Members who left are excluded; members who joined after a message was sent are
  excluded from that message's aggregate (a joiner can't have received history
  it predates). The shell passes the current member list from `group_members`.
- Outgoing side mirrors 1:1: `record_outgoing_group_receipt` +
  envelope-queue parity so group receipts are relay-capable and retried.

## Shell wiring

1. **Send/receive group digests**: on the D8 maintenance tick and on HELLO,
   for each group the peer is a member of, send a per-group digest instead of
   (or in addition to) the blunt `resendGroupOutboundToPeer`; on receiving a
   group digest, resend only the missing per-member envelopes (same
   `messagesAfter`/`outboundEnvelopesAfter` join, keyed by group id).
2. **Emit group receipts**: when a group message is delivered/read locally,
   author a group receipt naming the group + original author; queue for
   relay like 1:1 receipts.
3. **Group Message info**: show per-member delivered/read plus the aggregate.
   Reuse T6's `via_transport` route text per member.

## Order of work

1. This spec → review.
2. Core: `group_receipts` table + record/aggregate accessors + tests; group
   digest entry builder; decoder changes with T4-10 validation tests.
   Regenerate bindings.
3. Shells: send/receive group digests; emit/ingest group receipts; group
   Message-info UI.
4. Cross-platform **group field test** (the only device-gated step): 3-phone
   group, confirm per-member ✓✓ converges and the per-group digest doesn't
   re-send already-held envelopes.

## Compatibility checklist (must hold before merge)

- [ ] Old client receiving a group-scoped digest drops it via
      `digest_is_expected_chat_id` (proved by a decoder test).
- [ ] Old client receiving a group receipt ignores it (no crash, no 1:1
      corruption).
- [ ] New group frames pass every T4-10 validation rule (bounded counts,
      fixed-width ids, watermark bounds).
- [ ] 1:1 digest/receipt behaviour is byte-for-byte unchanged.
