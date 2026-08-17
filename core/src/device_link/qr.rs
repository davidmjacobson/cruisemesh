//! The `CMLINK1:` link QR (`specs/multi-device-v1.md` §9.1).
//!
//! The new device shows it; the approving device scans it. It carries
//! **ephemeral link material only**: a fresh X25519 public key minted for this
//! one ceremony, when the offer stops being valid, and the rendezvous hints the
//! scanner needs to reach this device. Nothing else fits — the payload has no
//! field a person id, a device id, a name, a roster, or a secret could ride in,
//! and `qr_carries_no_identity_material` walks the encoded bytes to prove it.
//!
//! # Why there is no relay credential in here
//!
//! A relay rendezvous needs a relay base URL and a family token. The URL is a
//! hint and rides here; the token never does. It does not need to: the two
//! devices are the same person, so the approving device already holds the
//! family token for the very family whose mailbox the hint names. A new device
//! that has no relay credentials yet simply publishes no relay hint and the
//! ceremony runs over LAN or BLE — which is the co-present case anyway, since
//! the QR is scanned by a camera. Putting the token in the QR would have made a
//! photograph of a screen worth a family's mailbox.
//!
//! # Endpoint privacy
//!
//! Every hint here is the NEW device's own endpoint, published by the device
//! that owns it to the one device that is about to become its sibling. Nothing
//! discovered, nothing third-party, nothing forwarded (DL-5's rule applied one
//! layer out; the roster itself still carries keys and never endpoints).
//!
//! # Forward tolerance (WPT)
//!
//! Every hint is tag- and length-framed, so a hint kind this build does not
//! know is skipped rather than rejected, and trailing bytes after the declared
//! hints are ignored. A payload version above this build's is
//! [`CoreError::UnsupportedLink`] — the "update the app" fail-soft, never a
//! half-parsed rendezvous.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use data_encoding::BASE64URL_NOPAD;

use crate::identity::extract_link_body;
use crate::lan_session::LAN_DEFAULT_TCP_PORT;
use crate::lan_util::core_parse_lan_endpoint;
use crate::relay_wire::normalize_relay_url;
use crate::CoreError;

/// The link-ceremony scheme (`specs/multi-device-v1.md` §9). Shipped as an
/// "update the app" fail-soft in `identity.rs` since WPT; this module is what
/// finally reads it.
pub const DEVICE_LINK_PREFIX: &str = "CMLINK1:";
/// URL form of the same payload, for the `/link` deep-link route. The ceremony
/// is an in-app scan, so [`core_build_link_qr`] emits the bare `CMLINK1:` token
/// — a smaller QR, and a system camera cannot send the user to a web page that
/// does not exist. The URL form is accepted on parse and available for a shell
/// that wants a tappable link.
const DEVICE_LINK_URL_BASE: &str = "https://cruisemesh.app/link#";

/// Payload version. A higher one is [`CoreError::UnsupportedLink`].
const LINK_QR_VERSION: u8 = 1;
/// X25519 public key length.
const LINK_PK_LEN: usize = 32;
/// Hard ceiling on the decoded payload. A QR a phone camera can read across a
/// table is a few hundred bytes; this bounds what a scanner will ever decode.
pub const LINK_QR_MAX_BYTES: usize = 512;
/// Most rendezvous hints one payload may carry, LAN and relay together.
pub const LINK_QR_MAX_HINTS: usize = 8;
/// Most bytes one hint may carry.
const LINK_QR_MAX_HINT_BYTES: usize = 128;
/// How long a freshly built offer stands by default. Short on purpose: the QR
/// is on a screen in front of the person holding both devices, and a stale
/// offer left in a photo roll should not still open a channel.
pub const LINK_QR_DEFAULT_LIFETIME_MS: i64 = 5 * 60 * 1_000;

/// Hint kinds. Unknown tags are skipped on parse (WPT forward tolerance).
const HINT_TAG_LAN: u8 = 0x01;
const HINT_TAG_RELAY: u8 = 0x02;

/// Length of the ephemeral rendezvous namespace — the same 16 bytes
/// [`compute_recipient_hint`](crate::compute_recipient_hint) consumes, so a
/// link rendezvous drops into every existing hint path where a `user_id` goes.
pub const LINK_RENDEZVOUS_ID_LEN: usize = 16;
/// Domain separator for the rendezvous namespace. Not a signing domain: it
/// keeps this derivation disjoint from every other digest the crate computes
/// over a public key.
const LINK_RENDEZVOUS_DOMAIN: &[u8] = b"CruiseMesh device link rendezvous v1\0";
/// Domain separator for one direction of a rendezvous. Distinct from the
/// namespace domain above, so a lane can never collide with the rendezvous id
/// it was derived from.
const LINK_RENDEZVOUS_LANE_DOMAIN: &[u8] = b"CruiseMesh device link lane v1\0";

/// Everything the `CMLINK1:` QR carries.
///
/// Note what is absent, because the absence is the point: no `person_id`, no
/// `device_id`, no signing key, no relay token, no name, no roster head. A
/// scanner learns where to knock and which ephemeral key answers, and nothing
/// about who lives there.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct LinkRendezvous {
    /// The new device's ephemeral X25519 public key for this one ceremony
    /// (32 bytes). Minted per ceremony and thrown away with it: it is never the
    /// device's long-term agreement key, so a photographed QR ages into
    /// nothing.
    pub link_pk: Vec<u8>,
    /// Unix milliseconds after which this offer is dead.
    pub expires_at_ms: i64,
    /// The new device's own LAN endpoints, `host:port` as
    /// [`core_format_lan_endpoint`](crate::core_format_lan_endpoint) writes
    /// them.
    pub lan_endpoints: Vec<String>,
    /// Relay base URLs the new device can already reach. Empty on a fresh
    /// install with no Shore Pass applied, which simply means the ceremony runs
    /// over LAN or BLE.
    pub relay_base_urls: Vec<String>,
}

/// The ephemeral mailbox namespace both devices derive from the QR's key:
/// `BLAKE2b-16(domain || link_pk)`.
///
/// It is derived rather than carried for two reasons. It cannot disagree with
/// the key the channel is bound to, and it costs the QR nothing. It is
/// deliberately not a person or device namespace: a link rendezvous is
/// short-lived, is never reused as a contact hint, and dies with the ceremony,
/// so it spends none of relayd's `MAX_FETCH_HINTS` budget beyond the minutes
/// the ceremony is live.
#[uniffi::export]
pub fn core_link_rendezvous_id(link_pk: Vec<u8>) -> Result<Vec<u8>, CoreError> {
    if link_pk.len() != LINK_PK_LEN {
        return Err(CoreError::InvalidKeyLength {
            expected: LINK_PK_LEN as u32,
            actual: link_pk.len() as u32,
        });
    }
    let mut hasher = Blake2bVar::new(LINK_RENDEZVOUS_ID_LEN).expect("valid blake2b output length");
    hasher.update(LINK_RENDEZVOUS_DOMAIN);
    hasher.update(&link_pk);
    let mut out = vec![0u8; LINK_RENDEZVOUS_ID_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    Ok(out)
}

/// Which half of a rendezvous a mailbox belongs to.
///
/// A store-and-forward rendezvous needs two mailboxes, not one: under a single
/// namespace each side would fetch its own posts straight back and hand them to
/// a state machine that has no state for them. The lane is the direction, named
/// for the end that reads it.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLinkLane {
    /// What the new device sends and the approving device reads.
    ToApprovingDevice,
    /// What the approving device sends and the new device reads.
    ToNewDevice,
}

/// The mailbox namespace one direction of a relay rendezvous uses:
/// `BLAKE2b-16(domain || lane || rendezvous_id)`.
///
/// Both shells derive this from the same scanned offer, which is why it lives
/// here and not in a shell: a lane byte invented twice is a lane byte invented
/// differently, and the two halves would poll past each other forever. Feed the
/// result to [`compute_recipient_hint`](crate::compute_recipient_hint) exactly
/// as a `user_id` is fed to it — a lane is a namespace, not a hint.
///
/// It inherits everything [`core_link_rendezvous_id`] is: ephemeral, derived
/// rather than carried, never reused as a contact hint, and dead when the
/// ceremony is.
#[uniffi::export]
pub fn core_link_rendezvous_lane(
    rendezvous_id: Vec<u8>,
    lane: CoreLinkLane,
) -> Result<Vec<u8>, CoreError> {
    if rendezvous_id.len() != LINK_RENDEZVOUS_ID_LEN {
        return Err(malformed("device-link rendezvous id must be 16 bytes"));
    }
    let lane_byte: u8 = match lane {
        CoreLinkLane::ToApprovingDevice => 0x01,
        CoreLinkLane::ToNewDevice => 0x02,
    };
    let mut hasher = Blake2bVar::new(LINK_RENDEZVOUS_ID_LEN).expect("valid blake2b output length");
    hasher.update(LINK_RENDEZVOUS_LANE_DOMAIN);
    hasher.update(&[lane_byte]);
    hasher.update(&rendezvous_id);
    let mut out = vec![0u8; LINK_RENDEZVOUS_ID_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    Ok(out)
}

/// Build the QR text the new device displays (§9.1).
///
/// Rejects rather than silently drops: these are the caller's own endpoints, so
/// a hint that will not survive the round trip is a bug in the shell, not a
/// peer being generous. Parsing is the lenient direction.
#[uniffi::export]
pub fn core_build_link_qr(rendezvous: LinkRendezvous) -> Result<String, CoreError> {
    let payload = encode_link_payload(&rendezvous)?;
    Ok(format!(
        "{DEVICE_LINK_PREFIX}{}",
        BASE64URL_NOPAD.encode(&payload)
    ))
}

/// Parse a scanned `CMLINK1:` payload, bare or inside a `/link#` URL.
#[uniffi::export]
pub fn core_parse_link_qr(text: String) -> Result<LinkRendezvous, CoreError> {
    let trimmed = text.trim();
    if trimmed.len() > LINK_QR_MAX_BYTES * 4 {
        return Err(malformed("link code is too large"));
    }
    let encoded = extract_link_body(trimmed, DEVICE_LINK_PREFIX)
        .ok_or_else(|| malformed("not a CruiseMesh device-link code"))?;
    let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    let payload = BASE64URL_NOPAD
        .decode(compact.as_bytes())
        .map_err(|_| malformed("invalid device-link base64url"))?;
    decode_link_payload(&payload)
}

/// The tappable form of a QR text, for a shell that wants one. Empty for
/// anything that is not a `CMLINK1:` payload.
#[uniffi::export]
pub fn core_link_qr_url(qr_text: String) -> String {
    let trimmed = qr_text.trim();
    if !trimmed.starts_with(DEVICE_LINK_PREFIX) {
        return String::new();
    }
    format!("{DEVICE_LINK_URL_BASE}{trimmed}")
}

// ---------------------------------------------------------------------------
// Wire layout
// ---------------------------------------------------------------------------
//
// version(1) || link_pk(32) || expires_at_ms(8, big-endian i64) || hint_count(1)
//   then hint_count × [ tag(1) || len(1) || bytes(len) ]
//
// Frozen by `golden_link_qr_payload`. Every field is fixed-width or
// length-framed, so an unknown hint tag is skippable and a later additive field
// still parses on this build.

pub(crate) fn encode_link_payload(rendezvous: &LinkRendezvous) -> Result<Vec<u8>, CoreError> {
    if rendezvous.link_pk.len() != LINK_PK_LEN {
        return Err(CoreError::InvalidKeyLength {
            expected: LINK_PK_LEN as u32,
            actual: rendezvous.link_pk.len() as u32,
        });
    }
    if rendezvous.expires_at_ms <= 0 {
        return Err(malformed("device-link offer has no expiry"));
    }
    let hints = rendezvous.lan_endpoints.len() + rendezvous.relay_base_urls.len();
    if hints > LINK_QR_MAX_HINTS {
        return Err(malformed("device-link offer carries too many hints"));
    }

    let mut out = Vec::with_capacity(64);
    out.push(LINK_QR_VERSION);
    out.extend_from_slice(&rendezvous.link_pk);
    out.extend_from_slice(&rendezvous.expires_at_ms.to_be_bytes());
    out.push(hints as u8);
    for endpoint in &rendezvous.lan_endpoints {
        let endpoint = endpoint.trim();
        if core_parse_lan_endpoint(endpoint.to_string(), LAN_DEFAULT_TCP_PORT).is_none() {
            return Err(malformed("device-link LAN hint is not an endpoint"));
        }
        push_hint(&mut out, HINT_TAG_LAN, endpoint.as_bytes())?;
    }
    for url in &rendezvous.relay_base_urls {
        // The same chokepoint every other relay URL in the app passes through:
        // an insecure or unusable URL normalizes to empty and is refused here
        // rather than being published in a QR.
        let normalized = normalize_relay_url(url.clone());
        if normalized.is_empty() {
            return Err(malformed(
                "device-link relay hint is not a usable https URL",
            ));
        }
        push_hint(&mut out, HINT_TAG_RELAY, normalized.as_bytes())?;
    }
    if out.len() > LINK_QR_MAX_BYTES {
        return Err(malformed("device-link offer is too large for a QR"));
    }
    Ok(out)
}

pub(crate) fn decode_link_payload(bytes: &[u8]) -> Result<LinkRendezvous, CoreError> {
    if bytes.len() > LINK_QR_MAX_BYTES {
        return Err(malformed("device-link offer is too large"));
    }
    let mut reader = Reader { bytes, at: 0 };
    let version = reader.u8()?;
    if version > LINK_QR_VERSION {
        // WPT item 2's fail-soft: a newer scheme is "update the app", never a
        // guess at what the new fields meant.
        return Err(CoreError::UnsupportedLink);
    }
    if version < LINK_QR_VERSION {
        return Err(malformed("device-link offer has an unknown version"));
    }
    let link_pk = reader.take(LINK_PK_LEN)?.to_vec();
    let expires_at_ms = i64::from_be_bytes(
        reader
            .take(8)?
            .try_into()
            .expect("eight bytes is an i64's width"),
    );
    if expires_at_ms <= 0 {
        return Err(malformed("device-link offer has no expiry"));
    }
    let hint_count = reader.u8()? as usize;
    if hint_count > LINK_QR_MAX_HINTS {
        return Err(malformed("device-link offer carries too many hints"));
    }

    let mut lan_endpoints = Vec::new();
    let mut relay_base_urls = Vec::new();
    for _ in 0..hint_count {
        let tag = reader.u8()?;
        let len = reader.u8()? as usize;
        let value = reader.take(len)?;
        match tag {
            HINT_TAG_LAN => {
                // Lenient in this direction: a hint that will not parse is one
                // route lost, not a reason to refuse a ceremony the other hints
                // can still carry.
                if let Ok(text) = std::str::from_utf8(value) {
                    if core_parse_lan_endpoint(text.to_string(), LAN_DEFAULT_TCP_PORT).is_some() {
                        lan_endpoints.push(text.to_string());
                    }
                }
            }
            HINT_TAG_RELAY => {
                if let Ok(text) = std::str::from_utf8(value) {
                    let normalized = normalize_relay_url(text.to_string());
                    if !normalized.is_empty() {
                        relay_base_urls.push(normalized);
                    }
                }
            }
            // A hint kind a later build adds. Length-framed, so it is skipped
            // exactly, and this build carries on with the hints it knows.
            _ => {}
        }
    }
    // Trailing bytes after the declared hints belong to a later version and are
    // deliberately ignored, the same tolerance `decode_friend_card_binary_v4`
    // keeps.
    Ok(LinkRendezvous {
        link_pk,
        expires_at_ms,
        lan_endpoints,
        relay_base_urls,
    })
}

fn push_hint(out: &mut Vec<u8>, tag: u8, value: &[u8]) -> Result<(), CoreError> {
    if value.is_empty() || value.len() > LINK_QR_MAX_HINT_BYTES {
        return Err(malformed("device-link hint has an unusable length"));
    }
    out.push(tag);
    out.push(value.len() as u8);
    out.extend_from_slice(value);
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], CoreError> {
        let end = self
            .at
            .checked_add(len)
            .ok_or_else(|| malformed("device-link offer is truncated"))?;
        if end > self.bytes.len() {
            return Err(malformed("device-link offer is truncated"));
        }
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, CoreError> {
        Ok(self.take(1)?[0])
    }
}

fn malformed(detail: &str) -> CoreError {
    CoreError::Malformed(detail.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_roster::generate_device_keypair;
    use crate::identity::generate_identity;

    /// Fixed key, never `generate_*`: the golden vector below is only worth
    /// anything if every byte that feeds it is pinned here.
    const LINK_PK: [u8; 32] = [
        0x9a, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];
    const EXPIRES_AT_MS: i64 = 1_755_000_000_000;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn fixture() -> LinkRendezvous {
        LinkRendezvous {
            link_pk: LINK_PK.to_vec(),
            expires_at_ms: EXPIRES_AT_MS,
            lan_endpoints: vec!["192.168.1.24:45892".to_string()],
            relay_base_urls: vec!["https://relay.example".to_string()],
        }
    }

    /// The QR payload layout, frozen. A deliberate format change edits this
    /// vector; an accidental one fails here.
    #[test]
    fn golden_link_qr_payload() {
        let payload = encode_link_payload(&fixture()).unwrap();
        assert_eq!(
            hex(&payload),
            concat!(
                // version
                "01",
                // link_pk
                "9a0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                // expires_at_ms
                "000001989e26ce00",
                // hint count
                "02",
                // LAN hint: tag, length, "192.168.1.24:45892"
                "0112",
                "3139322e3136382e312e32343a3435383932",
                // relay hint: tag, length, "https://relay.example"
                "0215",
                "68747470733a2f2f72656c61792e6578616d706c65",
            )
        );
        assert_eq!(
            core_build_link_qr(fixture()).unwrap(),
            "CMLINK1:AZoBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fAAABmJ4mzgACARIxOTIuMTY4LjEuMjQ6NDU4OTICFWh0dHBzOi8vcmVsYXkuZXhhbXBsZQ"
        );
    }

    /// The rendezvous namespace, frozen with it: both devices derive it, so a
    /// drift here would silently un-meet a fleet mid-rollout.
    #[test]
    fn golden_link_rendezvous_id() {
        assert_eq!(
            hex(&core_link_rendezvous_id(LINK_PK.to_vec()).unwrap()),
            "dcf05220181ae9e9abbfc7de0286318e"
        );
        assert_eq!(
            core_link_rendezvous_id(LINK_PK.to_vec()).unwrap().len(),
            LINK_RENDEZVOUS_ID_LEN
        );
        // A different offer is a different mailbox.
        let other = core_link_rendezvous_id(vec![0x11; 32]).unwrap();
        assert_ne!(other, core_link_rendezvous_id(LINK_PK.to_vec()).unwrap());
        assert!(core_link_rendezvous_id(vec![0u8; 31]).is_err());
    }

    /// The two lanes of one rendezvous, frozen for the same reason: a shell that
    /// derived either of them differently would poll a mailbox nobody posts to.
    #[test]
    fn golden_link_rendezvous_lanes() {
        let rendezvous_id = core_link_rendezvous_id(LINK_PK.to_vec()).unwrap();
        let to_approver =
            core_link_rendezvous_lane(rendezvous_id.clone(), CoreLinkLane::ToApprovingDevice)
                .unwrap();
        let to_newcomer =
            core_link_rendezvous_lane(rendezvous_id.clone(), CoreLinkLane::ToNewDevice).unwrap();
        assert_eq!(hex(&to_approver), "20b214e512898546fc6fa0833731e315");
        assert_eq!(hex(&to_newcomer), "807a53b64a74f743cf9324d41bc2cc19");

        // The whole point of a lane: neither half reads its own posts back, and
        // neither is the rendezvous id itself.
        assert_ne!(to_approver, to_newcomer);
        assert_ne!(to_approver, rendezvous_id);
        assert_ne!(to_newcomer, rendezvous_id);
        assert_eq!(to_approver.len(), LINK_RENDEZVOUS_ID_LEN);

        // A different offer is a different pair of mailboxes.
        let other = core_link_rendezvous_id(vec![0x11; 32]).unwrap();
        assert_ne!(
            core_link_rendezvous_lane(other, CoreLinkLane::ToApprovingDevice).unwrap(),
            to_approver
        );
        assert!(core_link_rendezvous_lane(vec![0u8; 15], CoreLinkLane::ToNewDevice).is_err());
    }

    /// **The §9.1 gate.** The QR carries ephemeral link material and nothing
    /// else. This walks the encoded bytes for every secret and every stable
    /// identifier a device holds and refuses to find one.
    #[test]
    fn qr_carries_no_identity_material() {
        let identity = generate_identity();
        let device = generate_device_keypair();
        let mut rendezvous = fixture();
        rendezvous.link_pk = device.agree_pk.clone();

        let payload = encode_link_payload(&rendezvous).unwrap();
        let text = core_build_link_qr(rendezvous.clone()).unwrap();

        for (label, secret) in [
            ("person root secret", identity.sign_sk.clone()),
            ("person root public key", identity.sign_pk.clone()),
            ("person id", identity.user_id.clone()),
            ("person agreement secret", identity.agree_sk.clone()),
            ("person agreement public key", identity.agree_pk.clone()),
            ("device signing secret", device.sign_sk.clone()),
            ("device signing public key", device.sign_pk.clone()),
            ("device id", device.device_id.clone()),
            ("device agreement secret", device.agree_sk.clone()),
        ] {
            assert!(
                !payload
                    .windows(secret.len())
                    .any(|w| w == secret.as_slice()),
                "{label} reached the QR payload"
            );
            assert!(
                !text.contains(&BASE64URL_NOPAD.encode(&secret)),
                "{label} reached the QR text"
            );
        }

        // The one 32-byte key in there is the ephemeral public key, and it sits
        // at the one offset the layout allows.
        assert_eq!(&payload[1..33], rendezvous.link_pk.as_slice());
        assert_eq!(payload.len(), 42 + 2 + 18 + 2 + 21);
    }

    #[test]
    fn qr_round_trips_bare_and_in_a_url() {
        let text = core_build_link_qr(fixture()).unwrap();
        assert_eq!(core_parse_link_qr(text.clone()).unwrap(), fixture());

        let url = core_link_qr_url(text.clone());
        assert!(url.starts_with("https://cruisemesh.app/link#"));
        assert_eq!(core_parse_link_qr(url).unwrap(), fixture());

        let in_prose = format!("scan this: {text} — then tap confirm");
        assert_eq!(core_parse_link_qr(in_prose).unwrap(), fixture());
        assert_eq!(core_link_qr_url("CMFRIEND3:abc".to_string()), "");
    }

    /// WPT forward tolerance, applied to this payload: a hint kind this build
    /// does not know is skipped, and bytes after the declared hints belong to a
    /// later version.
    #[test]
    fn unknown_hint_kinds_and_trailing_bytes_are_tolerated() {
        let mut payload = encode_link_payload(&fixture()).unwrap();
        payload[41] = 3; // one more hint than the two encoded
        payload.extend_from_slice(&[0x7f, 0x03, 0xaa, 0xbb, 0xcc]); // unknown kind
        payload.extend_from_slice(b"a later field entirely");

        let parsed = decode_link_payload(&payload).unwrap();
        assert_eq!(parsed, fixture());
    }

    #[test]
    fn a_newer_payload_version_is_the_update_the_app_fail_soft() {
        let mut payload = encode_link_payload(&fixture()).unwrap();
        payload[0] = LINK_QR_VERSION + 1;
        assert!(matches!(
            decode_link_payload(&payload),
            Err(CoreError::UnsupportedLink)
        ));

        payload[0] = 0;
        assert!(matches!(
            decode_link_payload(&payload),
            Err(CoreError::Malformed(_))
        ));
    }

    #[test]
    fn truncated_and_oversized_payloads_are_refused() {
        let payload = encode_link_payload(&fixture()).unwrap();
        for cut in [0, 1, 20, 33, 40, 44, 50] {
            assert!(
                decode_link_payload(&payload[..cut]).is_err(),
                "a {cut}-byte payload parsed"
            );
        }
        assert!(decode_link_payload(&vec![1u8; LINK_QR_MAX_BYTES + 1]).is_err());
        assert!(core_parse_link_qr("CMLINK1:not base64!!".to_string()).is_err());
        assert!(core_parse_link_qr("CMFRIEND3:abc".to_string()).is_err());
    }

    #[test]
    fn hints_are_the_new_devices_own_usable_endpoints() {
        let mut rendezvous = fixture();
        rendezvous.relay_base_urls = vec!["http://relay.example".to_string()];
        assert!(
            core_build_link_qr(rendezvous).is_err(),
            "an insecure relay hint must never be published"
        );

        let mut rendezvous = fixture();
        rendezvous.lan_endpoints = vec!["not an endpoint".to_string()];
        assert!(core_build_link_qr(rendezvous).is_err());

        let mut rendezvous = fixture();
        rendezvous.lan_endpoints = vec!["10.0.0.2:45892".to_string(); LINK_QR_MAX_HINTS + 1];
        rendezvous.relay_base_urls = Vec::new();
        assert!(core_build_link_qr(rendezvous).is_err());

        let mut rendezvous = fixture();
        rendezvous.expires_at_ms = 0;
        assert!(core_build_link_qr(rendezvous).is_err());

        let mut rendezvous = fixture();
        rendezvous.link_pk = vec![0u8; 31];
        assert!(core_build_link_qr(rendezvous).is_err());
    }

    /// A scanner drops a hint it cannot use rather than refusing the whole
    /// offer: one lost route, not a lost ceremony.
    #[test]
    fn a_scanner_drops_unusable_hints_and_keeps_the_rest() {
        let mut payload = encode_link_payload(&LinkRendezvous {
            lan_endpoints: vec!["192.168.1.24:45892".to_string()],
            relay_base_urls: Vec::new(),
            ..fixture()
        })
        .unwrap();
        payload[41] = 2;
        push_hint(&mut payload, HINT_TAG_RELAY, b"http://plaintext.example").unwrap();

        let parsed = decode_link_payload(&payload).unwrap();
        assert_eq!(parsed.lan_endpoints, vec!["192.168.1.24:45892".to_string()]);
        assert!(parsed.relay_base_urls.is_empty());
    }

    /// A ceremony with no hints at all is legal — the two devices met over BLE,
    /// or the shell already knows where to knock.
    #[test]
    fn an_offer_may_carry_no_hints() {
        let bare = LinkRendezvous {
            link_pk: LINK_PK.to_vec(),
            expires_at_ms: EXPIRES_AT_MS,
            lan_endpoints: Vec::new(),
            relay_base_urls: Vec::new(),
        };
        let text = core_build_link_qr(bare.clone()).unwrap();
        assert_eq!(core_parse_link_qr(text).unwrap(), bare);
    }
}
