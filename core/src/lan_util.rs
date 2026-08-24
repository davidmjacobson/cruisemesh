//! Pure same-LAN endpoint and network utilities.

use data_encoding::{BASE64URL, BASE64URL_NOPAD};
use sha2::{Digest, Sha256};

const LAN_LINK_PREFIX: &str = "CMLAN1:";
const LAN_LINK_BASE: &str = "https://cruisemesh.app/lan#";
pub const LAN_ENDPOINT_CACHE_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
pub const LAN_ENDPOINT_RESEND_INTERVAL_MS: i64 = 5 * 60 * 1_000;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreLanEndpoint {
    pub host: String,
    pub port: u16,
}

#[uniffi::export]
pub fn core_format_lan_endpoint(endpoint: CoreLanEndpoint) -> String {
    if endpoint.host.contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    }
}

#[uniffi::export]
pub fn core_parse_lan_endpoint(text: String, default_port: u16) -> Option<CoreLanEndpoint> {
    let value = text.trim();
    if value.is_empty() {
        return None;
    }
    let (host, port_text) = if let Some(after_open) = value.strip_prefix('[') {
        let closing = after_open.find(']')?;
        if closing == 0 {
            return None;
        }
        let host = &after_open[..closing];
        let suffix = &after_open[closing + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').filter(|value| !value.is_empty())?)
        };
        (host, port)
    } else if value.matches(':').count() == 1 {
        let (host, port) = value.split_once(':')?;
        (host, Some(port))
    } else {
        // An unbracketed IPv6 address uses the default port.
        (value, None)
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    let port = match port_text {
        Some(value) => value.parse::<u16>().ok().filter(|port| *port > 0)?,
        None if default_port > 0 => default_port,
        None => return None,
    };
    Some(CoreLanEndpoint {
        host: host.to_string(),
        port,
    })
}

#[uniffi::export]
pub fn core_make_lan_endpoint_link(endpoint: CoreLanEndpoint) -> String {
    let host = BASE64URL_NOPAD.encode(endpoint.host.as_bytes());
    format!("{LAN_LINK_BASE}{LAN_LINK_PREFIX}{host}:{}", endpoint.port)
}

#[uniffi::export]
pub fn core_parse_lan_endpoint_link(fragment: Option<String>) -> Option<CoreLanEndpoint> {
    let payload = fragment?.strip_prefix(LAN_LINK_PREFIX)?.to_string();
    let (encoded_host, port) = payload.rsplit_once(':')?;
    let host = String::from_utf8(BASE64URL_NOPAD.decode(encoded_host.as_bytes()).ok()?).ok()?;
    let endpoint = core_parse_lan_endpoint(format!("[{host}]:{port}"), 1)?;
    Some(endpoint)
}

#[uniffi::export]
pub fn core_lan_network_id_for_ipv4(address: String) -> Option<String> {
    let octets = parse_ipv4(&address)?;
    let prefix = format!("{}.{}.{}.0/24", octets[0], octets[1], octets[2]);
    core_lan_network_id_for_components(vec![format!("ipv4:{prefix}")])
}

/// Whether `candidate_host` sits on the same local network as `local_host` --
/// the same IPv4 /24 the network-id fingerprint is built from, or the same
/// routable IPv6 /64.
///
/// This decides whether a hinted address may be *filed* under this phone's
/// current network id, not whether it may be dialed. Dialing a hint across
/// subnets is deliberate (a routed LAN can carry TCP where mDNS cannot), but
/// that one bounded attempt must not leave a seven-day cache entry claiming a
/// foreign-subnet host belongs to the network we are on: a cached entry is
/// re-dialed on every Wi-Fi join, so one stale hint otherwise becomes an
/// endless probe of an address that can never answer here.
///
/// Both hosts must be address literals of the same family. A name, a mixed
/// pair, or any unparseable string answers `false` -- "same network" is only
/// claimed when it can be shown. IPv6 link-local addresses answer `false` as
/// well: every link is `fe80::/64`, so a match there is no evidence at all,
/// which is exactly the mistake this function exists to prevent. A global or
/// unique-local /64 is a real fingerprint and is honoured, so an IPv6-only
/// Wi-Fi network is not silently excluded from the cache.
///
/// Nothing here discovers or forwards an address; it compares two the caller
/// already holds.
#[uniffi::export]
pub fn lan_hosts_share_local_network(local_host: String, candidate_host: String) -> bool {
    if let Some(local) = core_lan_network_id_for_ipv4(local_host.clone()) {
        return core_lan_network_id_for_ipv4(candidate_host).is_some_and(|it| it == local);
    }
    let Some(local) = routable_ipv6_prefix_64(&local_host) else {
        return false;
    };
    routable_ipv6_prefix_64(&candidate_host).is_some_and(|candidate| candidate == local)
}

/// Whether two LAN host literals identify the same network address.
///
/// This is intentionally stricter than "same network": it is the shared
/// guard both transports use to keep a stale hint or cached endpoint from
/// dialing this phone's own listener after its address changes. Textual IPv6
/// spelling and interface-zone differences do not make an address different,
/// and an IPv4-mapped IPv6 literal compares equal to its IPv4 spelling.
/// Hostnames and malformed values never compare equal.
#[uniffi::export]
pub fn lan_hosts_are_same_address(left_host: String, right_host: String) -> bool {
    normalized_lan_ip(&left_host)
        .is_some_and(|left| normalized_lan_ip(&right_host).is_some_and(|right| right == left))
}

fn normalized_lan_ip(host: &str) -> Option<std::net::IpAddr> {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    let without_zone = unbracketed.split('%').next()?;
    match without_zone.parse::<std::net::IpAddr>().ok()? {
        std::net::IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(std::net::IpAddr::V4)
            .or(Some(std::net::IpAddr::V6(address))),
        address => Some(address),
    }
}

/// The /64 of an IPv6 literal that can fingerprint a network, or `None` when
/// the address cannot (link-local, loopback, unspecified, or unparseable). A
/// zone suffix (`fe80::1%wlan0`) is stripped before parsing -- Android hands
/// those out -- but such an address is link-local and rejected regardless.
fn routable_ipv6_prefix_64(address: &str) -> Option<[u8; 8]> {
    let host = address.split('%').next()?;
    let parsed: std::net::Ipv6Addr = host.parse().ok()?;
    if parsed.is_loopback() || parsed.is_unspecified() {
        return None;
    }
    let octets = parsed.octets();
    // fe80::/10 -- identical on every link, so it proves nothing.
    if octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80 {
        return None;
    }
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&octets[..8]);
    Some(prefix)
}

#[uniffi::export]
pub fn core_lan_network_id_for_components(components: Vec<String>) -> Option<String> {
    if components.is_empty() {
        return None;
    }
    let digest =
        Sha256::digest(format!("CruiseMesh LAN network v1\0{}", components.join("|")).as_bytes());
    Some(BASE64URL.encode(&digest[..16]))
}

#[uniffi::export]
pub fn core_subnet_24_hosts(address: String) -> Vec<String> {
    let Some(octets) = parse_ipv4(&address) else {
        return Vec::new();
    };
    (1..=254)
        .filter(|last| *last != octets[3] as u16)
        .map(|last| format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], last))
        .collect()
}

#[uniffi::export]
pub fn lan_endpoint_cache_is_fresh(saved_at_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(saved_at_ms) <= LAN_ENDPOINT_CACHE_MAX_AGE_MS
}

/// How a cached LAN endpoint came to be known.
///
/// The distinction exists because the two are worth very different amounts.
/// A hint is a claim the contact made about an address; an authenticated
/// entry is an address this phone reached and completed a Noise handshake
/// with. Only the second is evidence, so only the second may sit in the cache
/// on a subnet this phone cannot see itself on -- a routed LAN carries TCP
/// where mDNS cannot, and a peer proven there is legitimately cross-subnet.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanEndpointProvenance {
    /// The address arrived in a contact's endpoint hint and nothing has
    /// confirmed it. Values written before provenance was recorded decode as
    /// this: the conservative reading, since a pre-provenance build filed
    /// hints and proven addresses through the same door.
    Hinted,
    /// The address completed a Noise handshake with this phone.
    Authenticated,
}

/// One entry of the per-network LAN endpoint cache, as both apps hold it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct LanEndpointCacheEntry {
    pub host: String,
    pub port: u16,
    pub saved_at_ms: i64,
    pub provenance: LanEndpointProvenance,
}

/// What a shell should do with an entry it just read off disk.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanEndpointCacheDecision {
    /// Dial it.
    Use,
    /// Do not dial it, but leave it stored -- this load could not judge it.
    Skip,
    /// Delete it. It can never be dialed successfully from here again.
    Evict,
}

const CACHE_FIELD_SEPARATOR: char = '|';
const PROVENANCE_HINTED: &str = "h";
const PROVENANCE_AUTHENTICATED: &str = "a";

/// Serialises a cache entry to the single string both shells persist.
///
/// The shape is `base64url(host)|port|savedAtMs|provenance`, extending the
/// three-field value shipped builds wrote by appending a field rather than
/// changing one, so the three fields those builds wrote keep their meaning and
/// their position. The host is encoded because it can contain the separator
/// (an IPv6 literal) and a zone suffix.
///
/// That only buys forward compatibility because [`lan_endpoint_cache_decode`]
/// ignores fields it does not know: shipped builds did **not** -- their
/// three-field parsers rejected anything longer, and the shells delete a value
/// they cannot parse. So a phone that rolls back past this change loses its
/// cache; a phone that rolls back past a *later* appended field does not.
#[uniffi::export]
pub fn lan_endpoint_cache_encode(entry: LanEndpointCacheEntry) -> String {
    let provenance = match entry.provenance {
        LanEndpointProvenance::Hinted => PROVENANCE_HINTED,
        LanEndpointProvenance::Authenticated => PROVENANCE_AUTHENTICATED,
    };
    format!(
        "{}|{}|{}|{}",
        BASE64URL.encode(entry.host.as_bytes()),
        entry.port,
        entry.saved_at_ms,
        provenance
    )
}

/// Parses a stored cache value, or `None` when it cannot be trusted at all.
///
/// A legacy three-field value parses as [`LanEndpointProvenance::Hinted`].
/// That is the conservative reading and the whole point of the migration: the
/// builds that wrote those values filed cross-subnet hints, so treating them
/// as proven would preserve exactly the entries this is meant to clear.
/// An unrecognised provenance field is read as `Hinted` for the same reason.
///
/// Fields past the fourth are ignored rather than rejected. Both shells delete
/// a value this returns `None` for, so being strict about length would mean
/// that appending a fifth field later silently wipes the whole cache -- proven
/// cross-subnet entries included -- on any phone that rolls back to this build.
/// Ignoring the tail costs nothing and makes the next append survivable.
#[uniffi::export]
pub fn lan_endpoint_cache_decode(value: String) -> Option<LanEndpointCacheEntry> {
    let parts: Vec<&str> = value.split(CACHE_FIELD_SEPARATOR).collect();
    if parts.len() < 3 {
        return None;
    }
    let host = decode_cached_host(parts[0])?;
    if host.is_empty() {
        return None;
    }
    let port = parts[1].parse::<u16>().ok().filter(|port| *port > 0)?;
    let saved_at_ms = parts[2].parse::<i64>().ok()?;
    let provenance = match parts.get(3) {
        Some(&PROVENANCE_AUTHENTICATED) => LanEndpointProvenance::Authenticated,
        _ => LanEndpointProvenance::Hinted,
    };
    Some(LanEndpointCacheEntry {
        host,
        port,
        saved_at_ms,
        provenance,
    })
}

/// The value to store for `entry`, given whatever is already stored under the
/// same key (`None` when nothing is).
///
/// This exists so a save never silently *demotes* a proven address. A contact
/// keeps resending its endpoint hint, and if that hint names the address this
/// phone already authenticated, rewriting the entry as merely hinted would
/// hand it back to the eviction rule below and drop a working cross-subnet
/// peer on the next Wi-Fi join. A hint about an already-proven address
/// refreshes its clock and keeps the proof; anything else -- a different
/// address, or a fresh handshake -- writes what the caller passed.
#[uniffi::export]
pub fn lan_endpoint_cache_encode_update(
    existing_value: Option<String>,
    entry: LanEndpointCacheEntry,
) -> String {
    let mut merged = entry;
    if merged.provenance == LanEndpointProvenance::Hinted {
        let proven = existing_value
            .and_then(lan_endpoint_cache_decode)
            .filter(|stored| {
                stored.provenance == LanEndpointProvenance::Authenticated
                    && stored.host == merged.host
                    && stored.port == merged.port
            });
        if proven.is_some() {
            merged.provenance = LanEndpointProvenance::Authenticated;
        }
    }
    lan_endpoint_cache_encode(merged)
}

/// What to do with a cache entry read back on this phone's current network.
///
/// `local_host` is this phone's own LAN address, or `None` when it has none to
/// compare with.
///
/// Shipped builds filed a hinted address under this phone's network id
/// whatever subnet the address was on, so a phone that ever received such a
/// hint burns one connect timeout per Wi-Fi join for the seven days the entry
/// lives. Freshness and the host rule alone could not clear those: an address
/// that authenticated on a routed LAN is a legitimate cross-subnet entry and
/// looks identical without provenance. With provenance recorded, the rule is
/// finally expressible -- an unproven address must be on the network we are
/// on, a proven one need not be.
///
/// The two "cannot tell" cases are deliberately not the same answer. When
/// *this phone* has no address that can fingerprint a network, the load itself
/// is uninformative, so an unproven entry is skipped and left alone: not
/// dialing is enough to stop the loop, and the next load on a readable
/// interface can still judge it. When the *entry's* host is the unprovable one
/// (a link-local IPv6 address, identical on every link there has ever been) no
/// future load can judge it either -- unprovable is exactly what #271 said may
/// not be remembered -- so that is a terminal answer and the entry goes.
/// An entry equal to this phone's own current address is also terminal and is
/// checked before provenance: an earlier successful handshake at an address
/// does not license dialing it after DHCP assigns that address to this phone.
///
/// Nothing here discovers or forwards an address; every value examined is one
/// this phone already holds.
#[uniffi::export]
pub fn lan_endpoint_cache_decision(
    entry: LanEndpointCacheEntry,
    local_host: Option<String>,
    now_ms: i64,
) -> LanEndpointCacheDecision {
    if !lan_endpoint_cache_is_fresh(entry.saved_at_ms, now_ms) {
        return LanEndpointCacheDecision::Evict;
    }
    if !lan_endpoint_host_is_local(entry.host.clone()) {
        return LanEndpointCacheDecision::Evict;
    }
    if local_host
        .as_ref()
        .is_some_and(|host| lan_hosts_are_same_address(host.clone(), entry.host.clone()))
    {
        return LanEndpointCacheDecision::Evict;
    }
    if entry.provenance == LanEndpointProvenance::Authenticated {
        return LanEndpointCacheDecision::Use;
    }
    let Some(local_host) = local_host.filter(|host| host_can_fingerprint_network(host)) else {
        return LanEndpointCacheDecision::Skip;
    };
    if lan_hosts_share_local_network(local_host, entry.host) {
        LanEndpointCacheDecision::Use
    } else {
        LanEndpointCacheDecision::Evict
    }
}

/// Whether an address is specific enough to say which network it is on -- the
/// precondition for [`lan_hosts_share_local_network`] to mean anything. A name
/// or a link-local IPv6 address is not.
fn host_can_fingerprint_network(host: &str) -> bool {
    core_lan_network_id_for_ipv4(host.to_string()).is_some()
        || routable_ipv6_prefix_64(host).is_some()
}

/// Accepts both the padded URL-safe base64 shipped Android builds wrote and
/// the unpadded form, so no stored value becomes unreadable.
fn decode_cached_host(encoded: &str) -> Option<String> {
    let bytes = BASE64URL
        .decode(encoded.as_bytes())
        .or_else(|_| BASE64URL_NOPAD.decode(encoded.as_bytes()))
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// Whether a host may be dialed as a contact's LAN endpoint: an address
/// literal on the local network, never a name (see
/// [`crate::protocol`]'s `is_local_lan_host`, which this delegates to).
///
/// Exported for the endpoint cache in both apps. Cached entries were written
/// before this rule existed and are never re-checked on the way out, so a
/// seven-day-old entry could otherwise keep a host alive that a hint may no
/// longer carry.
#[uniffi::export]
pub fn lan_endpoint_host_is_local(host: String) -> bool {
    crate::protocol::is_local_lan_host(&host)
}

/// Whether a host is worth *publishing as this phone's own address*, which is
/// stricter than [`lan_endpoint_host_is_local`].
///
/// An IPv6 link-local address is a local address, and both shells will happily
/// pick one up out of the platform's link properties when the Wi-Fi join has
/// not produced an IPv4 address yet. Advertising it is still wrong: `fe80::/10`
/// is only reachable with the *dialer's* scope id, and the scope id a phone
/// reads off its own interface means nothing on the phone that receives it. The
/// address goes into mDNS and into endpoint hints all the same, where it
/// becomes a target that can never answer -- observed in the field as a phone
/// retrying one dead `fe80::…` address for half an hour while the peer it was
/// looking for sat on the same Wi-Fi.
///
/// So: a phone advertises an address a stranger to its interface list can dial,
/// or it advertises nothing and lets discovery do the work.
#[uniffi::export]
pub fn core_lan_host_is_reachable_endpoint(host: String) -> bool {
    crate::protocol::is_reachable_lan_host(&host)
}

/// Whether the LAN transport's periodic check may claim a sweep from its scan
/// planner.
///
/// The shells owned a copy of this each and it drifted into the multi-device
/// bug it now names: a sweep was worthwhile "while the transport has no links
/// at all", and both shells counted *every* live connection, including a link
/// to one of this person's own devices. Such a link is not a friend on this
/// Wi-Fi -- it carries no contact's mail, wins no route, and (having no route)
/// sat outside the LAN heartbeat entirely -- so one that had quietly died still
/// read as company and shut discovery off for the whole Wi-Fi join. The phone
/// that had just removed another device spent 26 minutes in exactly that state.
///
/// `peer_links` is therefore links to *contacts* only. The two escapes are the
/// people this phone still owes a search:
///
/// - `unlinked_capable_contacts`: a contact that has recently demonstrated LAN
///   support and has no authenticated LAN link (see
///   [`crate::lan_capability_motivates_scan`]'s shell mirrors) -- one connected
///   family member must not stop discovery of the rest.
/// - `own_device_search_live`: this phone is inside a bounded window during
///   which it is looking for one of this person's own devices
///   ([`core_lan_own_device_search_since`]). It is what makes a phone go
///   looking for its sibling at all, and mDNS was the only channel that ever
///   did.
///
/// Note that the second escape is a *bounded* one and the first is a *decayed*
/// one, and that neither is a bare count of who is missing. Both motives are
/// satisfied by an absence, and an absence never ends by itself: a contact who
/// went ashore, or a tablet left at home, would otherwise keep this phone
/// sweeping the /24 every five minutes on every Wi-Fi it ever joins. The
/// contact side decays (its shell mirrors only count a contact whose LAN
/// evidence is recent); the own-device side is time-boxed per reason to search.
///
/// In-flight work (pending outbound attempts, a running sweep) always defers.
/// The counts are unsigned on purpose: a shell that ever computes one of them
/// negative must clamp it to zero on the way in, which slows discovery down
/// rather than disabling it.
#[uniffi::export]
pub fn core_lan_scan_gate_open(
    peer_links: u32,
    unlinked_capable_contacts: u32,
    own_device_search_live: bool,
    pending_outbound_attempts: u32,
    scan_remaining: u32,
) -> bool {
    (peer_links == 0 || unlinked_capable_contacts > 0 || own_device_search_live)
        && pending_outbound_attempts == 0
        && scan_remaining == 0
}

/// How long a phone goes on sweeping the subnet for one of this person's own
/// devices once it has a reason to.
///
/// Long enough for several cheap `/24` sweeps and the expensive tier they arm
/// (the planner's local cadence is five minutes), short enough that it is a
/// search rather than a standing condition.
pub const LAN_OWN_DEVICE_SEARCH_WINDOW_MS: i64 = 15 * 60_000;

/// [`LAN_OWN_DEVICE_SEARCH_WINDOW_MS`], for the shells.
#[uniffi::export]
pub fn core_lan_own_device_search_window_ms() -> i64 {
    LAN_OWN_DEVICE_SEARCH_WINDOW_MS
}

/// When this phone's search for one of its own devices started, or `None` when
/// it is not searching. Feeds [`core_lan_scan_gate_open`]'s
/// `own_device_search_live`.
///
/// **Why this is a window and not "is a sibling missing".** The obvious rule --
/// sweep while the roster lists a device this phone has no link to -- never
/// stops being true. A second device that is switched off, left at home, or
/// simply out of the house is missing forever, so the gate would stand open
/// forever and the planner would hand out a `/24` sweep on its flat five-minute
/// cadence for the whole life of every Wi-Fi join, on battery, for exactly the
/// multi-device households this mechanism was added for. It is also not even
/// satisfiable for a person with three devices, because the transport keeps at
/// most one own-device link at a time. So the motive is bounded, in the same
/// spirit as the contact side's recency decay: a *reason* to search opens a
/// window, and the window closes.
///
/// The reasons, all of which mean "something about this person's fleet just
/// changed, and a link may be findable that was not before":
///
/// - `unlinked_own_devices` rose above `previous_unlinked_own_devices` -- a
///   sibling appeared on the roster, or an own-device link dropped. (The shells
///   also reset both across a network change, so joining a Wi-Fi re-arms.)
/// - `roster_changed` -- this person's device roster is not the one last
///   observed. This is the arm that matters on the phone that performed a
///   *removal*: the removed device leaves its roster immediately, so its
///   shortfall is zero and it would otherwise have no motive at all to go
///   looking for the phone it must still hand §10 step 5's notice to.
///
/// A backwards clock jump re-arms rather than expiring: the failure worth
/// avoiding is a phone that stops looking.
#[uniffi::export]
pub fn core_lan_own_device_search_since(
    previous_since_ms: Option<i64>,
    roster_changed: bool,
    unlinked_own_devices: u32,
    previous_unlinked_own_devices: u32,
    now_ms: i64,
) -> Option<i64> {
    if roster_changed || unlinked_own_devices > previous_unlinked_own_devices {
        return Some(now_ms);
    }
    let since = previous_since_ms?;
    if now_ms < since {
        return Some(now_ms);
    }
    if now_ms.saturating_sub(since) >= LAN_OWN_DEVICE_SEARCH_WINDOW_MS {
        return None;
    }
    Some(since)
}

/// How many consecutive failures a LAN reconnect target that has never once
/// completed a Noise handshake survives before it is dropped.
///
/// Matched to `CoreReconnectBackoffTracker`'s own failure budget: by the time
/// the backoff has given up on an address, the address has had every chance
/// this phone can give it.
pub const LAN_RECONNECT_UNPROVEN_FAILURE_CEILING: u32 = 6;

/// Whether a LAN reconnect target should be forgotten rather than retried
/// again.
///
/// A target created from this phone's own discovery (an mDNS resolution, a
/// subnet-sweep hit) is kept across failures on purpose: the address is one
/// this phone observed, and a peer that went to sleep comes back at it. What
/// shipped had no ceiling on that at all, and the backoff underneath it decays
/// to a permanent slow probe rather than a refusal, so a single stale mDNS
/// record became a dial at a dead address every sixty seconds for as long as
/// the phone stayed on the Wi-Fi -- the exact loop the field capture shows the
/// approving phone stuck in, in place of the search that would have found the
/// device it had removed.
///
/// An address that has *proved* itself (`ever_authenticated`) keeps its target
/// regardless: that is an ordinary contact link waiting out a sleeping peer,
/// and nothing here may make one harder to re-establish. Only the unproven kind
/// is retired, and retiring it loses nothing -- a fresh discovery or a sweep
/// re-creates the target the moment there is anything real to reach.
///
/// A link to one of this person's *own* devices proves its address the same
/// way, which is deliberate and worth naming: a removed device still presents
/// the agreement key that admits it (§10.1 rotates the inbox key, not the LAN
/// Noise static), so once it has handshaked, the honest phone keeps dialing it
/// for the rest of the network join. That is the point -- §10 step 5's notice
/// is what the fleet is trying to hand it -- but the same standing dial is a
/// presence signal that outlives the revocation, and it is accepted knowingly.
#[uniffi::export]
pub fn core_lan_reconnect_target_is_exhausted(
    ever_authenticated: bool,
    consecutive_failures: u32,
) -> bool {
    !ever_authenticated && consecutive_failures >= LAN_RECONNECT_UNPROVEN_FAILURE_CEILING
}

#[uniffi::export]
pub fn should_resend_lan_endpoint(
    previous_signature: Option<String>,
    previous_sent_at_ms: Option<i64>,
    current_signature: String,
    now_ms: i64,
) -> bool {
    previous_signature.as_deref() != Some(current_signature.as_str())
        || previous_sent_at_ms
            .map(|sent| now_ms.saturating_sub(sent) >= LAN_ENDPOINT_RESEND_INTERVAL_MS)
            .unwrap_or(true)
}

fn parse_ipv4(address: &str) -> Option<[u8; 4]> {
    let parts: Vec<_> = address.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    Some([
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
        parts[3].parse().ok()?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_ipv4_and_ipv6() {
        assert_eq!(
            core_parse_lan_endpoint("192.168.1.7:9999".into(), 45892),
            Some(CoreLanEndpoint {
                host: "192.168.1.7".into(),
                port: 9999
            })
        );
        assert_eq!(
            core_parse_lan_endpoint("[fe80::1]:123".into(), 45892),
            Some(CoreLanEndpoint {
                host: "fe80::1".into(),
                port: 123
            })
        );
        assert_eq!(
            core_parse_lan_endpoint("host.local".into(), 45892)
                .unwrap()
                .port,
            45892
        );
        assert!(core_parse_lan_endpoint("host:0".into(), 45892).is_none());
    }

    #[test]
    fn endpoint_link_round_trips() {
        let endpoint = CoreLanEndpoint {
            host: "fe80::cafe".into(),
            port: 45_892,
        };
        let link = core_make_lan_endpoint_link(endpoint.clone());
        let fragment = link.split_once('#').unwrap().1.to_string();
        assert_eq!(core_parse_lan_endpoint_link(Some(fragment)), Some(endpoint));
    }

    #[test]
    fn subnet_excludes_self_and_network_edges() {
        let hosts = core_subnet_24_hosts("10.2.3.9".into());
        assert_eq!(hosts.len(), 253);
        assert!(!hosts.contains(&"10.2.3.9".to_string()));
        assert_eq!(hosts.first().unwrap(), "10.2.3.1");
        assert_eq!(hosts.last().unwrap(), "10.2.3.254");
    }

    #[test]
    fn network_id_is_stable_within_subnet() {
        assert_eq!(
            core_lan_network_id_for_ipv4("192.168.8.1".into()),
            core_lan_network_id_for_ipv4("192.168.8.200".into())
        );
        assert_ne!(
            core_lan_network_id_for_ipv4("192.168.8.1".into()),
            core_lan_network_id_for_ipv4("192.168.9.1".into())
        );
        assert_eq!(
            core_lan_network_id_for_ipv4("10.154.189.58".into()).as_deref(),
            Some("NcJ68sf-sL-VO63PUTnngg==")
        );
    }

    #[test]
    fn exported_host_check_matches_the_hint_rule() {
        // The apps re-check cached endpoints with this; it must agree with
        // what a hint itself is allowed to carry.
        for host in ["10.0.0.7", "192.168.1.7", "169.254.3.4", "fe80::1%wlan0"] {
            assert!(lan_endpoint_host_is_local(host.into()), "{host}");
        }
        for host in [
            "phone.local",
            "cruisemesh.app",
            "8.8.8.8",
            "2606:4700::1",
            "",
        ] {
            assert!(!lan_endpoint_host_is_local(host.into()), "{host}");
        }
    }

    #[test]
    fn same_network_check_only_says_yes_for_a_shared_ipv4_24() {
        assert!(lan_hosts_share_local_network(
            "192.168.86.31".into(),
            "192.168.86.23".into()
        ));
        assert!(lan_hosts_share_local_network(
            "10.80.209.1".into(),
            "10.80.209.68".into()
        ));
        // The field case: a hint from a foreign subnet must never be filed
        // under the network this phone is actually on.
        assert!(!lan_hosts_share_local_network(
            "192.168.86.31".into(),
            "10.80.209.68".into()
        ));
        // Neighbouring /24s are different networks even inside one prefix.
        assert!(!lan_hosts_share_local_network(
            "192.168.86.31".into(),
            "192.168.87.23".into()
        ));
        // Same host is trivially on its own network.
        assert!(lan_hosts_share_local_network(
            "192.168.86.31".into(),
            "192.168.86.31".into()
        ));
    }

    #[test]
    fn same_network_check_honours_a_routable_ipv6_64() {
        // An IPv6-only Wi-Fi network still has a real fingerprint, so hints
        // on it are cacheable rather than silently dropped.
        assert!(lan_hosts_share_local_network(
            "2001:db8:1:2::31".into(),
            "2001:db8:1:2::23".into()
        ));
        assert!(lan_hosts_share_local_network(
            "fd12:3456:789a:1::1".into(),
            "fd12:3456:789a:1::99".into()
        ));
        // A different /64 is a different network.
        assert!(!lan_hosts_share_local_network(
            "2001:db8:1:2::31".into(),
            "2001:db8:1:3::23".into()
        ));
        // Link-local is fe80::/64 on every link in the world: a match there
        // is no evidence, so it never authorises a cache entry -- with or
        // without a zone suffix.
        assert!(!lan_hosts_share_local_network(
            "fe80::1".into(),
            "fe80::2".into()
        ));
        assert!(!lan_hosts_share_local_network(
            "fe80::1%wlan0".into(),
            "fe80::2%wlan0".into()
        ));
        // Families never mix.
        assert!(!lan_hosts_share_local_network(
            "192.168.86.31".into(),
            "2001:db8:1:2::23".into()
        ));
        assert!(!lan_hosts_share_local_network(
            "2001:db8:1:2::31".into(),
            "192.168.86.23".into()
        ));
    }

    #[test]
    fn same_network_check_rejects_anything_it_cannot_parse() {
        for (local, candidate) in [
            ("192.168.86.31", "phone.local"),
            ("192.168.86.31", "fe80::1"),
            ("192.168.86.31", "[fe80::1]"),
            ("192.168.86.31", ""),
            ("192.168.86.31", "192.168.86"),
            ("192.168.86.31", "192.168.86.999"),
            ("192.168.86.31", "192.168.86.23:45892"),
            ("fe80::1", "fe80::1"),
            ("::1", "::1"),
            ("2001:db8:1:2::31", "not:an:address"),
            ("", "192.168.86.23"),
            ("", ""),
            ("router", "192.168.86.23"),
        ] {
            assert!(
                !lan_hosts_share_local_network(local.into(), candidate.into()),
                "{local} vs {candidate}"
            );
        }
    }

    #[test]
    fn address_equality_normalizes_ip_literals_without_trusting_names() {
        for (left, right) in [
            ("192.168.86.20", "192.168.86.20"),
            ("2001:db8::1", "2001:0db8:0:0:0:0:0:1"),
            ("fe80::1%en0", "[fe80:0:0:0:0:0:0:1%wlan0]"),
            ("192.168.86.20", "::ffff:192.168.86.20"),
        ] {
            assert!(
                lan_hosts_are_same_address(left.into(), right.into()),
                "{left} vs {right}"
            );
        }
        for (left, right) in [
            ("192.168.86.20", "192.168.86.21"),
            ("phone.local", "phone.local"),
            ("", ""),
            ("192.168.86.20:45892", "192.168.86.20"),
        ] {
            assert!(
                !lan_hosts_are_same_address(left.into(), right.into()),
                "{left} vs {right}"
            );
        }
    }

    fn entry(host: &str, provenance: LanEndpointProvenance) -> LanEndpointCacheEntry {
        LanEndpointCacheEntry {
            host: host.into(),
            port: 45_892,
            saved_at_ms: 1_000,
            provenance,
        }
    }

    /// The exact bytes a pre-provenance build wrote: URL-safe padded base64 of
    /// the host, then port and timestamp, with no fourth field.
    fn legacy_value(host: &str, port: u16, saved_at_ms: i64) -> String {
        format!("{}|{port}|{saved_at_ms}", BASE64URL.encode(host.as_bytes()))
    }

    #[test]
    fn cache_entries_round_trip_through_the_stored_string() {
        for provenance in [
            LanEndpointProvenance::Hinted,
            LanEndpointProvenance::Authenticated,
        ] {
            for host in ["192.168.86.23", "fe80::1%wlan0", "10.0.0.7"] {
                let original = entry(host, provenance);
                let encoded = lan_endpoint_cache_encode(original.clone());
                assert_eq!(lan_endpoint_cache_decode(encoded), Some(original), "{host}");
            }
        }
    }

    #[test]
    fn legacy_three_field_values_decode_as_hinted() {
        // The migration hinge: a value written before provenance existed came
        // from a build that filed cross-subnet hints, so it must read as
        // unproven -- reading it as proven would preserve the very entries
        // this change exists to clear.
        assert_eq!(
            lan_endpoint_cache_decode(legacy_value("10.80.209.68", 45_892, 1_000)),
            Some(LanEndpointCacheEntry {
                host: "10.80.209.68".into(),
                port: 45_892,
                saved_at_ms: 1_000,
                provenance: LanEndpointProvenance::Hinted,
            })
        );
        // Unpadded base64 reads too, and so does an unrecognised provenance.
        assert_eq!(
            lan_endpoint_cache_decode(format!(
                "{}|45892|1000|z",
                BASE64URL_NOPAD.encode("10.0.0.7".as_bytes())
            ))
            .map(|it| it.provenance),
            Some(LanEndpointProvenance::Hinted)
        );
    }

    #[test]
    fn fields_past_the_fourth_are_ignored_rather_than_rejected() {
        // Room for the next append. Both shells delete a value they cannot
        // parse, so rejecting an unknown tail would mean that adding a fifth
        // field later wipes the entire cache -- proven cross-subnet entries
        // included -- on any phone that rolls back to a build older than the
        // append. Ignoring it costs nothing.
        assert_eq!(
            lan_endpoint_cache_decode(format!(
                "{}|45892|1000|a|whatever-comes-next",
                BASE64URL.encode(b"10.80.209.68")
            )),
            Some(LanEndpointCacheEntry {
                host: "10.80.209.68".into(),
                port: 45_892,
                saved_at_ms: 1_000,
                provenance: LanEndpointProvenance::Authenticated,
            })
        );
    }

    #[test]
    fn undecodable_cache_values_are_rejected() {
        // Each of these fails on a field it must be able to read -- too few
        // fields, an unusable host, port or timestamp. Never on a field it
        // does not recognise: see the test above.
        for value in [
            "",
            "only-one-field",
            "a|b",
            "a|b|c|d|e",
            &format!("{}|0|1000|h", BASE64URL.encode(b"10.0.0.7")),
            &format!("{}|45892|nope|h", BASE64URL.encode(b"10.0.0.7")),
            &format!("{}|45892|1000|h", BASE64URL.encode(b"")),
            "!!!|45892|1000|h",
        ] {
            assert!(
                lan_endpoint_cache_decode(value.to_string()).is_none(),
                "{value}"
            );
        }
    }

    #[test]
    fn a_poisoned_legacy_entry_is_evicted_and_a_proven_one_survives() {
        let now = 2_000;
        let local = Some("192.168.86.31".to_string());
        // The field case: 10.80.209.68 filed by a shipped build while this
        // phone sat on 192.168.86.0/24. It can never answer here.
        let poisoned = lan_endpoint_cache_decode(legacy_value("10.80.209.68", 45_892, 1_000))
            .expect("legacy value parses");
        assert_eq!(
            lan_endpoint_cache_decision(poisoned, local.clone(), now),
            LanEndpointCacheDecision::Evict
        );
        // A peer proven on a routed LAN is legitimately cross-subnet.
        assert_eq!(
            lan_endpoint_cache_decision(
                entry("10.80.209.68", LanEndpointProvenance::Authenticated),
                local.clone(),
                now
            ),
            LanEndpointCacheDecision::Use
        );
        // A legacy entry on this phone's own subnet is still dialable.
        let same_subnet = lan_endpoint_cache_decode(legacy_value("192.168.86.23", 45_892, 1_000))
            .expect("legacy value parses");
        assert_eq!(
            lan_endpoint_cache_decision(same_subnet, local, now),
            LanEndpointCacheDecision::Use
        );
    }

    #[test]
    fn cache_never_dials_this_phones_own_current_address() {
        for provenance in [
            LanEndpointProvenance::Hinted,
            LanEndpointProvenance::Authenticated,
        ] {
            assert_eq!(
                lan_endpoint_cache_decision(
                    entry("192.168.86.20", provenance),
                    Some("192.168.86.20".into()),
                    2_000
                ),
                LanEndpointCacheDecision::Evict
            );
        }

        // Proven cross-subnet peers remain valid: equality, not subnet
        // difference, is the exceptional condition ahead of provenance.
        assert_eq!(
            lan_endpoint_cache_decision(
                entry("10.80.209.68", LanEndpointProvenance::Authenticated),
                Some("192.168.86.20".into()),
                2_000
            ),
            LanEndpointCacheDecision::Use
        );
    }

    #[test]
    fn cache_decision_evicts_only_what_it_can_show_is_unusable() {
        let local = Some("192.168.86.31".to_string());
        // Stale beats everything, proven or not.
        for provenance in [
            LanEndpointProvenance::Hinted,
            LanEndpointProvenance::Authenticated,
        ] {
            assert_eq!(
                lan_endpoint_cache_decision(
                    entry("192.168.86.23", provenance),
                    local.clone(),
                    1_000 + LAN_ENDPOINT_CACHE_MAX_AGE_MS + 1
                ),
                LanEndpointCacheDecision::Evict
            );
            // So does a host a hint may no longer carry.
            assert_eq!(
                lan_endpoint_cache_decision(entry("8.8.8.8", provenance), local.clone(), 2_000),
                LanEndpointCacheDecision::Evict
            );
        }
        // A link-local entry can never be shown to be on this network -- every
        // link is fe80::/64 -- so, unlike the case below, no later load will
        // do better and the entry is retired for good.
        assert_eq!(
            lan_endpoint_cache_decision(
                entry("fe80::1%wlan0", LanEndpointProvenance::Hinted),
                local.clone(),
                2_000
            ),
            LanEndpointCacheDecision::Evict
        );
        // No local address to compare with, or one that fingerprints nothing:
        // skip the dial, but keep the entry -- a later load can still judge it.
        for local_host in [
            None,
            Some("router".to_string()),
            Some("fe80::1".to_string()),
        ] {
            assert_eq!(
                lan_endpoint_cache_decision(
                    entry("10.80.209.68", LanEndpointProvenance::Hinted),
                    local_host.clone(),
                    2_000
                ),
                LanEndpointCacheDecision::Skip,
                "{local_host:?}"
            );
            // A proven entry is dialed regardless: it answered once already.
            assert_eq!(
                lan_endpoint_cache_decision(
                    entry("10.80.209.68", LanEndpointProvenance::Authenticated),
                    local_host,
                    2_000
                ),
                LanEndpointCacheDecision::Use
            );
        }
    }

    #[test]
    fn a_repeated_hint_never_demotes_a_proven_entry() {
        let proven = lan_endpoint_cache_encode(LanEndpointCacheEntry {
            host: "10.80.209.68".into(),
            port: 45_892,
            saved_at_ms: 1_000,
            provenance: LanEndpointProvenance::Authenticated,
        });
        // The same address arriving again as a mere hint refreshes the clock
        // and keeps the proof; losing it would evict a working routed-LAN
        // peer on the next Wi-Fi join.
        let refreshed = lan_endpoint_cache_encode_update(
            Some(proven.clone()),
            LanEndpointCacheEntry {
                host: "10.80.209.68".into(),
                port: 45_892,
                saved_at_ms: 9_000,
                provenance: LanEndpointProvenance::Hinted,
            },
        );
        assert_eq!(
            lan_endpoint_cache_decode(refreshed),
            Some(LanEndpointCacheEntry {
                host: "10.80.209.68".into(),
                port: 45_892,
                saved_at_ms: 9_000,
                provenance: LanEndpointProvenance::Authenticated,
            })
        );
        // A *different* address is new information and is filed as hinted --
        // the old proof said nothing about this one.
        let replaced = lan_endpoint_cache_encode_update(
            Some(proven.clone()),
            entry("192.168.86.23", LanEndpointProvenance::Hinted),
        );
        assert_eq!(
            lan_endpoint_cache_decode(replaced).map(|it| it.provenance),
            Some(LanEndpointProvenance::Hinted)
        );
        // A different port on the same host is a different endpoint too.
        let other_port = lan_endpoint_cache_encode_update(
            Some(proven),
            LanEndpointCacheEntry {
                host: "10.80.209.68".into(),
                port: 45_893,
                saved_at_ms: 9_000,
                provenance: LanEndpointProvenance::Hinted,
            },
        );
        assert_eq!(
            lan_endpoint_cache_decode(other_port).map(|it| it.provenance),
            Some(LanEndpointProvenance::Hinted)
        );
        // Nothing stored, or an unreadable value: the caller's word stands.
        for existing in [None, Some("garbage".to_string())] {
            assert_eq!(
                lan_endpoint_cache_decode(lan_endpoint_cache_encode_update(
                    existing,
                    entry("10.80.209.68", LanEndpointProvenance::Hinted)
                ))
                .map(|it| it.provenance),
                Some(LanEndpointProvenance::Hinted)
            );
        }
        // A handshake promotes a stored hint in place.
        let promoted = lan_endpoint_cache_encode_update(
            Some(legacy_value("10.80.209.68", 45_892, 1_000)),
            entry("10.80.209.68", LanEndpointProvenance::Authenticated),
        );
        assert_eq!(
            lan_endpoint_cache_decode(promoted).map(|it| it.provenance),
            Some(LanEndpointProvenance::Authenticated)
        );
    }

    #[test]
    fn a_link_to_one_of_this_persons_own_devices_never_shuts_the_sweep_off() {
        // The field case: an approver whose only live LAN link was to the phone
        // it had just removed. That link is not a friend on this Wi-Fi, so it
        // is not counted as one, and the sweep that would have re-found the
        // removed phone stays available.
        assert!(core_lan_scan_gate_open(0, 0, false, 0, 0));
        // ... whereas a real contact link with nobody else owed a search does
        // stop it, exactly as before.
        assert!(!core_lan_scan_gate_open(1, 0, false, 0, 0));
    }

    #[test]
    fn a_sibling_this_phone_is_not_linked_to_motivates_a_sweep() {
        assert!(core_lan_scan_gate_open(3, 0, true, 0, 0));
        assert!(!core_lan_scan_gate_open(3, 0, false, 0, 0));
        // Contacts still motivate one on their own.
        assert!(core_lan_scan_gate_open(3, 2, false, 0, 0));
    }

    #[test]
    fn in_flight_work_always_defers_the_sweep() {
        assert!(!core_lan_scan_gate_open(0, 5, true, 1, 0));
        assert!(!core_lan_scan_gate_open(0, 5, true, 0, 12));
    }

    /// The motive that goes looking for a sibling has to stop, exactly as the
    /// contact-side motive decays. A second phone that is switched off or left
    /// at home is missing forever; unbounded, it would sweep the /24 every five
    /// minutes on every Wi-Fi this person ever joins, on battery, for the whole
    /// life of each join.
    #[test]
    fn the_search_for_a_sibling_runs_out() {
        let window = LAN_OWN_DEVICE_SEARCH_WINDOW_MS;
        // A sibling that was not missing a moment ago now is: start searching.
        let armed = core_lan_own_device_search_since(None, false, 1, 0, 1_000);
        assert_eq!(armed, Some(1_000));
        // It stays armed for the window, unchanged, however long the sibling
        // stays missing...
        assert_eq!(
            core_lan_own_device_search_since(armed, false, 1, 1, 1_000 + window - 1),
            Some(1_000)
        );
        // ... and then stops. This is the finding: nothing else ever stopped it.
        assert_eq!(
            core_lan_own_device_search_since(armed, false, 1, 1, 1_000 + window),
            None
        );
        // And having stopped, a still-missing sibling does not restart it.
        assert_eq!(
            core_lan_own_device_search_since(None, false, 1, 1, 1_000 + window * 4),
            None
        );
        // A gate fed the expired window is a gate that lets one contact link
        // shut the sweep off again, which is the whole point of stopping.
        assert!(!core_lan_scan_gate_open(1, 0, false, 0, 0));
    }

    /// A person with three devices can never have every sibling linked -- the
    /// transport keeps one own-device link at a time -- so a bare "is a sibling
    /// missing" motive is not merely long-lived there, it is permanent.
    #[test]
    fn a_three_device_person_does_not_sweep_forever() {
        let window = LAN_OWN_DEVICE_SEARCH_WINDOW_MS;
        let armed = core_lan_own_device_search_since(None, false, 2, 0, 0);
        assert_eq!(armed, Some(0));
        // One of the two is linked; the other never can be. Still bounded.
        assert_eq!(
            core_lan_own_device_search_since(armed, false, 1, 2, window - 1),
            Some(0)
        );
        assert_eq!(
            core_lan_own_device_search_since(armed, false, 1, 1, window),
            None
        );
    }

    /// The phone that performed a removal drops the removed device from its own
    /// roster at once, so its shortfall is zero -- it would have no motive to go
    /// looking for the device it must still hand the notice to. The roster
    /// change itself is the motive.
    #[test]
    fn a_removal_sends_the_approving_phone_looking() {
        let armed = core_lan_own_device_search_since(None, true, 0, 0, 5_000);
        assert_eq!(armed, Some(5_000));
        assert!(core_lan_scan_gate_open(1, 0, armed.is_some(), 0, 0));
        // Bounded like every other reason to search.
        assert_eq!(
            core_lan_own_device_search_since(
                armed,
                false,
                0,
                0,
                5_000 + LAN_OWN_DEVICE_SEARCH_WINDOW_MS
            ),
            None
        );
    }

    #[test]
    fn a_backwards_clock_restarts_the_search_rather_than_ending_it() {
        assert_eq!(
            core_lan_own_device_search_since(Some(10_000), false, 1, 1, 4),
            Some(4)
        );
    }

    #[test]
    fn only_an_address_another_phone_can_dial_is_advertisable() {
        assert!(core_lan_host_is_reachable_endpoint("192.168.86.20".into()));
        assert!(core_lan_host_is_reachable_endpoint("fd00::1".into()));
        // The field's dead target: link-local, with and without the scope id
        // that makes it dialable only on the phone that read it.
        assert!(!core_lan_host_is_reachable_endpoint("fe80::c88e:1".into()));
        assert!(!core_lan_host_is_reachable_endpoint(
            "fe80::c88e:1%wlan0".into()
        ));
        // Still a *local* host, so an arriving hint carrying one is not refused
        // outright -- the two rules are deliberately different.
        assert!(lan_endpoint_host_is_local("fe80::c88e:1%wlan0".into()));
        assert!(!core_lan_host_is_reachable_endpoint("93.184.216.34".into()));
        assert!(!core_lan_host_is_reachable_endpoint("peer.local".into()));
    }

    #[test]
    fn an_address_that_never_answered_stops_being_retried() {
        // The field's loop: an mDNS-derived target at a dead link-local
        // address, retried forever because nothing ever retired it.
        assert!(!core_lan_reconnect_target_is_exhausted(false, 0));
        assert!(!core_lan_reconnect_target_is_exhausted(
            false,
            LAN_RECONNECT_UNPROVEN_FAILURE_CEILING - 1
        ));
        assert!(core_lan_reconnect_target_is_exhausted(
            false,
            LAN_RECONNECT_UNPROVEN_FAILURE_CEILING
        ));
        // An address a contact link once authenticated at is never retired by
        // failure count -- ordinary LAN delivery must survive a sleeping peer.
        assert!(!core_lan_reconnect_target_is_exhausted(
            true,
            LAN_RECONNECT_UNPROVEN_FAILURE_CEILING * 10
        ));
    }

    #[test]
    fn cache_and_resend_policies_handle_time() {
        assert!(lan_endpoint_cache_is_fresh(1_000, 2_000));
        assert!(!lan_endpoint_cache_is_fresh(
            0,
            LAN_ENDPOINT_CACHE_MAX_AGE_MS + 1
        ));
        assert!(!should_resend_lan_endpoint(
            Some("same".into()),
            Some(1_000),
            "same".into(),
            2_000
        ));
        assert!(should_resend_lan_endpoint(
            Some("old".into()),
            Some(1_000),
            "new".into(),
            2_000
        ));
    }
}
