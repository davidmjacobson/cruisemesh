# Group management v1

Status: **Proposed — implementation requires orchestrator approval**

## Scope

This increment adds group rename, adding members after creation, and a useful
member-details surface on Android and iOS. It deliberately does not add member
removal, admin roles, ownership transfer, or key rotation.

## Current constraints

- A `Group` contains a stable id, display name, symmetric key, and canonical
  member list.
- A kind-4 invite already transports the complete group record pairwise to one
  accepted contact.
- Existing members share the current key. Removing a member without rotating
  that key would not revoke access, so removal is out of scope.
- Reusing a kind-4 invite for a rename is ambiguous: receivers cannot tell a
  stale replay from an intentional metadata update.

## Wire changes

Reserve kind **18** (`group_metadata_update`) for an encrypted group-stream
message containing:

| Field | Type | Meaning |
|---|---|---|
| `group_id` | 16 bytes | Stable group id |
| `name` | UTF-8 string | New canonical display name |
| `revision` | u64 | Monotonic metadata revision |
| `changed_by` | 16 bytes | Member user id that authored the update |

Every imported group stores `metadata_revision`, initially zero. A receiver
accepts an update only when the sender is in the current member list, the
`group_id` matches the enclosing group stream, and the revision is greater
than the stored revision. Equal or older revisions are idempotent no-ops.
Concurrent same-revision updates resolve by lexicographically comparing
`changed_by`; the winning tuple `(revision, changed_by)` is persisted so all
members converge.

Renaming authors kind 18 through the existing group encryption, store, BLE
flood, carry, and D6 relay fan-out paths. It does not rotate the group key.

## Adding members

The initiating member selects accepted contacts not already in the group. The
core canonicalizes `existing + additions`, updates the local group membership,
and queues a normal pairwise kind-4 invite containing the existing group key
and new member list to each added contact.

To make existing members converge, the initiator also sends a kind-18 metadata
update at the next revision with an optional `member_user_ids` field. Receivers
replace their member list only when the update wins the revision rule. A member
must never be removed by this operation: the core rejects an update whose list
is not a strict superset of the current canonical list.

## UI

- Group details uses avatar rows, reachability badges, and a `You` label.
- Rename is an inline action with a confirmation step and literal failure copy.
- Add members reuses the New Group multi-picker, excluding current members.
- Both actions remain available offline; queued updates show in local state
  immediately and travel through normal sync.

## Tests and rollout

- Core: revision ordering, tie-break convergence, non-member rejection,
  strict-superset validation, idempotent replay, encode/decode vectors.
- Store/engine: rename and membership update survive relay fan-out and replay.
- Android/iOS: details rows, picker exclusions, queued-offline behavior, and
  cross-platform receive tests.
- Field test: three members rename offline, reconnect, add a fourth member,
  and verify all four converge without exposing a removal control.

Implementation begins only after the orchestrator approves kind 18, the
revision tie-break, and the strict-superset membership rule.
