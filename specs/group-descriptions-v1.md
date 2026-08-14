# Group descriptions v1

Status: **Proposed — design only**

## Goal

Give every group an optional, encrypted description that members can read and
edit. A description is shared group metadata, not a local note and not part of
the group name. It must converge after offline concurrent edits, survive backup
and restore, reach members over Bluetooth/carry/relay, and remain compatible
with clients that predate descriptions.

Examples include a meeting point, family-trip dates, or a short statement of
the group's purpose.

## Product behavior

- A description is optional. Empty means that no description is set.
- Any current member may edit or clear it. This matches the existing v1 group
  rename policy; there are no group admins or owners.
- The value is trimmed at its leading and trailing whitespace. Interior spaces
  and newlines are preserved.
- The core accepts at most 280 Unicode scalar values and 1,120 UTF-8 bytes.
  Both limits are enforced before authoring and after decoding.
- New Group offers an optional Description field. Group Details shows the
  current description and an Add/Edit description action. The conversation
  title remains the short group name.
- Saving updates local state immediately. Delivery uses the normal offline
  queue, with no promise that every member sees the edit immediately.
- Description updates are hidden control messages: they do not create chat
  bubbles, unread counts, previews, or notifications.

## Why this is a new message kind

The existing kind-4 group invitation has no version byte and its decoder
rejects trailing data. Appending a description would therefore prevent older
clients from importing new invitations.

Kind 19 group metadata is versioned, but replacing its v1 payload with a v2
payload would make older clients lose renames and member additions authored by
new clients. Name, membership, and description also need independent conflict
clocks: a concurrent description edit must not overwrite a rename merely
because both happened at revision N.

Reserve kind **20** (`group_description_update`) for descriptions. Older
clients ignore this unknown hidden kind while continuing to process kind-4
invitations and kind-19 name/membership updates unchanged.

## Persisted model

Extend `Group` and the `groups` table with:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `description` | UTF-8 string | `""` | Current normalized value |
| `description_revision` | `u64` | `0` | Description register clock |
| `description_changed_by` | 16-byte UserID | empty bytes | Tie-break author |

The existing `metadata_revision` and `metadata_changed_by` continue to govern
the kind-19 name register only. Membership remains the existing add-only union.
Database migration defaults describe legacy groups as never having received a
description update; revision zero must not author a clearing update on its own.

Backups add these fields with backward-compatible defaults. Restoring an old
backup produces an empty description at revision zero.

## Wire record

The core owns this record and all validation:

```text
GroupDescriptionUpdate {
    group_id: 16 bytes,
    description: UTF-8 string,
    revision: u64,
    changed_by: 16-byte UserID,
}
```

Kind-20 content uses this big-endian layout:

```text
version(1) = 1
group_id(16)
revision(u64)
changed_by(16)
description_len(u16)
description_utf8(description_len)
```

Trailing bytes, malformed UTF-8, an invalid group/UserID length, revision zero,
and values above either description limit are rejected before state changes.

## Authoring and convergence

`create_group_description_update(group, changed_by, description)`:

1. validates the group and requires `changed_by` to be a current member;
2. normalizes and validates the proposed description;
3. increments `group.description_revision`, rejecting overflow; and
4. returns an update with `changed_by` equal to the author.

`apply_group_description_update(group, update, sender_user_id)`:

1. validates the record and exact `group_id` match;
2. requires the verified envelope signer to equal `changed_by`;
3. requires both signer and receiving device to be current group members; and
4. applies the value only when `(revision, changed_by)` is lexicographically
   greater than the group's current
   `(description_revision, description_changed_by)`.

Equal and older updates are idempotent no-ops. Two members editing offline at
the same revision converge on the update whose `changed_by` bytes sort later.
This description register is independent of the name register, so a concurrent
rename and description edit both survive.

An edit is authored as a normal group-sealed kind-20 envelope and uses the
existing durable group store, Bluetooth flood, carry, digest replay, and
per-member relay fan-out paths. The shells execute typed core author/apply
results; neither shell implements its own conflict rule.

## Invites and late joiners

The kind-4 invitation remains byte-for-byte v1 for old-client compatibility.
Whenever a group is created or members are added, the inviter queues, for each
invitee and in this order:

1. the existing pairwise-sealed kind-4 invitation; then
2. a pairwise-sealed kind-20 description snapshot when
   `description_revision > 0` (including an intentional clear).

The snapshot carries the current description clock and value; it does not
increment the revision. A dedicated core authoring operation must preserve the
invitation-before-snapshot pairwise lamport order and set the body `chat_id` to
the group id.

Transport can still reorder envelopes. If a valid pairwise snapshot arrives
before its invitation, the receiver stores it in a bounded pending-group-state
table keyed by `(group_id, changed_by)`, with the envelope's normal expiry.
Importing the invitation immediately retries matching pending state. Pending
state is encrypted-at-rest exactly like the primary store and is capped using
the same unknown/onboarding-input defenses as pending invitations.

## Refresh and mixed-version recovery

Old clients ignore kind 20 and may discard a description they received before
upgrading. Normal group replay will usually redeliver the author's retained
envelope, but expiry means that cannot be the only recovery mechanism.

A client with `description_revision > 0` reauthors the current value with a new
revision when all of these are true:

- Group Details is opened;
- no kind-20 snapshot has been authored locally for that group in 21 days; and
- the device is still a member.

This applies to an intentional empty value as well as non-empty text. The
21-day timestamp is local rate-limit state, not a convergence clock. Concurrent
refreshes are harmless under the normal tuple rule. A legacy-upgraded client at
revision zero never invents an empty refresh that could erase a known value.

Thus old clients keep all existing group behavior, new clients converge among
themselves, and a member that upgrades later recovers the description when it
next encounters a knowledgeable member.

## Security and privacy

- Group-sealed updates have the same confidentiality and signer verification as
  group messages. Pairwise invite snapshots use the existing pairwise seal.
- Possession of the group key alone is insufficient: apply still requires the
  verified signer to be in the stored member list.
- Descriptions never enter relay hints, push payloads, diagnostic logs, or
  notification text.
- The description is untrusted display text. It receives no Markdown or HTML
  interpretation. Links, if enabled, use the existing safe message-link policy.
- Local private nicknames are never serialized into the description record.

## Platform requirements

Android and iOS ship together with equivalent:

- optional creation field and validation;
- Group Details display/edit/clear controls;
- offline queued state and failure copy;
- accessibility labels and dynamic-type/font-scale behavior;
- hidden-message filtering; and
- backup/restore behavior.

Exact native controls may differ, but copy and validation outcomes must match.

## Tests and rollout gates

Core tests must cover:

- encode/decode vectors, size limits, malformed input, and trailing bytes;
- member/signature/group-id authorization;
- edit, clear, stale replay, idempotence, overflow, and concurrent convergence;
- independence from name revision and membership union;
- SQLite migration, backup defaults, and restore round trips;
- group-sealed authoring/fan-out/carry/relay replay;
- ordered invitation plus pairwise snapshot authoring; and
- pending-before-invite application, expiry, and bounded storage.

Both binding smoke suites add the new record shape. Android and iOS tests cover
creation, editing, clearing, immediate local display, hidden unread/preview
behavior, offline queueing, add-member snapshots, and mixed-version recovery.

The field gate uses three phones: edit the description while two are offline,
make a concurrent rename on a second phone, add the third member, then reconnect
through a carried/relay path. All three must converge on both the description
and name, with no description bubble or notification.

Implementation should land as a dedicated protocol PR after this specification
is approved. It must update `DESIGN.md` kind tables and regenerate both UniFFI
surfaces; generated iOS bindings are never hand-edited.
