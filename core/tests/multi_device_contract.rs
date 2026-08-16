//! Multi-device v1 WP0 contract vectors.
//!
//! Implemented vectors drive today's core. Future-surface vectors deliberately
//! remain data-only: they describe the accepted v1 rules without a test-only
//! roster, key, or linking implementation.

use cruisemesh_core::{
    compute_recipient_hint, core_own_capabilities, core_relay_ack_ids, core_should_ack_inbound,
    CarriedEnvelope, CoreInboundDisposition, CoreRelayEnvelopeDisposition, MessageStore,
    StoredMessage, CAP_MULTI_DEVICE,
};

/// Assert with the vector id in the failure message, following
/// `protocol_contract.rs`'s contract assertion style.
macro_rules! contract_assert {
    ($id:expr, $cond:expr, $($detail:tt)+) => {
        assert!(
            $cond,
            "{} violated: {}",
            $id,
            format_args!($($detail)+),
        )
    };
}

/// §5: envelopes with no sealed-body device field map to this reserved
/// all-zero stream. The sentinel is pinned here so a future WP1 constant that
/// picks a different value has to edit this vector deliberately.
const LEGACY_DEVICE_ID: [u8; 16] = [0u8; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RosterVersion {
    recovery_epoch: u64,
    seq: u64,
}

/// DL-5 fixture for a roster document. It has fields for device keys and
/// revocation tombstones and *structurally* has nowhere to put an endpoint --
/// that is the point of the vector. WP1's real `Roster` type must keep that
/// property: endpoints must be impossible to represent, not merely absent by
/// convention, so the endpoint-privacy invariant cannot regress by an
/// innocent-looking struct field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RosterDocument {
    device_keys: &'static [&'static [u8]],
    tombstones: &'static [&'static [u8]],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    RosterUpdate {
        stored: RosterVersion,
        incoming: RosterVersion,
    },
    /// DL-1's second half: version ordering alone never accepts a roster; the
    /// signature chain must also verify back to the person root.
    RosterSignatureChain {
        stored: RosterVersion,
        incoming: RosterVersion,
        chain_verifies_to_person_root: bool,
    },
    RosterFork {
        version: RosterVersion,
        stored_content: &'static [u8],
        incoming_content: &'static [u8],
    },
    /// DL-2's follow-on: once a person is quarantined, later rosters for that
    /// person stay quarantined however well-formed they are.
    RosterUpdateAfterFork {
        quarantined_at: RosterVersion,
        incoming: RosterVersion,
        incoming_is_strictly_higher: bool,
        auto_resolves: bool,
    },
    RosterTombstonedDeviceReturns {
        version: RosterVersion,
        tombstoned_device_id: &'static [u8],
        returning_device_id: &'static [u8],
    },
    RosterRelinkFreshKey {
        version: RosterVersion,
        tombstoned_device_id: &'static [u8],
        replacement_device_id: &'static [u8],
    },
    RosterPairwiseGossipNoDirectory {
        version: RosterVersion,
        sealed_pairwise: bool,
        uses_directory: bool,
    },
    RosterKeysNeverEndpoints {
        version: RosterVersion,
        document: RosterDocument,
    },
    RecoveryAuthority {
        version: RosterVersion,
        root_secret_only_in_encrypted_backup: bool,
        device_key_alone_can_mint_higher_epoch: bool,
    },
    LegacyEnvelopeWithoutDevice {
        legacy_device_id: [u8; 16],
    },
    TwoDevicesSamePersonSameLamport {
        first_device_id: &'static [u8],
        second_device_id: &'static [u8],
    },
    OwnDeviceFanoutConsumed,
    SiblingDeviceFanoutCarried,
    LegacyPersonAddressedConsumed,
    PersonSealedCarriedCopy,
    StaleRosterSealing {
        revoked_device_still_listed: bool,
        sealed_with_inbox_key_generation: u64,
        current_inbox_key_generation: u64,
        surviving_devices_still_receive: bool,
    },
    CapabilityReservation,
    AddDevice {
        resulting_device_count: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Accepted,
    Ignored,
    ForkQuarantined,
    TombstonePermanent,
    FreshKeyAccepted,
    PairwiseGossipNoDirectory,
    KeysOnlyNoEndpoints,
    RecoveryRequiresEncryptedBackup,
    LegacyDeviceStream,
    SeparateStreams,
    Quarantined,
    Acknowledged,
    NotAcknowledgedBecauseCarried,
    NotAcknowledgedByNamespaceRefusal,
    PersonDigestProofOnly,
    DigestProofOnly,
    SurvivingDevicesDeliver,
    ReservedNotAdvertised,
    Advertised,
    DeviceAdded,
    DeviceAddedWithWarning,
    DeviceAddRefused,
}

struct Vector {
    id: &'static str,
    scenario: Scenario,
    target_outcome: Outcome,
    implemented: bool,
}

const VECTORS: &[Vector] = &[
    // DL-1: (2, 0) strictly supersedes (1, 1000), even though its seq resets.
    Vector {
        id: "MD-ROSTER-GREATER",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 1,
                seq: 1000,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
        },
        target_outcome: Outcome::Accepted,
        implemented: false,
    },
    // DL-1: (1, 1001) must not roll back the already stored (2, 0).
    Vector {
        id: "MD-ROSTER-LOWER",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 1,
                seq: 1001,
            },
        },
        target_outcome: Outcome::Ignored,
        implemented: false,
    },
    // DL-1: equal (recovery_epoch, seq) is idempotent gossip, not an update.
    Vector {
        id: "MD-ROSTER-EQUAL",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
        },
        target_outcome: Outcome::Ignored,
        implemented: false,
    },
    // DL-1 discriminator: the ordinary case. Within one recovery epoch, a
    // strictly higher seq is the normal add/revoke and is accepted. Without
    // this vector an implementation that only ever compared `recovery_epoch`
    // would pass every other DL-1 vector.
    Vector {
        id: "MD-ROSTER-SAME-EPOCH-ADVANCE",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 1,
            },
        },
        target_outcome: Outcome::Accepted,
        implemented: false,
    },
    // DL-1 discriminator: within one recovery epoch a lower seq is a replayed
    // or stale roster and is ignored -- the same-epoch twin of MD-ROSTER-LOWER.
    Vector {
        id: "MD-ROSTER-SAME-EPOCH-ROLLBACK",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 5,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 3,
            },
        },
        target_outcome: Outcome::Ignored,
        implemented: false,
    },
    // DL-1 discriminator: version ordering is necessary, never sufficient. A
    // strictly higher (3, 0) whose signature chain does not verify back to the
    // person root is ignored -- otherwise anyone could mint a roster.
    Vector {
        id: "MD-ROSTER-CHAIN-BROKEN",
        scenario: Scenario::RosterSignatureChain {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 3,
                seq: 0,
            },
            chain_verifies_to_person_root: false,
        },
        target_outcome: Outcome::Ignored,
        implemented: false,
    },
    // DL-2: equal (recovery_epoch, seq) plus different content is a fork.
    Vector {
        id: "MD-ROSTER-FORK",
        scenario: Scenario::RosterFork {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            stored_content: b"approved-device-a",
            incoming_content: b"approved-device-b",
        },
        target_outcome: Outcome::ForkQuarantined,
        implemented: false,
    },
    // DL-2 follow-on: quarantine is sticky. A legitimately higher (2, 1)
    // roster arriving after the fork does NOT lift the quarantine -- "never
    // auto-resolve a fork" means a later good version is not a resolution.
    Vector {
        id: "MD-ROSTER-FORK-QUARANTINE-PERSISTS",
        scenario: Scenario::RosterUpdateAfterFork {
            quarantined_at: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 1,
            },
            incoming_is_strictly_higher: true,
            auto_resolves: false,
        },
        target_outcome: Outcome::ForkQuarantined,
        implemented: false,
    },
    // DL-4: the tombstoned device_id itself can never return.
    Vector {
        id: "MD-ROSTER-TOMBSTONE",
        scenario: Scenario::RosterTombstonedDeviceReturns {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 1,
            },
            tombstoned_device_id: b"revoked-phone-key",
            returning_device_id: b"revoked-phone-key",
        },
        target_outcome: Outcome::TombstonePermanent,
        implemented: false,
    },
    // DL-4: re-linking that hardware mints a fresh key, which may be accepted.
    Vector {
        id: "MD-ROSTER-RELINK-FRESH-KEY",
        scenario: Scenario::RosterRelinkFreshKey {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 2,
            },
            tombstoned_device_id: b"revoked-phone-key",
            replacement_device_id: b"relinked-phone-fresh-key",
        },
        target_outcome: Outcome::FreshKeyAccepted,
        implemented: false,
    },
    // DL-3: a roster is sealed pairwise gossip; no directory sees plaintext.
    Vector {
        id: "MD-ROSTER-PAIRWISE-GOSSIP",
        scenario: Scenario::RosterPairwiseGossipNoDirectory {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 3,
            },
            sealed_pairwise: true,
            uses_directory: false,
        },
        target_outcome: Outcome::PairwiseGossipNoDirectory,
        implemented: false,
    },
    // DL-5: roster documents carry device keys and tombstones, never
    // third-party endpoints. The fixture type has no endpoint field at all.
    Vector {
        id: "MD-ROSTER-KEYS-NOT-ENDPOINTS",
        scenario: Scenario::RosterKeysNeverEndpoints {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 4,
            },
            document: RosterDocument {
                device_keys: &[b"alice-phone-device-key", b"alice-tablet-device-key"],
                tombstones: &[b"revoked-phone-key"],
            },
        },
        target_outcome: Outcome::KeysOnlyNoEndpoints,
        implemented: false,
    },
    // §3 / §14.2: only the encrypted backup's root secret can raise epoch.
    Vector {
        id: "MD-RECOVERY-BACKUP-AUTHORITY",
        scenario: Scenario::RecoveryAuthority {
            version: RosterVersion {
                recovery_epoch: 3,
                seq: 0,
            },
            root_secret_only_in_encrypted_backup: true,
            device_key_alone_can_mint_higher_epoch: false,
        },
        target_outcome: Outcome::RecoveryRequiresEncryptedBackup,
        implemented: false,
    },
    // §5: absent sealed-body device fields become the all-zero legacy stream.
    Vector {
        id: "MD-STREAM-LEGACY-ID",
        scenario: Scenario::LegacyEnvelopeWithoutDevice {
            legacy_device_id: LEGACY_DEVICE_ID,
        },
        target_outcome: Outcome::LegacyDeviceStream,
        implemented: false,
    },
    // §5: sibling author streams must remain independent even at one lamport.
    Vector {
        id: "MD-STREAM-SIBLING-LAMPORT",
        scenario: Scenario::TwoDevicesSamePersonSameLamport {
            first_device_id: b"alice-phone-device-key",
            second_device_id: b"alice-tablet-device-key",
        },
        target_outcome: Outcome::SeparateStreams,
        implemented: true,
    },
    // ACK-MD-1: a consumed row in this device's fan-out namespace acks.
    Vector {
        id: "MD-ACK-OWN-FANOUT",
        scenario: Scenario::OwnDeviceFanoutConsumed,
        target_outcome: Outcome::Acknowledged,
        implemented: true,
    },
    // ACK-MD-1: today a sibling-sealed row cannot open here, so it falls to
    // Carried and happens not to ack. v1's required non-ack is instead an
    // explicit namespace refusal; do not mistake today's crypto accident for it.
    Vector {
        id: "MD-ACK-SIBLING-FANOUT",
        scenario: Scenario::SiblingDeviceFanoutCarried,
        target_outcome: Outcome::NotAcknowledgedByNamespaceRefusal,
        implemented: true,
    },
    // ACK-MD-2: a legacy person-addressed row opens here and therefore acks
    // today, but a multi-device fleet must leave it for its sibling devices.
    Vector {
        id: "MD-ACK-LEGACY-PERSON-ROW",
        scenario: Scenario::LegacyPersonAddressedConsumed,
        target_outcome: Outcome::NotAcknowledgedByNamespaceRefusal,
        implemented: true,
    },
    // ACK-MD-3: this is a person-sealed carried copy. A single device cannot
    // confirm for the person; receipt proof must be person-safe in v1.
    Vector {
        id: "MD-ACK-CARRIED-DIGEST-PROOF",
        scenario: Scenario::PersonSealedCarriedCopy,
        target_outcome: Outcome::PersonDigestProofOnly,
        implemented: true,
    },
    // §6: stale inbox-key sealing has bounded exposure, not a delivery brick.
    // A months-offline contact seals to the roster it knows -- which still
    // lists the revoked device and still uses the pre-revocation inbox key
    // generation -- and the surviving devices must still receive that mail.
    Vector {
        id: "MD-SEAL-STALE-ROSTER",
        scenario: Scenario::StaleRosterSealing {
            revoked_device_still_listed: true,
            sealed_with_inbox_key_generation: 4,
            current_inbox_key_generation: 5,
            surviving_devices_still_receive: true,
        },
        target_outcome: Outcome::SurvivingDevicesDeliver,
        implemented: false,
    },
    // §12: WP1 advertises the reserved capability through HELLO2.
    Vector {
        id: "MD-CAPABILITY-RESERVED",
        scenario: Scenario::CapabilityReservation,
        target_outcome: Outcome::Advertised,
        implemented: true,
    },
    // §14.3: soft 8 / hard 16, with the boundary pinned by the 2026-08-16
    // decision: counts are the roster size AFTER this add; up to 8 is silent,
    // the 9th warns, 16 is the last allowed, the 17th is refused.
    Vector {
        id: "MD-DEVICE-CAP-7",
        scenario: Scenario::AddDevice {
            resulting_device_count: 7,
        },
        target_outcome: Outcome::DeviceAdded,
        implemented: false,
    },
    Vector {
        id: "MD-DEVICE-CAP-8",
        scenario: Scenario::AddDevice {
            resulting_device_count: 8,
        },
        target_outcome: Outcome::DeviceAdded,
        implemented: false,
    },
    Vector {
        id: "MD-DEVICE-CAP-9",
        scenario: Scenario::AddDevice {
            resulting_device_count: 9,
        },
        target_outcome: Outcome::DeviceAddedWithWarning,
        implemented: false,
    },
    Vector {
        id: "MD-DEVICE-CAP-16",
        scenario: Scenario::AddDevice {
            resulting_device_count: 16,
        },
        target_outcome: Outcome::DeviceAddedWithWarning,
        implemented: false,
    },
    Vector {
        id: "MD-DEVICE-CAP-17",
        scenario: Scenario::AddDevice {
            resulting_device_count: 17,
        },
        target_outcome: Outcome::DeviceAddRefused,
        implemented: false,
    },
];

/// The one pinned table of every vector's target outcome. Editing any
/// vector's `target_outcome` -- including flipping a data-only rule -- fails
/// here, so a future work package cannot quietly re-decide an accepted rule
/// while its own tests stay green.
const PINNED_TARGETS: &[(&str, Outcome)] = &[
    ("MD-ROSTER-GREATER", Outcome::Accepted),
    ("MD-ROSTER-LOWER", Outcome::Ignored),
    ("MD-ROSTER-EQUAL", Outcome::Ignored),
    ("MD-ROSTER-SAME-EPOCH-ADVANCE", Outcome::Accepted),
    ("MD-ROSTER-SAME-EPOCH-ROLLBACK", Outcome::Ignored),
    ("MD-ROSTER-CHAIN-BROKEN", Outcome::Ignored),
    ("MD-ROSTER-FORK", Outcome::ForkQuarantined),
    (
        "MD-ROSTER-FORK-QUARANTINE-PERSISTS",
        Outcome::ForkQuarantined,
    ),
    ("MD-ROSTER-TOMBSTONE", Outcome::TombstonePermanent),
    ("MD-ROSTER-RELINK-FRESH-KEY", Outcome::FreshKeyAccepted),
    (
        "MD-ROSTER-PAIRWISE-GOSSIP",
        Outcome::PairwiseGossipNoDirectory,
    ),
    ("MD-ROSTER-KEYS-NOT-ENDPOINTS", Outcome::KeysOnlyNoEndpoints),
    (
        "MD-RECOVERY-BACKUP-AUTHORITY",
        Outcome::RecoveryRequiresEncryptedBackup,
    ),
    ("MD-STREAM-LEGACY-ID", Outcome::LegacyDeviceStream),
    ("MD-STREAM-SIBLING-LAMPORT", Outcome::SeparateStreams),
    ("MD-ACK-OWN-FANOUT", Outcome::Acknowledged),
    (
        "MD-ACK-SIBLING-FANOUT",
        Outcome::NotAcknowledgedByNamespaceRefusal,
    ),
    (
        "MD-ACK-LEGACY-PERSON-ROW",
        Outcome::NotAcknowledgedByNamespaceRefusal,
    ),
    (
        "MD-ACK-CARRIED-DIGEST-PROOF",
        Outcome::PersonDigestProofOnly,
    ),
    ("MD-SEAL-STALE-ROSTER", Outcome::SurvivingDevicesDeliver),
    ("MD-CAPABILITY-RESERVED", Outcome::Advertised),
    ("MD-DEVICE-CAP-7", Outcome::DeviceAdded),
    ("MD-DEVICE-CAP-8", Outcome::DeviceAdded),
    ("MD-DEVICE-CAP-9", Outcome::DeviceAddedWithWarning),
    ("MD-DEVICE-CAP-16", Outcome::DeviceAddedWithWarning),
    ("MD-DEVICE-CAP-17", Outcome::DeviceAddRefused),
];

/// The pinned map of executed vector id -> what today's core actually does.
/// This is the WP0 photograph of current behaviour: every change to a driver
/// result, in either direction, must be a deliberate edit here.
const PINNED_DRIVER_RESULTS: &[(&str, Outcome)] = &[
    ("MD-STREAM-SIBLING-LAMPORT", Outcome::Quarantined),
    ("MD-ACK-OWN-FANOUT", Outcome::Acknowledged),
    (
        "MD-ACK-SIBLING-FANOUT",
        Outcome::NotAcknowledgedBecauseCarried,
    ),
    ("MD-ACK-LEGACY-PERSON-ROW", Outcome::Acknowledged),
    ("MD-ACK-CARRIED-DIGEST-PROOF", Outcome::DigestProofOnly),
    ("MD-CAPABILITY-RESERVED", Outcome::ReservedNotAdvertised),
];

/// This person's wire identity (`person_id` in new code, `user_id` today).
const PERSON_ID: [u8; 16] = [0x5A; 16];
const OWN_DEVICE_ID: &[u8] = b"alice-phone-device-key";
const SIBLING_DEVICE_ID: &[u8] = b"alice-tablet-device-key";
const NOW_MS: i64 = 1_700_000_000_000;

/// §7's per-device hint namespace, derived from `(person_id, device_id)`.
///
/// WP2 owns the real derivation (alongside `device_fanout_msg_id`); this stand-in
/// only has to be *distinct per device and distinct from the bare person hint*,
/// which is exactly the property the ack vectors below depend on. Today's
/// planner cannot tell these hints apart -- it only special-cases imported
/// group hints -- so both a device-namespaced row and a bare person row are
/// planned for ack. When WP2 teaches the planner to ack only rows in this
/// device's own namespace, the bare-person row stops being planned and
/// `MD-ACK-LEGACY-PERSON-ROW`'s driver result flips from `Acknowledged`, which
/// trips both `PINNED_DRIVER_RESULTS` and the divergence ledger with no hand
/// edit to the fixtures.
fn device_namespace_id(person_id: &[u8], device_id: &[u8]) -> Vec<u8> {
    let mut id = person_id.to_vec();
    id.extend_from_slice(device_id);
    id
}

fn device_hint(device_id: &[u8]) -> Vec<u8> {
    compute_recipient_hint(device_namespace_id(&PERSON_ID, device_id), NOW_MS)
}

fn person_hint() -> Vec<u8> {
    compute_recipient_hint(PERSON_ID.to_vec(), NOW_MS)
}

fn stored_message(payload: &[u8]) -> StoredMessage {
    StoredMessage {
        chat_id: b"alice".to_vec(),
        sender_user_id: b"alice".to_vec(),
        lamport: 7,
        timestamp: 1_700_000_000_000,
        kind: 1,
        payload: payload.to_vec(),
    }
}

fn relay_item(
    relay_id: i64,
    disposition: CoreInboundDisposition,
    recipient_hint: Vec<u8>,
) -> CoreRelayEnvelopeDisposition {
    CoreRelayEnvelopeDisposition {
        relay_id,
        msg_id: vec![relay_id as u8; 16],
        disposition,
        recipient_hint,
    }
}

/// Run the PRODUCTION ack planner -- the same
/// `MessageStore::core_relay_ack_ids_with_consumed` (`core/src/engine.rs:985`)
/// every shell calls, where the legacy shared-hint withholding lives
/// (`core/src/engine.rs:994-1006`) -- over one relay row, on an empty store
/// owned by `PERSON_ID`.
fn plan_acks(item: CoreRelayEnvelopeDisposition) -> Vec<i64> {
    let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
    store
        .core_relay_ack_ids_with_consumed(vec![item], PERSON_ID.to_vec(), NOW_MS)
        .expect("production relay ack planner")
}

/// Execute every present-day vector through its real core surface. Future
/// vectors return `None`, preserving their data-only status until their work
/// package exists. The returned value is the driver result used by the
/// divergence ledger below -- never a copied `current_outcome` fixture.
fn drive(vector: &Vector) -> Option<Outcome> {
    if !vector.implemented {
        return None;
    }

    let outcome = match vector.scenario {
        Scenario::TwoDevicesSamePersonSameLamport {
            first_device_id,
            second_device_id,
        } => {
            contract_assert!(
                vector.id,
                first_device_id != second_device_id,
                "the scenario must name two distinct device ids"
            );
            // §5: TODAY both device ids still map to the one legacy stream key
            // `UNIQUE(chat_id, sender_user_id, lamport)`
            // (`core/src/store.rs:8872`; the insert that trips it is at
            // `core/src/store.rs:2338`). The two rows below differ only in the
            // authoring device, which is exactly what today's key cannot see.
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            contract_assert!(
                vector.id,
                store
                    .insert_message(stored_message(first_device_id))
                    .expect("first legacy-stream insert"),
                "the first device row must insert"
            );
            contract_assert!(
                vector.id,
                !store
                    .insert_message(stored_message(second_device_id))
                    .expect("second legacy-stream insert"),
                "the shared legacy stream key must reject the sibling row"
            );
            contract_assert!(
                vector.id,
                store.has_message_conflicts().expect("conflict diagnostic"),
                "the rejected sibling row must be conflict-quarantined"
            );
            Outcome::Quarantined
        }
        Scenario::OwnDeviceFanoutConsumed => {
            // ACK-MD-1: this device successfully opened a row addressed to its
            // OWN device hint namespace. This is the row that must keep acking
            // after WP2 -- it is the one namespace this device may delete from.
            let item = relay_item(
                41,
                CoreInboundDisposition::Consumed,
                device_hint(OWN_DEVICE_ID),
            );
            let planned = plan_acks(item.clone());
            contract_assert!(
                vector.id,
                planned == vec![41],
                "the production planner must include the consumed own-device row, got {planned:?}"
            );
            // Secondary: the disposition-only rule underneath the planner.
            contract_assert!(
                vector.id,
                core_should_ack_inbound(item.disposition),
                "a consumed own-device row must be ackable"
            );
            contract_assert!(
                vector.id,
                core_relay_ack_ids(vec![item]) == vec![41],
                "the disposition-only planner agrees on the own-device row"
            );
            Outcome::Acknowledged
        }
        Scenario::SiblingDeviceFanoutCarried => {
            // ACK-MD-1: mesh_receive's pairwise open fails for a sibling's key
            // and its foreign-traffic branch returns Carried. Carried is never
            // eligible for the production planner.
            //
            // KNOWN, ACCEPTED RESIDUAL: this pin is label-inert until WP2
            // introduces per-device relay rows. Today the sibling's row is
            // merely an envelope this device cannot open, so it is withheld by
            // the Carried rule and not by any namespace check -- the fixture's
            // sibling hint is invisible to today's planner. The pin's value is
            // that it must not START acking; when WP2 lands, the withholding
            // reason must become the explicit namespace refusal named by
            // `target_outcome`, and Carried alone will no longer be the story.
            let item = relay_item(
                42,
                CoreInboundDisposition::Carried,
                device_hint(SIBLING_DEVICE_ID),
            );
            let planned = plan_acks(item.clone());
            contract_assert!(
                vector.id,
                planned.is_empty(),
                "the production planner must not ack a carried sibling row, got {planned:?}"
            );
            contract_assert!(
                vector.id,
                !core_should_ack_inbound(item.disposition),
                "the sibling-key open failure must fall through to non-ackable Carried"
            );
            contract_assert!(
                vector.id,
                core_relay_ack_ids(vec![item]).is_empty(),
                "the disposition-only planner also withholds the carried sibling row"
            );
            Outcome::NotAcknowledgedBecauseCarried
        }
        Scenario::LegacyPersonAddressedConsumed => {
            // ACK-MD-2: a legacy sender uploads ONE person-addressed row, so
            // this fixture carries the bare person hint -- not a device
            // namespace hint. The legacy person key opens here, so the current
            // core sees Consumed and (incorrectly for a multi-device fleet)
            // deletes the only copy the siblings could still fetch.
            //
            // This row and MD-ACK-OWN-FANOUT's differ in the one input the
            // planner will learn to read: `recipient_hint`. When WP2 restricts
            // acks to this device's own namespace, this vector's planner output
            // becomes empty while MD-ACK-OWN-FANOUT's stays `[41]`.
            let item = relay_item(43, CoreInboundDisposition::Consumed, person_hint());
            let planned = plan_acks(item.clone());
            contract_assert!(
                vector.id,
                planned == vec![43],
                "the production planner currently deletes the legacy person row, got {planned:?}"
            );
            contract_assert!(
                vector.id,
                core_should_ack_inbound(item.disposition),
                "a successfully opened legacy person row is consumed today"
            );
            contract_assert!(
                vector.id,
                core_relay_ack_ids(vec![item]) == vec![43],
                "the disposition-only planner also acks the legacy person row today"
            );
            Outcome::Acknowledged
        }
        Scenario::PersonSealedCarriedCopy => {
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            let now_ms = NOW_MS;
            let person_id = PERSON_ID.to_vec();
            let msg_id = vec![8; 16];
            contract_assert!(
                vector.id,
                !core_should_ack_inbound(CoreInboundDisposition::Carried),
                "ACK-MD-3 carried copies must not enter the relay ack plan"
            );
            contract_assert!(
                vector.id,
                plan_acks(relay_item(
                    44,
                    CoreInboundDisposition::Carried,
                    person_hint()
                ))
                .is_empty(),
                "a carried disposition must plan no relay acknowledgement"
            );
            contract_assert!(
                vector.id,
                store
                    .enqueue_carried_envelope(
                        CarriedEnvelope {
                            msg_id: msg_id.clone(),
                            hop_ttl: 6,
                            expiry: now_ms + 60_000,
                            recipient_hint: compute_recipient_hint(person_id.clone(), now_ms),
                            sealed: vec![9; 64],
                        },
                        true,
                        now_ms,
                        1024 * 1024,
                    )
                    .expect("enqueue person-sealed carried copy"),
                "the carried fixture must be retained until receipt proof"
            );
            contract_assert!(
                vector.id,
                store
                    .core_confirm_carried_deliveries(
                        person_id.clone(),
                        vec![msg_id.clone()],
                        false,
                        now_ms,
                    )
                    .expect("unauthenticated digest")
                    == 0,
                "an unauthenticated digest must never remove the carried copy"
            );
            contract_assert!(
                vector.id,
                store
                    .carried_len()
                    .expect("carried count after unauthenticated digest")
                    == 1,
                "the unauthenticated digest must leave the carried copy present"
            );
            contract_assert!(
                vector.id,
                store
                    .core_confirm_carried_deliveries(person_id, vec![msg_id], true, now_ms)
                    .expect("authenticated digest")
                    == 1,
                "today an authenticated single-device digest retires the person-sealed copy"
            );
            Outcome::DigestProofOnly
        }
        Scenario::CapabilityReservation => {
            contract_assert!(
                vector.id,
                CAP_MULTI_DEVICE == 1 << 2,
                "CAP_MULTI_DEVICE must retain its assigned bit"
            );
            contract_assert!(
                vector.id,
                core_own_capabilities() & CAP_MULTI_DEVICE == 0,
                "CAP_MULTI_DEVICE remains reserved but unadvertised before WP1"
            );
            Outcome::ReservedNotAdvertised
        }
        Scenario::RosterUpdate { .. }
        | Scenario::RosterSignatureChain { .. }
        | Scenario::RosterFork { .. }
        | Scenario::RosterUpdateAfterFork { .. }
        | Scenario::RosterTombstonedDeviceReturns { .. }
        | Scenario::RosterRelinkFreshKey { .. }
        | Scenario::RosterPairwiseGossipNoDirectory { .. }
        | Scenario::RosterKeysNeverEndpoints { .. }
        | Scenario::RecoveryAuthority { .. }
        | Scenario::LegacyEnvelopeWithoutDevice { .. }
        | Scenario::StaleRosterSealing { .. }
        | Scenario::AddDevice { .. } => unreachable!("unimplemented vector ran"),
    };
    Some(outcome)
}

/// Every vector's target outcome is pinned exactly once, in order. Flipping
/// any `target_outcome` -- data-only vectors included -- fails here.
#[test]
fn every_vector_target_outcome_is_pinned() {
    let actual: Vec<(&str, Outcome)> = VECTORS
        .iter()
        .map(|vector| (vector.id, vector.target_outcome))
        .collect();
    let expected: Vec<(&str, Outcome)> = PINNED_TARGETS.to_vec();
    assert_eq!(
        actual, expected,
        "the accepted v1 rules are a pinned table; changing a target outcome is a spec decision"
    );
}

#[test]
fn roster_and_cap_data_encode_the_accepted_rules() {
    for vector in VECTORS {
        match vector.scenario {
            Scenario::RosterUpdate { stored, incoming } if vector.id == "MD-ROSTER-GREATER" => {
                contract_assert!(
                    vector.id,
                    (stored.recovery_epoch, stored.seq) == (1, 1000)
                        && (incoming.recovery_epoch, incoming.seq) == (2, 0),
                    "DL-1 recovery-epoch supersession fixture changed"
                );
            }
            Scenario::RosterUpdate { stored, incoming } if vector.id == "MD-ROSTER-LOWER" => {
                contract_assert!(
                    vector.id,
                    (stored.recovery_epoch, stored.seq) == (2, 0)
                        && (incoming.recovery_epoch, incoming.seq) == (1, 1001),
                    "DL-1 rollback fixture changed"
                );
            }
            Scenario::RosterUpdate { stored, incoming } if vector.id == "MD-ROSTER-EQUAL" => {
                contract_assert!(
                    vector.id,
                    stored == incoming && (stored.recovery_epoch, stored.seq) == (2, 0),
                    "DL-1 equal-version fixture changed"
                );
            }
            Scenario::RosterUpdate { stored, incoming }
                if vector.id == "MD-ROSTER-SAME-EPOCH-ADVANCE" =>
            {
                contract_assert!(
                    vector.id,
                    stored.recovery_epoch == incoming.recovery_epoch && incoming.seq > stored.seq,
                    "DL-1 same-epoch advance must keep the epoch and raise the seq"
                );
            }
            Scenario::RosterUpdate { stored, incoming }
                if vector.id == "MD-ROSTER-SAME-EPOCH-ROLLBACK" =>
            {
                contract_assert!(
                    vector.id,
                    stored.recovery_epoch == incoming.recovery_epoch
                        && incoming.seq < stored.seq
                        && (stored.seq, incoming.seq) == (5, 3),
                    "DL-1 same-epoch rollback fixture changed"
                );
            }
            Scenario::RosterSignatureChain {
                stored,
                incoming,
                chain_verifies_to_person_root,
            } => contract_assert!(
                vector.id,
                (incoming.recovery_epoch, incoming.seq) > (stored.recovery_epoch, stored.seq)
                    && !chain_verifies_to_person_root,
                "DL-1 chain vector must be strictly higher AND fail to verify"
            ),
            Scenario::RosterFork {
                version,
                stored_content,
                incoming_content,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 0)
                    && stored_content != incoming_content,
                "DL-2 requires equal version and different content"
            ),
            Scenario::RosterUpdateAfterFork {
                quarantined_at,
                incoming,
                incoming_is_strictly_higher,
                auto_resolves,
            } => contract_assert!(
                vector.id,
                (quarantined_at.recovery_epoch, quarantined_at.seq) == (2, 0)
                    && (incoming.recovery_epoch, incoming.seq) == (2, 1)
                    && incoming_is_strictly_higher
                    && !auto_resolves,
                "DL-2 quarantine must survive a legitimately higher roster"
            ),
            Scenario::RosterTombstonedDeviceReturns {
                version,
                tombstoned_device_id,
                returning_device_id,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 1)
                    && tombstoned_device_id == returning_device_id,
                "DL-4 tombstoned device_id fixture changed"
            ),
            Scenario::RosterRelinkFreshKey {
                version,
                tombstoned_device_id,
                replacement_device_id,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 2)
                    && tombstoned_device_id != replacement_device_id,
                "DL-4 re-link must mint a fresh device key"
            ),
            Scenario::RosterPairwiseGossipNoDirectory {
                version,
                sealed_pairwise,
                uses_directory,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 3)
                    && sealed_pairwise
                    && !uses_directory,
                "DL-3 roster gossip is pairwise-sealed and directory-free"
            ),
            Scenario::RosterKeysNeverEndpoints { version, document } => {
                // The fixture type itself is the assertion: `RosterDocument`
                // has device keys and tombstones and no field an endpoint
                // could live in. WP1's real `Roster` must keep endpoints
                // structurally impossible, not merely omitted.
                contract_assert!(
                    vector.id,
                    (version.recovery_epoch, version.seq) == (2, 4)
                        && !document.device_keys.is_empty()
                        && !document.tombstones.is_empty()
                        && document
                            .device_keys
                            .iter()
                            .all(|key| !document.tombstones.contains(key)),
                    "DL-5 roster fixture must list device keys and tombstones, disjointly"
                );
            }
            Scenario::RecoveryAuthority {
                version,
                root_secret_only_in_encrypted_backup,
                device_key_alone_can_mint_higher_epoch,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (3, 0)
                    && root_secret_only_in_encrypted_backup
                    && !device_key_alone_can_mint_higher_epoch,
                "§3 / §14.2 recovery authority fixture changed"
            ),
            Scenario::LegacyEnvelopeWithoutDevice { legacy_device_id } => contract_assert!(
                vector.id,
                legacy_device_id == [0u8; 16],
                "§5 reserves the all-zero LEGACY_DEVICE_ID for device-less envelopes"
            ),
            Scenario::StaleRosterSealing {
                revoked_device_still_listed,
                sealed_with_inbox_key_generation,
                current_inbox_key_generation,
                surviving_devices_still_receive,
            } => contract_assert!(
                vector.id,
                revoked_device_still_listed
                    && sealed_with_inbox_key_generation < current_inbox_key_generation
                    && surviving_devices_still_receive,
                "§6 stale-roster sealing is a bounded exposure, never a delivery brick"
            ),
            Scenario::AddDevice {
                resulting_device_count,
            } => {
                // §14.3, boundary as pinned by the 2026-08-16 decision: the
                // 8th device does not warn, the 9th does, 16 is allowed, the
                // 17th is refused.
                let expected = match resulting_device_count {
                    7 | 8 => Outcome::DeviceAdded,
                    9 | 16 => Outcome::DeviceAddedWithWarning,
                    17 => Outcome::DeviceAddRefused,
                    _ => unreachable!("unexpected §14.3 cap fixture"),
                };
                contract_assert!(
                    vector.id,
                    vector.target_outcome == expected,
                    "§14.3 soft-8/hard-16 add-device outcome changed"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn vectors_match_the_pinned_current_core_behaviour() {
    let actual: Vec<(&str, Outcome)> = VECTORS
        .iter()
        .filter_map(|vector| drive(vector).map(|current| (vector.id, current)))
        .collect();
    let expected: Vec<(&str, Outcome)> = PINNED_DRIVER_RESULTS.to_vec();
    assert_eq!(
        actual, expected,
        "today's driven core behaviour is pinned; a changed driver result needs a deliberate edit"
    );
}

#[test]
fn divergence_ledger_is_derived_from_driver_results() {
    let actual: Vec<_> = VECTORS
        .iter()
        .filter_map(|vector| {
            drive(vector)
                .and_then(|current| (current != vector.target_outcome).then_some(vector.id))
        })
        .collect();
    let expected = [
        "MD-STREAM-SIBLING-LAMPORT",
        "MD-ACK-SIBLING-FANOUT",
        "MD-ACK-LEGACY-PERSON-ROW",
        "MD-ACK-CARRIED-DIGEST-PROOF",
        "MD-CAPABILITY-RESERVED",
    ];
    assert_eq!(
        actual, expected,
        "implemented current-core divergences must be discovered from drivers"
    );
}

#[test]
fn unimplemented_vector_ledger_is_deliberate() {
    let actual: Vec<_> = VECTORS
        .iter()
        .filter(|vector| !vector.implemented)
        .map(|vector| vector.id)
        .collect();
    let expected = [
        "MD-ROSTER-GREATER",
        "MD-ROSTER-LOWER",
        "MD-ROSTER-EQUAL",
        "MD-ROSTER-SAME-EPOCH-ADVANCE",
        "MD-ROSTER-SAME-EPOCH-ROLLBACK",
        "MD-ROSTER-CHAIN-BROKEN",
        "MD-ROSTER-FORK",
        "MD-ROSTER-FORK-QUARANTINE-PERSISTS",
        "MD-ROSTER-TOMBSTONE",
        "MD-ROSTER-RELINK-FRESH-KEY",
        "MD-ROSTER-PAIRWISE-GOSSIP",
        "MD-ROSTER-KEYS-NOT-ENDPOINTS",
        "MD-RECOVERY-BACKUP-AUTHORITY",
        "MD-STREAM-LEGACY-ID",
        "MD-SEAL-STALE-ROSTER",
        "MD-DEVICE-CAP-7",
        "MD-DEVICE-CAP-8",
        "MD-DEVICE-CAP-9",
        "MD-DEVICE-CAP-16",
        "MD-DEVICE-CAP-17",
    ];
    assert_eq!(
        actual, expected,
        "the data-only WP0 vectors are a pinned ledger"
    );
}
