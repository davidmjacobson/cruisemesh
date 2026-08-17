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

pub mod activation;
pub mod bootstrap;
pub mod ceremony;
pub mod qr;
pub mod restore;
