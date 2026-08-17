//! Linking a new device to a person (`specs/multi-device-v1.md` §9).
//!
//! Two halves of one ceremony, both pure:
//!
//! * [`qr`] — the `CMLINK1:` payload the new device shows (§9.1). Ephemeral
//!   link material and nothing else: a fresh DH public key, an expiry, and the
//!   new device's OWN rendezvous hints. No identity secret, no person id, no
//!   device id, no relay credential. The payload bytes are frozen by a golden
//!   vector and pinned by a test that walks them for identity material.
//! * [`ceremony`] — both sides as driver-boundary state machines (§9.2), in
//!   the shape `core/src/session/relay_pass.rs` and `core/src/media/lan_pull.rs`
//!   established: typed actions out, typed resumes in, an explicit `now_ms`,
//!   declared budgets, one outstanding action at a time, and no operating
//!   system anywhere. There is no socket here, no camera, no timer, no thread —
//!   the shell carries bytes over LAN, BLE, or a relay rendezvous and hands
//!   back exactly what arrived.
//!
//! * [`bootstrap`] — §9.3's canonical export: a versioned statement of what
//!   this person knows (identity material incl. the inbox key, contacts and
//!   their rosters, group state, a recent history head), explicitly not a raw
//!   sqlite clone. It rides the confirmed channel in sealed chunks.
//! * [`activation`] — §9.4's two phases and the gate they exist for. The
//!   approving device signs the roster at `seq + 1`; the new device imports the
//!   bootstrap and acknowledges that roster's exact head; until both have
//!   happened, every advertise, author, and ack path in core refuses.
//! * [`restore`] — what opening a `.cmbak` on a fresh install may mean:
//!   "Replace this device" (old semantics) or "Link as new device" (into this
//!   ceremony), with §14.2's recovery-epoch path for the case where no
//!   approving device is left to ask.
//!
//! # The shape of one whole link
//!
//! ```text
//! new device                                approving device
//! ----------------------------------------  ----------------------------------------
//! §9.1 CoreLinkNewDevice::new -> QR
//!                                           §9.2 CoreLinkApprovingDevice::scan
//!                                                ... Noise, digits, confirm ...
//! begin_link_activation(binding, now)  <- silent from here
//! core_link_device_offer(device keys)  ->
//!                                           core_link_open_device_offer
//!                                           core_link_sign_new_device_roster (seq+1)
//!                                           build_link_bootstrap (signs it)
//!                                      <-   core_link_bootstrap_chunks / seal
//! import_link_bootstrap  (§9.4a)
//!   ^ verifies binding, signer, expiry
//! core_link_activation_ack(roster head) ->
//!                                           core_link_open_activation_ack
//!                                             ^ must be the offering device
//! complete_link_activation  (§9.4b)         adopt_own_roster
//!   ^ visible from here                       ^ both fleets now agree
//!
//! ... or, at any point before the last line:
//! abandon_link_activation(now)  <- audible again, and still unlinked
//! ```
//!
//! Everything older than the head arrives later as WP4 self-sync catch-up;
//! [`bootstrap::core_link_catch_up_plan`] is that seam, marked and stubbed.
//!
//! What is NOT here, and is felt from the moment a link completes: §9 step 5's
//! gossip of the new roster to the person's CONTACTS. WP4 owns the carrier and
//! WP5 the notification. See [`activation::MessageStore::complete_link_activation`]
//! and `MD-ROSTER-GOSSIP-TO-CONTACTS` for what it costs meanwhile.
//!
//! # Dormancy notes: what later work packages must land, and what is untrue
//! # until they do
//!
//! This crate keeps its "reserved but not yet load-bearing" facts here, in one
//! place, because each of them is a claim a reader could reasonably believe the
//! code already makes.
//!
//! * **`MD-AUTHORING-DEVICE-SIGNED` (owed by WP1).** §5 gives every message an
//!   authoring device, and [`crate::DeviceSigningDomain::MessageAuthoring`] is
//!   the frozen domain for signing as one — but nothing signs in it yet. Until
//!   it does, a message body's `sender_device_id` is a **label authenticated as
//!   coming from the sender**, not a signature by the device it names, so
//!   §10.3's revocation check
//!   ([`crate::MessageStore`]'s `signer_device_revoked`) binds an
//!   unauthenticated id. It stops a revoked device speaking under its own name;
//!   it does not stop whoever holds the identity key from relabelling. The
//!   capability half of §10 (inbox key and relay token rotation) is what does
//!   not depend on this.
//!
//! * **`MD-ROOT-OFF-DEVICE` (owed alongside the same work).** §14.2's whole
//!   argument — "a stolen approving device provably cannot sign at a higher
//!   recovery epoch" — rests on the person root secret living only inside the
//!   passphrase-encrypted `.cmbak`. Today it is [`crate::Identity`]'s
//!   `sign_sk`, and it is on every device, because that is the key an install
//!   signs everything with. So the recovery premise is a **design invariant
//!   that the deployment does not yet enforce**: a thief with a phone in an
//!   unlocked state has the root too, and §14.2's dethroning is only as strong
//!   as platform key storage until authoring moves to device keys and the root
//!   can be retired from the running install. Every rule written against it
//!   ([`crate::RosterUpdateReason::RecoveryEpochRequiresRoot`], §10's recovery
//!   path, the relay rotation authority) is correct code resting on a premise
//!   with a dated expiry, and this is that date.
//!
//! * **`MD-ROSTER-GOSSIP-TO-CONTACTS` (owed by WP6).** §10.1's contact leg
//!   produces the sealed document and the exact recipient list
//!   ([`crate::RevocationCommit::roster_document`] and `contact_user_ids`) and
//!   stops there: **there is no envelope kind that carries a roster to a
//!   contact**, so the leg is data with no carrier. Until WP6 adds one, a
//!   contact learns of a revocation only if some other path tells them, and
//!   §10.4's changed-safety-state surface has nothing to fire on. That
//!   dependency is carried forward deliberately rather than left implicit,
//!   because every §10 test that "gossips to contacts" is in fact asserting
//!   about a list and a blob, not about delivery.

pub mod activation;
pub mod bootstrap;
pub mod ceremony;
pub mod qr;
pub mod restore;
