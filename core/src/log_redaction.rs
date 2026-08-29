//! Replaces network and device addresses in a diagnostics log line with short,
//! stable stand-ins.
//!
//! The connection log is this app's own captured output, and it is written to
//! be *shared*: a family member hands the whole archive to whoever is helping
//! them. What makes that log worth reading is also what makes it worth
//! minimising -- which peer, which link, in what order -- and none of that
//! needs the literal address. A Wi-Fi address, a Bluetooth device address and a
//! contact's public key are all permanent-ish identifiers that would otherwise
//! ride out of the phone in a support email, and the diagnostics note in both
//! shells promises they do not.
//!
//! Blanket removal would answer the promise and destroy the log. The whole
//! value of a connectivity trace is that two lines about the same peer can be
//! recognised as the same peer, and that "these two phones were on one
//! network" is visible. So every address is replaced by a stand-in derived from
//! a salt the phone keeps to itself:
//!
//! * the same address always yields the same stand-in, so lines still line up;
//! * an IPv4/IPv6 address keeps its network half and its host half separate,
//!   so same-subnet and different-host remain readable;
//! * ports, counts, timings and every non-address word are untouched.
//!
//! The salt never leaves the device, so a shared archive cannot be turned back
//! into an address, nor matched against an address someone already holds.
//!
//! Deliberately line-at-a-time: the captured log is bounded at a few megabytes
//! and both shells stream it, so nothing here should require holding a whole
//! log in memory.

use data_encoding::HEXLOWER;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

/// Hex characters below which a bare run is left alone.
///
/// A contact id and a message id are each sixteen bytes, printed as exactly
/// thirty-two hex characters -- every long run in a real capture measured
/// thirty-two and nothing measured more. So this is the length itself and not
/// a margin under it: raise it by one and every id in the log walks straight
/// through. Below it, ordinary hex-looking words -- "cafe", a status code, a
/// duration -- stay readable.
const MIN_ID_HEX_CHARS: usize = 32;

/// A fresh redaction salt, as lowercase hex.
///
/// Each shell generates this once and keeps it beside its capture switch, so
/// every export from one phone shares a namespace and a support thread spanning
/// two archives still reads as one story. It is discarded when the captured
/// logs are erased: that gesture means "forget what was recorded", and a salt
/// that outlived it would keep the old stand-ins meaningful.
#[uniffi::export]
pub fn core_new_log_redaction_salt() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    HEXLOWER.encode(&bytes)
}

/// One log line with every address it carries replaced by a stand-in.
///
/// Returns the line unchanged when it holds nothing of the sort, which is the
/// overwhelmingly common case.
#[uniffi::export]
pub fn core_redact_log_line(salt: String, line: String) -> String {
    let bytes = line.as_bytes();
    // Copied byte-wise rather than char-wise: every run byte is ASCII, and no
    // UTF-8 continuation byte is, so a run can never begin or end mid-character
    // and anything outside a run is passed through exactly as it arrived.
    let mut out: Vec<u8> = Vec::with_capacity(line.len());
    let mut index = 0;
    while index < bytes.len() {
        if !is_run_byte(bytes[index]) {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_run_byte(bytes[index]) {
            index += 1;
        }
        let run = &line[start..index];
        // Separators at the edges of a run belong to the sentence, not to the
        // address. This app writes a peer as
        // `HELLO from AA:BB:CC:DD:EE:FF: userId=...` and the platform writes
        // one as `bd_addr:ff:ff:ff:ff:ff:ff`; neither the trailing colon nor
        // the leading one may stop the address matching.
        let core = run.trim_matches(is_edge_char);
        let core_start = run.len() - run.trim_start_matches(is_edge_char).len();
        // ...but a candidate welded straight onto a word is part of the word.
        // `ClientState::RESUMED` and `DebugCommand::handleResponse` each end a
        // word in a hex letter and then carry `::`, which read alone is a
        // perfectly good spelling of the all-zero IPv6 address; matching it
        // there costs a letter out of a readable word and names something that
        // was never an address. A trimmed separator is itself a fence, which is
        // what keeps `bd_addr:` above from being read as a weld.
        let fenced = |trimmed: bool, neighbour: Option<usize>| {
            trimmed || !is_word_byte(neighbour.and_then(|at| bytes.get(at).copied()))
        };
        let candidate = fenced(core_start > 0, start.checked_sub(1))
            && fenced(core_start + core.len() < run.len(), Some(index));
        let run_bytes = run.as_bytes();
        out.extend_from_slice(&run_bytes[..core_start]);
        match candidate.then(|| redact_run(&salt, core)).flatten() {
            Some(replacement) => out.extend_from_slice(replacement.as_bytes()),
            None => out.extend_from_slice(core.as_bytes()),
        }
        out.extend_from_slice(&run_bytes[core_start + core.len()..]);
    }
    String::from_utf8(out).unwrap_or(line)
}

/// Whether the byte beside a run continues a word, so the run is part of an
/// identifier rather than an address of its own. Absent (start or end of line)
/// counts as a fence.
fn is_word_byte(byte: Option<u8>) -> bool {
    byte.is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
}

/// A separator that can sit *inside* an address and therefore also at the edge
/// of a candidate run, where it is punctuation instead.
fn is_edge_char(c: char) -> bool {
    c == ':' || c == '.' || c == '-'
}

/// Bytes that can be part of an address: hex digits and the three separators
/// addresses are written with. Everything else ends the run, which is what
/// keeps `BleCentral`, `retryAfter=` and a contact's name out of the scanner.
fn is_run_byte(byte: u8) -> bool {
    byte.is_ascii_hexdigit() || byte == b':' || byte == b'.' || byte == b'-'
}

/// The stand-in for one candidate run, or `None` to leave it alone.
///
/// Ordered most specific first. A logcat timestamp (`08-27`, `14:23:11.123`)
/// and an app version (`1.0.25`) reach here on every line and must fall
/// through all of them untouched, which is why each test below is exact rather
/// than "looks addressy".
fn redact_run(salt: &str, run: &str) -> Option<String> {
    if let Some(mac) = parse_mac(run) {
        return Some(format!("device-{}", tag(salt, "device", &mac)));
    }
    let (host_part, port) = split_port(run);
    if let Some(octets) = parse_ipv4(host_part) {
        return Some(with_port(
            &format!(
                "net-{}.host-{}",
                tag(salt, "net", &octets[..3]),
                tag(salt, "host", &octets[3..])
            ),
            port,
        ));
    }
    if let Some(address) = parse_ipv6(host_part) {
        return Some(with_port(
            &format!(
                "net-{}.host-{}",
                tag(salt, "net", &address[..8]),
                tag(salt, "host", &address[8..])
            ),
            port,
        ));
    }
    if run.len() >= MIN_ID_HEX_CHARS && run.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(format!("id-{}", tag(salt, "id", run.as_bytes())));
    }
    None
}

fn with_port(body: &str, port: Option<&str>) -> String {
    match port {
        Some(port) => format!("{body}:{port}"),
        None => body.to_string(),
    }
}

/// Four hex characters of a salted digest. Short on purpose: this is a name a
/// human reads and compares across lines, not a key. A collision costs one
/// confusing pair of lines in a log that holds a handful of peers.
fn tag(salt: &str, kind: &str, value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update([0u8]);
    hasher.update(kind.as_bytes());
    hasher.update([0u8]);
    hasher.update(value);
    HEXLOWER.encode(&hasher.finalize()[..2])
}

/// Splits a trailing `:<port>` off an address, if it is unambiguously one.
///
/// Only an IPv4-shaped run can carry a bare port; `fe80::1:8080` is an address,
/// not an address and a port, so a bracketed IPv6 endpoint gets here already
/// split by its brackets (which are not run bytes).
fn split_port(run: &str) -> (&str, Option<&str>) {
    let Some((host, port)) = run.rsplit_once(':') else {
        return (run, None);
    };
    let is_port = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && parse_ipv4(host).is_some();
    if is_port {
        (host, Some(port))
    } else {
        (run, None)
    }
}

/// Six hex pairs separated by `:` or `-`, as a Bluetooth or Ethernet address is
/// written. Case-folded via the parse, so `AA:BB:..` and `aa:bb:..` are one
/// device.
fn parse_mac(run: &str) -> Option<[u8; 6]> {
    let separator = if run.contains(':') { ':' } else { '-' };
    let mut parts = run.split(separator);
    let mut bytes = [0u8; 6];
    for slot in bytes.iter_mut() {
        let part = parts.next()?;
        if part.len() != 2 || !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(bytes)
}

/// Exactly four decimal octets. Rejects leading zeros so a date or an id that
/// happens to have three dots is not mistaken for an address.
fn parse_ipv4(run: &str) -> Option<[u8; 4]> {
    let mut parts = run.split('.');
    let mut octets = [0u8; 4];
    for slot in octets.iter_mut() {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if part.len() > 1 && part.starts_with('0') {
            return None;
        }
        *slot = part.parse::<u8>().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(octets)
}

/// A textual IPv6 address, normalised to its sixteen bytes so the two spellings
/// of one address share a stand-in.
///
/// Handles `::` compression and a trailing embedded IPv4. A zone suffix
/// (`%en0`) never reaches here because `%` is not a run byte, so the zone is
/// preserved verbatim beside the stand-in -- which is what a reader wants
/// anyway.
fn parse_ipv6(run: &str) -> Option<[u8; 16]> {
    if !run.contains(':') {
        return None;
    }
    let (head, tail) = match run.split_once("::") {
        Some((head, tail)) => {
            if tail.contains("::") {
                return None;
            }
            (head, Some(tail))
        }
        None => (run, None),
    };
    let mut leading = Vec::new();
    let mut trailing = Vec::new();
    parse_ipv6_groups(head, &mut leading)?;
    if let Some(tail) = tail {
        parse_ipv6_groups(tail, &mut trailing)?;
    }
    let total = leading.len() + trailing.len();
    match tail {
        // No compression: every one of the sixteen bytes must be written out.
        None if total != 16 => return None,
        // `::` has to stand for at least one group, or the address would have
        // been written without it.
        Some(_) if total >= 16 => return None,
        _ => {}
    }
    let mut address = [0u8; 16];
    address[..leading.len()].copy_from_slice(&leading);
    address[16 - trailing.len()..].copy_from_slice(&trailing);
    Some(address)
}

/// Appends the bytes of a colon-separated group list. An empty list is allowed
/// (either side of a leading or trailing `::`).
fn parse_ipv6_groups(text: &str, out: &mut Vec<u8>) -> Option<()> {
    if text.is_empty() {
        return Some(());
    }
    let groups: Vec<&str> = text.split(':').collect();
    for (index, group) in groups.iter().enumerate() {
        let last = index + 1 == groups.len();
        if last && group.contains('.') {
            out.extend_from_slice(&parse_ipv4(group)?);
            continue;
        }
        if group.is_empty() || group.len() > 4 || !group.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let value = u16::from_str_radix(group, 16).ok()?;
        out.extend_from_slice(&value.to_be_bytes());
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn lan_endpoint_is_replaced_and_keeps_its_port() {
        let redacted = core_redact_log_line(
            SALT.into(),
            "BLE introduced LAN peer at 192.168.1.42:7777".into(),
        );
        assert!(!redacted.contains("192.168.1.42"), "{redacted}");
        assert!(redacted.ends_with(":7777"), "{redacted}");
        assert!(redacted.starts_with("BLE introduced LAN peer at net-"));
    }

    #[test]
    fn bluetooth_device_address_is_replaced() {
        let redacted =
            core_redact_log_line(SALT.into(), "tearDownLink: AA:BB:CC:DD:EE:FF (idle)".into());
        assert!(!redacted.contains("AA:BB"), "{redacted}");
        assert!(redacted.starts_with("tearDownLink: device-"), "{redacted}");
        assert!(redacted.ends_with(" (idle)"), "{redacted}");
    }

    #[test]
    fn device_address_case_does_not_change_the_stand_in() {
        let upper = core_redact_log_line(SALT.into(), "AA:BB:CC:DD:EE:FF".into());
        let lower = core_redact_log_line(SALT.into(), "aa:bb:cc:dd:ee:ff".into());
        assert_eq!(upper, lower);
    }

    #[test]
    fn contact_id_is_replaced() {
        let id = "9f".repeat(32);
        let redacted = core_redact_log_line(SALT.into(), format!("HELLO from userId={id}"));
        assert!(!redacted.contains(&id), "{redacted}");
        assert!(redacted.starts_with("HELLO from userId=id-"), "{redacted}");
    }

    /// The length ids are actually written at: sixteen bytes, thirty-two hex
    /// characters. The test above uses a longer run, so on its own it would
    /// still pass with the threshold set anywhere up to sixty-four -- and a
    /// threshold of thirty-three lets every real id in the log through
    /// untouched. Both lines here name one peer, so both have to go, and both
    /// have to end up naming the same peer.
    #[test]
    fn an_id_at_the_length_the_log_writes_is_replaced() {
        let id = "fb4bde6a43ac243ef08fa0910aebe505";
        assert_eq!(id.len(), MIN_ID_HEX_CHARS);
        let hello = core_redact_log_line(SALT.into(), format!("HELLO from lan:peer: userId={id}"));
        let delivery = core_redact_log_line(
            SALT.into(),
            format!("Confirmed delivery of 24 carried envelope(s) to {id}; dropped our copy"),
        );
        assert!(!hello.contains(id), "{hello}");
        assert!(!delivery.contains(id), "{delivery}");
        let stand_in = hello.rsplit_once("userId=").unwrap().1.to_string();
        assert!(stand_in.starts_with("id-"), "{hello}");
        assert!(delivery.contains(&stand_in), "{delivery}");
    }

    #[test]
    fn the_same_address_gets_the_same_stand_in_within_a_run() {
        let first = core_redact_log_line(SALT.into(), "connected 10.0.0.7:7777".into());
        let second = core_redact_log_line(SALT.into(), "closed 10.0.0.7:7777".into());
        let first_tag = first.trim_start_matches("connected ");
        let second_tag = second.trim_start_matches("closed ");
        assert_eq!(first_tag, second_tag);
    }

    #[test]
    fn a_different_salt_gives_a_different_stand_in() {
        let one = core_redact_log_line(SALT.into(), "10.0.0.7".into());
        let two =
            core_redact_log_line("ffffffffffffffffffffffffffffffff".into(), "10.0.0.7".into());
        assert_ne!(one, two);
    }

    #[test]
    fn same_subnet_shares_a_network_half_and_differs_in_the_host_half() {
        let one = core_redact_log_line(SALT.into(), "10.0.0.7".into());
        let two = core_redact_log_line(SALT.into(), "10.0.0.9".into());
        let three = core_redact_log_line(SALT.into(), "10.9.0.7".into());
        let network = |line: &str| line.split_once(".host-").unwrap().0.to_string();
        assert_eq!(network(&one), network(&two));
        assert_ne!(one, two);
        assert_ne!(network(&one), network(&three));
    }

    #[test]
    fn ipv6_spellings_of_one_address_share_a_stand_in() {
        let short = core_redact_log_line(SALT.into(), "fe80::1".into());
        let long = core_redact_log_line(
            SALT.into(),
            "fe80:0000:0000:0000:0000:0000:0000:0001".into(),
        );
        assert_eq!(short, long);
        assert!(short.starts_with("net-"), "{short}");
    }

    #[test]
    fn a_bracketed_ipv6_endpoint_keeps_its_brackets_and_port() {
        let redacted = core_redact_log_line(SALT.into(), "peer [fe80::1]:7777 answered".into());
        assert!(redacted.starts_with("peer [net-"), "{redacted}");
        assert!(redacted.ends_with("]:7777 answered"), "{redacted}");
    }

    /// How this app writes a peer on its own busiest lines. The colon after the
    /// address belongs to the sentence, and an earlier cut of this scanner let
    /// every one of these through untouched -- the worst possible miss, since
    /// these are the lines that name a device.
    #[test]
    fn a_device_address_followed_by_a_colon_is_still_replaced() {
        for line in [
            "HELLO from AA:BB:CC:DD:EE:FF: userId=abc",
            "MTU negotiated for AA:BB:CC:DD:EE:FF: 517",
            "Receipt from AA:BB:CC:DD:EE:FF: ackedSender=42 throughLamport=267",
            "port_find_mcb: not found, bd_addr:AA:BB:CC:DD:EE:FF",
        ] {
            let redacted = core_redact_log_line(SALT.into(), line.into());
            assert!(!redacted.contains("AA:BB"), "{redacted}");
            assert!(redacted.contains("device-"), "{redacted}");
            // Every colon the sentence owns has to survive where it was; only
            // the five inside the address may go.
            assert_eq!(
                redacted.matches(':').count(),
                line.matches(':').count() - 5,
                "{redacted}"
            );
        }
    }

    /// A run welded to a word is part of the word. `::` alone spells the
    /// all-zero IPv6 address, so a scanner that does not check its edges eats a
    /// letter out of every C++ scope operator whose left side ends in a-f.
    #[test]
    fn a_scope_operator_inside_an_identifier_is_left_alone() {
        for line in [
            "resume_registered_clients: client is not paused ClientState::RESUMED(0x3)",
            "update_connectability_state: state:ConnectabilityState::ARMED status:SUCCESS",
            "In DebugCommand::handleResponse, mType:GET_FEATURE",
            "system/stack/acl/btm_acl.cc:194 disconnect_acl: All channels closed",
            "at com.cruisemesh.app.mesh.MeshService::onStartCommand(MeshService.kt:412)",
        ] {
            assert_eq!(core_redact_log_line(SALT.into(), line.into()), line);
        }
    }

    /// Service and characteristic UUIDs are the app's own constants, identify
    /// nobody, and are how a reader tells one BLE channel from another.
    #[test]
    fn a_service_uuid_stays_readable() {
        let line = "Write request for a5987315-cdcf-4e09-b036-ce10af3c05d4 (21 bytes)";
        assert_eq!(core_redact_log_line(SALT.into(), line.into()), line);
    }

    /// The pass label both shells log is short, salt-free and derived in the
    /// core so two archives from two phones can be compared. It is eight hex
    /// characters, which is under [MIN_ID_HEX_CHARS] and so passes through --
    /// and has to keep doing so, because a stand-in would be per-phone and
    /// answer nothing. The link-name a peer is written under is likewise the
    /// only handle a reader has on one session.
    #[test]
    fn the_pass_label_and_a_link_name_survive() {
        for line in [
            "Relay configured: host=relay.cruisemesh.app pass=056855d3 epoch=3 shareOnline=true",
            "Relay request failed (503) [upstream_unavailable] body=512B",
            "HELLO from lan:07525959-6f17-445a-acbb-aa3648f4cbc0",
        ] {
            assert_eq!(core_redact_log_line(SALT.into(), line.into()), line);
        }
    }

    #[test]
    fn an_ordinary_line_is_returned_untouched() {
        let line = "08-27 14:23:11.123  4021  4099 I MeshService: \
                    Relay sync complete: configs=2 net=wifi reason=periodic in 1234ms, 512B";
        assert_eq!(core_redact_log_line(SALT.into(), line.into()), line);
    }

    #[test]
    fn version_and_status_words_survive() {
        for line in [
            "CruiseMesh 1.0.25 (25) Pixel 7 Android 16",
            "MTU negotiated: 517 (status=0)",
            "GET /envelopes -> 429 [rate_limited] in 88ms retryAfter=30",
            "Imported group Deck 12 from invite",
            "sendFrame: queued 3 fragment(s) for a-peer (512 bytes)",
        ] {
            assert_eq!(core_redact_log_line(SALT.into(), line.into()), line);
        }
    }

    #[test]
    fn a_relay_hostname_is_left_readable() {
        let line = "Relay sync failed for https://relay.cruisemesh.app: timeout";
        assert_eq!(core_redact_log_line(SALT.into(), line.into()), line);
    }

    #[test]
    fn an_address_inside_a_connection_error_is_replaced() {
        let redacted = core_redact_log_line(
            SALT.into(),
            "ConnectException: failed to connect to relay.example.com/203.0.113.5 (port 443) \
             from /192.168.1.7 (port 41234)"
                .into(),
        );
        assert!(!redacted.contains("203.0.113.5"), "{redacted}");
        assert!(!redacted.contains("192.168.1.7"), "{redacted}");
        assert!(redacted.contains("relay.example.com"), "{redacted}");
        assert!(redacted.contains("(port 443)"), "{redacted}");
    }

    #[test]
    fn non_ascii_text_survives_the_scan() {
        let line = "Imported group Café · Deck 12 from 10.0.0.7";
        let redacted = core_redact_log_line(SALT.into(), line.into());
        assert!(redacted.starts_with("Imported group Café · Deck 12 from net-"));
        assert!(!redacted.contains("10.0.0.7"));
    }

    #[test]
    fn redacting_twice_is_stable() {
        let once = core_redact_log_line(
            SALT.into(),
            "peer 192.168.1.42:7777 AA:BB:CC:DD:EE:FF".into(),
        );
        let twice = core_redact_log_line(SALT.into(), once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn a_fresh_salt_is_hex_and_not_repeated() {
        let salt = core_new_log_redaction_salt();
        assert_eq!(salt.len(), 32);
        assert!(salt.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(salt, core_new_log_redaction_salt());
    }
}
