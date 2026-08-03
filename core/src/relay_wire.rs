use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};

use crate::CoreError;

const RELAY_MAX_SEALED_BYTES: usize = 512 * 1024;

/// Rows per fetch page.
///
/// This used to be 16, sized against the theoretical worst case of a page
/// entirely made of maximum-size sealed rows (16 × 512 KiB = 8 MiB). That
/// bound was real but it was not the *binding* one — the response body cap
/// below already refuses anything over 12 MiB before a single byte is
/// decoded — and it made pagination the dominant cost of every sync pass.
///
/// A relay mailbox legitimately accumulates rows that are never acked (a
/// proxy-fetched `CARRIED` copy stays as the durable fallback; a legacy
/// group-hint row is never acked at all), relayd returns rows in ascending
/// id order, and a *fresh* message therefore has the highest id and is
/// fetched last. At 16 rows a page, a mailbox observed in the field at ~29k
/// rows needed on the order of 1,800 sequential HTTP round trips before the
/// newest message was even looked at, and passes routinely timed out before
/// finishing. Typical sealed rows are around a kilobyte, not half a megabyte,
/// so 16 was buying a memory bound that the body cap already guaranteed and
/// paying for it in minutes of delivery latency.
///
/// 256 keeps every existing safety property (see
/// [`RELAY_FETCH_MAX_DECODED_BYTES`]) and cuts the page count by 16×.
/// relayd's own ceiling is 500, so the deployed server accepts this without
/// any change; a server that clamps lower is handled by the client's
/// termination rule (see [`crate::relay_fetch_walk_continues`]), which ends a
/// walk on an *empty* page rather than on a short one.
const RELAY_FETCH_MAX_ROWS: usize = 256;

/// Ceiling on the decoded (post-base64) sealed bytes one fetch page may
/// materialize.
///
/// Pinned to the response body cap rather than to `rows × max row size`,
/// because with 256 rows the latter is no longer a bound at all (256 × 512
/// KiB = 128 MiB, an order of magnitude above anything the transport would
/// hand us). The body cap *is* the real bound and it is the tighter one:
/// base64url expands by 4/3, so a body that passes
/// [`RELAY_MAX_RESPONSE_BODY_BYTES`] can carry at most ~9 MiB of decoded
/// payload no matter how the rows are arranged. Keeping the explicit running
/// check is defense in depth — [`relay_decode_fetch_page`] enforces the body
/// cap itself, at its own boundary, so a caller outside the first-party
/// shells cannot skip it, and this second check then holds even if that ever
/// changed.
const RELAY_FETCH_MAX_DECODED_BYTES: usize = RELAY_MAX_RESPONSE_BODY_BYTES;

const RELAY_MAX_RESPONSE_BODY_BYTES: usize = 12 * 1024 * 1024;
const RELAY_MAX_PRESENCE_ROWS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayFetchedEnvelope {
    pub id: i64,
    pub msg_id: Vec<u8>,
    pub hop_ttl: u8,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
    pub expiry_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayFetchPage {
    pub envelopes: Vec<CoreRelayFetchedEnvelope>,
    pub next_cursor: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayPresence {
    pub hint: Vec<u8>,
    pub last_seen_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayPresencePage {
    pub now_ms: i64,
    pub presence: Vec<CoreRelayPresence>,
}

#[derive(Serialize, Deserialize)]
struct EnvelopeWire {
    id: i64,
    msg_id: String,
    hop_ttl: u8,
    recipient_hint: String,
    sealed: String,
    expiry_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct PostEnvelopeWire {
    msg_id: String,
    hop_ttl: u8,
    recipient_hint: String,
    sealed: String,
    expiry_ms: i64,
}

#[derive(Deserialize)]
struct PostResponse {
    id: i64,
}

#[derive(Deserialize)]
struct FetchResponse {
    envelopes: Vec<EnvelopeWire>,
    next_cursor: i64,
}

#[derive(Serialize)]
struct AckRequest {
    ids: Vec<i64>,
}

#[derive(Serialize)]
struct PresenceRequest {
    announce: Vec<String>,
    query: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct PresenceWire {
    hint: String,
    last_seen_ms: i64,
}

#[derive(Deserialize)]
struct PresenceResponse {
    now_ms: i64,
    presence: Vec<PresenceWire>,
}

/// CP4 (deposit-token split): class prefix that marks a *deposit* relay
/// token — post-only into one family's mailbox, minted by attenuating the
/// family's full member token. The prefix makes the class recognizable from
/// the token string alone, so routing policy (below) and the relay can both
/// classify a credential without any extra field on the wire.
const RELAY_DEPOSIT_TOKEN_PREFIX: &str = "cmdep1-";

/// Domain-separation context for the deposit derivation. relayd derives the
/// same value at provisioning (`relayd/src/lib.rs::deposit_token_for`);
/// golden vectors in both crates pin the two implementations together.
const RELAY_DEPOSIT_TOKEN_CONTEXT: &[u8] = b"cruisemesh relay deposit token v1";

/// True when `token` is a deposit-class relay credential (CP4): valid only
/// for posting envelopes into its family's mailbox, never for fetch/ack/
/// presence/WebSocket. Friend cards carry this class; the Cruise Pass setup
/// card carries the full member class.
#[uniffi::export]
pub fn relay_token_is_deposit(token: String) -> bool {
    token.trim().starts_with(RELAY_DEPOSIT_TOKEN_PREFIX)
}

/// CP4: attenuate a member relay token into its deposit-class counterpart —
/// `cmdep1-` ‖ base64url(BLAKE2b-256(context ‖ member_token)).
///
/// Derivation (a one-way hash), not random minting, on purpose: the phone
/// can stamp a deposit token onto a friend card entirely offline, knowing
/// only its own member token, with no new relay endpoint, no extra stored
/// credential, and no change to the Cruise Pass setup card. The relay
/// derives and stores the identical value at provisioning/startup, so both
/// sides agree without ever exchanging it. Preimage resistance means a
/// deposit token (semi-public: it rides QR friend cards) reveals nothing
/// about the member token it was derived from — provided the member token
/// is high-entropy, which `DEPLOY.md` §1 requires (`openssl rand -hex 32`).
///
/// Idempotent: a token that is already deposit-class is returned unchanged,
/// so re-encoding a card can never double-attenuate. Empty input stays empty.
#[uniffi::export]
pub fn relay_deposit_token_for(member_token: String) -> String {
    let member = member_token.trim();
    if member.is_empty() || member.starts_with(RELAY_DEPOSIT_TOKEN_PREFIX) {
        return member.to_string();
    }
    let mut hasher = Blake2bVar::new(32).expect("valid blake2b output length");
    hasher.update(RELAY_DEPOSIT_TOKEN_CONTEXT);
    hasher.update(member.as_bytes());
    let mut out = [0u8; 32];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    format!(
        "{RELAY_DEPOSIT_TOKEN_PREFIX}{}",
        BASE64URL_NOPAD.encode(&out)
    )
}

/// Which relay mailbox serves an envelope addressed to a contact: the
/// contact's own mailbox from their friend card when complete, else the
/// device's saved fallback config. relayd scopes every row per family token,
/// so cross-family delivery only works when the envelope lands in the
/// *recipient's* mailbox — posting to the sender's own mailbox strands it
/// (T11). Shared here so both shells route identically.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct RelayEndpoint {
    pub url: String,
    pub token: String,
}

/// Send-path routing (CP4-aware). The contact's card credential wins when
/// complete — for a post-CP4 card that is their family's deposit token,
/// exactly the capability a send needs — with one refinement: when the
/// contact's deposit token is the attenuation of our OWN member token (same
/// family, same relay), the send uses our member credential instead. Family
/// traffic thus stays on the member-class rate buckets and is never
/// throttled by the tighter deposit allowance; only genuine cross-family
/// deposits ride the deposit bucket.
#[uniffi::export]
pub fn resolved_contact_relay(
    contact_relay_url: Option<String>,
    contact_relay_token: Option<String>,
    fallback_url: Option<String>,
    fallback_token: Option<String>,
) -> Option<RelayEndpoint> {
    let contact = relay_endpoint_from(contact_relay_url, contact_relay_token);
    let fallback = relay_endpoint_from(fallback_url, fallback_token);
    match (contact, fallback) {
        (Some(contact), Some(fallback)) => {
            if contact.url == fallback.url
                && relay_token_is_deposit(contact.token.clone())
                && relay_deposit_token_for(fallback.token.clone()) == contact.token
            {
                Some(fallback)
            } else {
                Some(contact)
            }
        }
        (contact, fallback) => contact.or(fallback),
    }
}

/// Does this contact's card credential belong to the *same* Cruise Pass as
/// ours? Both classes of card count: a post-CP4 card carries our family's
/// deposit token (the attenuation of our member token), a pre-CP4 one carries
/// the member token itself.
///
/// A pure comparison, and `false` whenever either side has no pass — "not
/// known to be the same family" rather than "known to be a different one".
/// Deciding what an *absent* pass should mean is a policy question with a
/// different answer per caller, so it lives in
/// [`friend_introduction_eligible`] rather than here.
#[uniffi::export]
pub fn relay_contact_shares_own_family(
    contact_relay_url: Option<String>,
    contact_relay_token: Option<String>,
    own_relay_url: Option<String>,
    own_relay_token: Option<String>,
) -> bool {
    let (Some(contact), Some(own)) = (
        relay_endpoint_from(contact_relay_url, contact_relay_token),
        relay_endpoint_from(own_relay_url, own_relay_token),
    ) else {
        return false;
    };
    contact.url == own.url
        && (contact.token == own.token || contact.token == relay_deposit_token_for(own.token))
}

/// May this contact take part in friends-of-friends introductions with us —
/// as a candidate we offer, or as a recipient we send a directory to?
///
/// Introductions are scoped to one Cruise Pass (specs/friends-of-friends.md
/// decision 7), because the contact graph does not stop at a household: one
/// person who has scanned somebody outside the family is otherwise enough for
/// that outside circle to propagate into family suggestion lists.
///
/// The rule, and why an absent pass is not simply "ours":
///
/// - **We have a pass.** Eligible only if the contact is on it. A contact with
///   no pass is *not yet* in the family rather than outside it, and becomes
///   eligible the moment they enter the pass we gave them — the pass-change
///   re-fan handles that automatically. Counting them in meanwhile is what
///   reopened the leak: a family met on holiday who never bought a pass looks
///   identical to a relative who has not finished onboarding.
/// - **Neither of us has a pass.** There is no family boundary drawn yet, so
///   fall back to the only boundary that exists — whether we actually met.
///   `contact_added_nearby` is the stored fact that this contact was added
///   over a nearby transport (`ContactProvenance::added_nearby`), which a
///   remote re-add never unmakes.
/// - **They have a pass and we do not.** Not eligible: they belong to a family
///   whose boundary we cannot see, and we are in no position to introduce
///   across it.
///
/// Still scoping, not access control. Reading another family's mailbox is
/// prevented by the token class itself (see [`resolved_contact_poll_relay`]).
#[uniffi::export]
pub fn friend_introduction_eligible(
    contact_relay_url: Option<String>,
    contact_relay_token: Option<String>,
    own_relay_url: Option<String>,
    own_relay_token: Option<String>,
    contact_added_nearby: bool,
) -> bool {
    let contact = relay_endpoint_from(contact_relay_url.clone(), contact_relay_token.clone());
    let own = relay_endpoint_from(own_relay_url.clone(), own_relay_token.clone());
    match (contact, own) {
        (Some(_), Some(_)) => relay_contact_shares_own_family(
            contact_relay_url,
            contact_relay_token,
            own_relay_url,
            own_relay_token,
        ),
        (None, None) => contact_added_nearby,
        _ => false,
    }
}

/// Poll-path routing (CP4): which mailbox, if any, may be *read*
/// (fetch/ack/presence) on this contact's behalf. Deposit tokens cannot
/// read, so a resolved endpoint that would carry one is dropped rather than
/// handed to the sync engine to 403 on every pass:
///
/// - Same family (the card token attenuates from our own member token):
///   `resolved_contact_relay` already resolved to our member endpoint —
///   polled as before.
/// - Legacy member-class card token: still polled (pre-CP4 proxy-polling
///   keeps working until the contact re-shares their card).
/// - Cross-family deposit-class token: `None`. Reading another family's
///   mailbox with a friend-card credential is exactly the capability CP4
///   removes; the contact's family drains its own mailbox.
#[uniffi::export]
pub fn resolved_contact_poll_relay(
    contact_relay_url: Option<String>,
    contact_relay_token: Option<String>,
    fallback_url: Option<String>,
    fallback_token: Option<String>,
) -> Option<RelayEndpoint> {
    resolved_contact_relay(
        contact_relay_url,
        contact_relay_token,
        fallback_url,
        fallback_token,
    )
    .filter(|endpoint| !relay_token_is_deposit(endpoint.token.clone()))
}

/// Send-path routing when the contact's card endpoint has been written off
/// (see [`crate::contact_relay_health`]).
///
/// A card whose endpoint authoritatively rejects us is worse than no card at
/// all: [`resolved_contact_relay`] returns the contact endpoint
/// unconditionally, so one dead field beats a working alternative *forever*
/// and the messages never leave the queue. This is that same resolution with
/// one added rule — a written-off endpoint is skipped, exactly as though the
/// card had carried no relay fields, which falls through to our own.
///
/// Falling back is not a new capability: a card with no relay fields already
/// resolves to our own endpoint today. It is also the routing that actually
/// delivers whenever the contact is in our own family (they poll the mailbox
/// we are posting to) — the common case for somebody we handed a Cruise Pass
/// to. For a cross-family contact it delivers nothing, but neither did the
/// dead endpoint, and unlike the dead endpoint this state is surfaced, so a
/// person can repair the card.
#[uniffi::export]
pub fn resolved_contact_delivery_relay(
    contact_relay_url: Option<String>,
    contact_relay_token: Option<String>,
    fallback_url: Option<String>,
    fallback_token: Option<String>,
    contact_endpoint_usable: bool,
) -> Option<RelayEndpoint> {
    if contact_endpoint_usable {
        return resolved_contact_relay(
            contact_relay_url,
            contact_relay_token,
            fallback_url,
            fallback_token,
        );
    }
    let Some(fallback) = relay_endpoint_from(fallback_url, fallback_token) else {
        return None;
    };
    // Only worth a request if it is somewhere other than the host we just
    // wrote off; otherwise report "nowhere to post" honestly rather than
    // retrying the same dead host under a different name.
    match relay_endpoint_from(contact_relay_url, contact_relay_token) {
        Some(contact) if contact.url == fallback.url => None,
        _ => Some(fallback),
    }
}

/// One group member's relay situation, as the shell resolved it this pass.
///
/// The two health flags are deliberately separate rather than pre-combined:
/// they justify different answers when the member's endpoint is out of
/// service, and collapsing them into one "unusable" bit is exactly how the
/// fan-out path came to redirect a resting member's mail to our own mailbox.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct GroupRelayMember {
    pub relay_url: Option<String>,
    pub relay_token: Option<String>,
    /// False once this member's card endpoint has been written off for
    /// authoritative rejections — [`crate::contact_relay_health::core_contact_relay_endpoint_usable`].
    pub endpoint_usable: bool,
    /// False while this member's card endpoint is resting because it stopped
    /// answering — [`crate::contact_relay_health::core_contact_relay_unreachable_endpoint_usable`].
    pub endpoint_answering: bool,
}

/// Which single mailbox a group envelope's fan-out rows go to, or `None` for
/// "post nothing this pass".
///
/// Group text is addressed to the group id, not to a person, so the shells
/// pick one mailbox and post every per-member row there
/// (specs/group-relay-durability.md §4.2). The choice walks the membership in
/// order and takes the first member that resolves to somewhere worth posting,
/// falling back to our own configured relay when none of them carries a card
/// endpoint of their own.
///
/// The rule this exists to hold is the last one. A member whose endpoint is
/// *resting for silence* contributes no fallback: if nobody else in the group
/// resolves, the answer is `None` and the envelope simply is not posted this
/// pass. Falling back would put a cross-family member's copy in our own
/// mailbox, which they never read — and because `relay_posted_at` is
/// terminal, that is not a retry but a permanent misroute. `None` leaves the
/// envelope queued for a later pass and for the BLE/LAN paths, so a host that
/// comes back still receives it.
///
/// A member written off for *rejection* keeps falling back, unchanged: a 401
/// proves the card is wrong, and our own relay really delivers when both
/// sides have since moved to the same new host.
#[uniffi::export]
pub fn core_group_fanout_relay_target(
    members: Vec<GroupRelayMember>,
    fallback_url: Option<String>,
    fallback_token: Option<String>,
) -> Option<RelayEndpoint> {
    let mut any_member_resting = false;
    for member in members {
        if !member.endpoint_answering {
            any_member_resting = true;
            continue;
        }
        if let Some(endpoint) = resolved_contact_delivery_relay(
            member.relay_url,
            member.relay_token,
            fallback_url.clone(),
            fallback_token.clone(),
            member.endpoint_usable,
        ) {
            return Some(endpoint);
        }
    }
    if any_member_resting {
        return None;
    }
    relay_endpoint_from(fallback_url, fallback_token)
}

/// Poll-path routing with the same written-off rule.
///
/// Proxy-polling a written-off endpoint is pure waste — it rejects every
/// pass exactly as the posts did — so a stale card drops out of the poll set
/// entirely. There is deliberately no fallback here: our own mailbox is
/// already polled on its own account, and reading it again under a contact's
/// heading would fetch nothing new.
#[uniffi::export]
pub fn resolved_contact_delivery_poll_relay(
    contact_relay_url: Option<String>,
    contact_relay_token: Option<String>,
    fallback_url: Option<String>,
    fallback_token: Option<String>,
    contact_endpoint_usable: bool,
) -> Option<RelayEndpoint> {
    if !contact_endpoint_usable {
        return None;
    }
    resolved_contact_poll_relay(
        contact_relay_url,
        contact_relay_token,
        fallback_url,
        fallback_token,
    )
}

fn relay_endpoint_from(url: Option<String>, token: Option<String>) -> Option<RelayEndpoint> {
    let url = normalize_relay_url(url.unwrap_or_default());
    let token = token.unwrap_or_default().trim().to_string();
    if url.is_empty() || token.is_empty() {
        None
    } else {
        Some(RelayEndpoint { url, token })
    }
}

/// Canonicalize a relay base URL, **rejecting anything that would put the
/// family's relay token on an unencrypted connection**.
///
/// A bare host still gains an implicit `https://`, as it always did. What
/// changed: an explicit non-HTTPS scheme no longer passes through. It returns
/// the empty string instead, which every caller already reads as "no relay
/// configured" — so the rejection fails closed at load, at save, at friend-card
/// import, and at relay-update apply without any of them needing a new branch.
///
/// This is the *only* chokepoint that sees every relay URL the app will ever
/// use, from three sources with very different trust: a URL the user typed, a
/// URL inside a scanned friend card, and a URL inside a kind-9 relay-change
/// notice sealed by a contact. `validate_setup` has always required HTTPS for
/// Cruise Pass setup cards; the other two paths reached
/// [`RelayConfig`](crate::relay_setup) with whatever scheme they carried. Message
/// bodies are sealed either way, so this is not about message secrecy — it is
/// the relay token, the recipient hints, and the envelope sizes, which an
/// `http://` endpoint hands to anyone on the path. Until now the only thing
/// stopping that was each platform's cleartext-traffic default (Android
/// `targetSdk` 36, iOS ATS), which is a manifest setting away from silently
/// regressing.
///
/// Plain `http://` survives for loopback only — see [`is_loopback_relay_host`].
#[uniffi::export]
pub fn normalize_relay_url(value: String) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    let candidate = match trimmed.split_once("://") {
        // Lowercase the scheme so downstream `starts_with("https://")` checks
        // (`relay_setup_is_official`, the WebSocket upgrade in both shells)
        // can't be sidestepped by `HTTPS://`.
        Some((scheme, rest)) => format!("{}://{}", scheme.to_ascii_lowercase(), rest),
        None => format!("https://{trimmed}"),
    };
    if relay_url_transport_is_secure(&candidate) {
        candidate
    } else {
        String::new()
    }
}

/// True when a non-empty relay URL was rejected by [`normalize_relay_url`] for
/// using an unencrypted transport. Purely for user-facing copy: the shells show
/// "must start with https://" under a manually typed field instead of letting
/// the value silently vanish. Remote sources (friend cards, relay-update
/// notices) deliberately do not surface anything.
#[uniffi::export]
pub fn relay_url_is_insecure(value: String) -> bool {
    !value.trim().trim_end_matches('/').is_empty() && normalize_relay_url(value).is_empty()
}

fn relay_url_transport_is_secure(url: &str) -> bool {
    match url.split_once("://") {
        Some(("https", _)) => true,
        Some(("http", _)) => is_loopback_relay_host(relay_url_host(url)),
        _ => false,
    }
}

/// Extract the host from an already-schemed URL, the way a browser would:
/// authority is everything up to the first `/`, `?`, or `#`; userinfo before
/// the *last* `@` is discarded; a bracketed IPv6 literal keeps its brackets off.
///
/// Parsing this by hand rather than by prefix match is the whole point — a
/// naive "contains 127.0.0.1" check would wave through
/// `http://127.0.0.1@attacker.example/`, whose real host is `attacker.example`.
fn relay_url_host(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    match host_port.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => host_port.rsplit_once(':').map_or(host_port, |(h, _)| h),
    }
}

/// Hosts still reachable over plain `http://`: the loopback interface, and the
/// Android emulator's alias for its host machine.
///
/// Running relayd locally over HTTP is how the relay is developed and how
/// `tools/relay_admin.sh` talks to a box through an SSH tunnel, so forbidding
/// it outright would trade a real workflow for no attacker-visible traffic —
/// loopback never leaves the device. `10.0.2.2` is unroutable off an emulator,
/// so honouring it costs nothing on a real phone; the existing
/// `RelayClientTest` pins it.
fn is_loopback_relay_host(host: &str) -> bool {
    const ANDROID_EMULATOR_HOST: &str = "10.0.2.2";
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host == ANDROID_EMULATOR_HOST
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Maximum response body that either mobile shell may accumulate before
/// cancelling the relay request. The core repeats this check at every decoder
/// so callers outside the first-party shells cannot bypass it.
#[uniffi::export]
pub fn relay_max_response_bytes() -> u32 {
    RELAY_MAX_RESPONSE_BODY_BYTES as u32
}

/// Rows a shell asks for per fetch page — see [`RELAY_FETCH_MAX_ROWS`] for
/// why this is 256 and what still bounds the memory a page can cost.
///
/// A shell must not treat a *short* page as end-of-mailbox: a server may
/// clamp `limit=` below this. [`crate::relay_fetch_walk_continues`] owns that
/// rule for both shells.
#[uniffi::export]
pub fn relay_fetch_batch_limit() -> u32 {
    RELAY_FETCH_MAX_ROWS as u32
}

/// The `limit=` to retry a fetch with after the relay's answer came back
/// bigger than [`relay_max_response_bytes`] — or `None` when there is nothing
/// left to shrink.
///
/// ## Why the client needs this at all
///
/// A row-counted page has no byte bound. `sealed` may be up to 512 KiB
/// ([`RELAY_MAX_SEALED_BYTES`]) and rides base64 inside JSON, so a mailbox
/// holding enough large attachment chunks can produce a 256-row window whose
/// body is past the 12 MiB cap. The shells refuse that body at the transport
/// (they must — it is the only thing bounding how much a hostile or
/// misbehaving relay can make a phone allocate), and because the next pass
/// asks the same relay for the same window from the same cursor, it fails
/// identically. The frontier never advances and the mailbox is stuck until
/// those rows expire.
///
/// Current relayd carries a byte budget of its own and never builds such a
/// page. That fixes the relays we run — but family relays are self-hosted,
/// nobody is obliged to upgrade one, and a phone cannot tell a
/// budget-enforcing relayd from an older build until a page has already blown
/// the cap. So the client keeps its own escape hatch: ask for half as many
/// rows and try the very same cursor again. Halving reaches a single row in
/// at most eight steps from 256, and one row is always fetchable, because a
/// single `sealed` maxes out at 512 KiB — over twenty times under the cap
/// even after base64.
///
/// `None` means *stop*: a one-row page that still exceeds the cap is not a
/// paging problem at all (nothing smaller can be asked for), so the caller
/// should surface the failure rather than spin.
#[uniffi::export]
pub fn relay_fetch_shrunk_limit(current_limit: u32) -> Option<u32> {
    // Clamp first: a caller that somehow asked above our own ceiling must
    // come back with something `relay_build_fetch_path` will accept.
    let current = current_limit.min(RELAY_FETCH_MAX_ROWS as u32);
    if current <= 1 {
        return None;
    }
    Some((current / 2).max(1))
}

#[uniffi::export]
pub fn relay_encode_post_envelope(
    msg_id: Vec<u8>,
    hop_ttl: u8,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
    expiry_ms: i64,
) -> Result<Vec<u8>, CoreError> {
    validate_envelope(&msg_id, &recipient_hint, &sealed)?;
    json_encode(&PostEnvelopeWire {
        msg_id: b64(&msg_id),
        hop_ttl,
        recipient_hint: b64(&recipient_hint),
        sealed: b64(&sealed),
        expiry_ms,
    })
}

#[uniffi::export]
pub fn relay_decode_post_response(body: Vec<u8>) -> Result<i64, CoreError> {
    validate_response_body(&body)?;
    let id = json_decode::<PostResponse>(&body)?.id;
    if id < 0 {
        return Err(malformed("relay id cannot be negative"));
    }
    Ok(id)
}

#[uniffi::export]
pub fn relay_build_fetch_path(
    hints: Vec<Vec<u8>>,
    after_id: i64,
    limit: u32,
) -> Result<String, CoreError> {
    if limit == 0 || limit as usize > RELAY_FETCH_MAX_ROWS {
        return Err(malformed("relay fetch limit is out of range"));
    }
    if after_id < 0 {
        return Err(malformed("relay cursor cannot be negative"));
    }
    for hint in &hints {
        validate_hint(hint)?;
    }
    Ok(format!(
        "/envelopes?hints={}&after={after_id}&limit={limit}",
        hints
            .iter()
            .map(|hint| b64(hint))
            .collect::<Vec<_>>()
            .join(",")
    ))
}

#[uniffi::export]
pub fn relay_decode_fetch_page(body: Vec<u8>) -> Result<CoreRelayFetchPage, CoreError> {
    validate_response_body(&body)?;
    let wire = json_decode::<FetchResponse>(&body)?;
    if wire.envelopes.len() > RELAY_FETCH_MAX_ROWS {
        return Err(malformed("relay fetch page contains too many envelopes"));
    }
    if wire.next_cursor < 0 {
        return Err(malformed("relay cursor cannot be negative"));
    }

    let mut envelopes = Vec::with_capacity(wire.envelopes.len());
    let mut decoded_bytes = 0usize;
    let mut previous_id = None;
    for item in wire.envelopes {
        if item.id < 0 || previous_id.is_some_and(|previous| item.id <= previous) {
            return Err(malformed(
                "relay envelope ids must be non-negative and increasing",
            ));
        }
        validate_b64_len(&item.msg_id, 16, "relay msg_id")?;
        validate_b64_len(&item.recipient_hint, 8, "relay recipient hint")?;
        if item.sealed.is_empty() || item.sealed.len() > max_b64_len(RELAY_MAX_SEALED_BYTES) {
            return Err(malformed("relay sealed payload is too large"));
        }

        let msg_id = b64_decode(&item.msg_id)?;
        let recipient_hint = b64_decode(&item.recipient_hint)?;
        let sealed = b64_decode(&item.sealed)?;
        validate_envelope(&msg_id, &recipient_hint, &sealed)?;
        decoded_bytes = decoded_bytes
            .checked_add(sealed.len())
            .ok_or_else(|| malformed("relay fetch page payload size overflow"))?;
        if decoded_bytes > RELAY_FETCH_MAX_DECODED_BYTES {
            return Err(malformed("relay fetch page payload is too large"));
        }
        previous_id = Some(item.id);
        envelopes.push(CoreRelayFetchedEnvelope {
            id: item.id,
            msg_id,
            hop_ttl: item.hop_ttl,
            recipient_hint,
            sealed,
            expiry_ms: item.expiry_ms,
        });
    }
    if previous_id.is_some_and(|last_id| wire.next_cursor != last_id) {
        return Err(malformed("relay cursor does not match the fetch page"));
    }
    Ok(CoreRelayFetchPage {
        envelopes,
        next_cursor: wire.next_cursor,
    })
}

#[uniffi::export]
pub fn relay_encode_ack_request(ids: Vec<i64>) -> Result<Vec<u8>, CoreError> {
    if ids.iter().any(|id| *id < 0) {
        return Err(malformed("relay id cannot be negative"));
    }
    json_encode(&AckRequest { ids })
}

#[uniffi::export]
pub fn relay_encode_presence_request(
    announce: Vec<Vec<u8>>,
    query: Vec<Vec<u8>>,
) -> Result<Vec<u8>, CoreError> {
    for hint in announce.iter().chain(&query) {
        validate_hint(hint)?;
    }
    json_encode(&PresenceRequest {
        announce: announce.iter().map(|v| b64(v)).collect(),
        query: query.iter().map(|v| b64(v)).collect(),
    })
}

#[uniffi::export]
pub fn relay_decode_presence_page(body: Vec<u8>) -> Result<CoreRelayPresencePage, CoreError> {
    validate_response_body(&body)?;
    let wire = json_decode::<PresenceResponse>(&body)?;
    if wire.presence.len() > RELAY_MAX_PRESENCE_ROWS {
        return Err(malformed("relay presence page contains too many entries"));
    }
    let presence = wire
        .presence
        .into_iter()
        .map(|item| {
            let hint = b64_decode(&item.hint)?;
            validate_hint(&hint)?;
            Ok(CoreRelayPresence {
                hint,
                last_seen_ms: item.last_seen_ms,
            })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;
    Ok(CoreRelayPresencePage {
        now_ms: wire.now_ms,
        presence,
    })
}

fn validate_envelope(msg_id: &[u8], hint: &[u8], sealed: &[u8]) -> Result<(), CoreError> {
    if msg_id.len() != 16 {
        return Err(malformed("relay msg_id must be 16 bytes"));
    }
    validate_hint(hint)?;
    if sealed.is_empty() {
        return Err(malformed("relay sealed payload cannot be empty"));
    }
    if sealed.len() > RELAY_MAX_SEALED_BYTES {
        return Err(malformed("relay sealed payload is too large"));
    }
    Ok(())
}

fn validate_hint(hint: &[u8]) -> Result<(), CoreError> {
    if hint.len() != 8 {
        return Err(malformed("relay recipient hint must be 8 bytes"));
    }
    Ok(())
}

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}
fn b64_decode(value: &str) -> Result<Vec<u8>, CoreError> {
    BASE64URL_NOPAD
        .decode(value.as_bytes())
        .map_err(|_| malformed("invalid relay base64url"))
}
fn validate_b64_len(value: &str, decoded_len: usize, field: &str) -> Result<(), CoreError> {
    if value.len() != max_b64_len(decoded_len) {
        return Err(malformed(&format!("{field} has invalid encoded length")));
    }
    Ok(())
}
fn max_b64_len(decoded_len: usize) -> usize {
    decoded_len.saturating_mul(4).saturating_add(2) / 3
}
fn validate_response_body(body: &[u8]) -> Result<(), CoreError> {
    if body.len() > RELAY_MAX_RESPONSE_BODY_BYTES {
        return Err(malformed("relay response body is too large"));
    }
    Ok(())
}
fn json_encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CoreError> {
    serde_json::to_vec(value).map_err(|e| malformed(&format!("invalid relay JSON: {e}")))
}
fn json_decode<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, CoreError> {
    serde_json::from_slice(body).map_err(|e| malformed(&format!("invalid relay JSON: {e}")))
}
fn malformed(message: &str) -> CoreError {
    CoreError::Malformed(message.to_string())
}

/// Whether a contact can be reached at all when no direct path exists.
///
/// This answers the one question a chat app trained on Signal and WhatsApp
/// never has to ask: *can I message this person from across the country?*
/// Nearby delivery is always free and always available; delivery over the
/// internet needs the **recipient** to have a mailbox, because the sender
/// posts into the recipient's mailbox and the recipient fetches from it
/// (DESIGN.md §9.1). A contact who never shared internet delivery cannot be
/// reached from a distance no matter how long the message waits, and saying
/// otherwise is the kind of promise this app must not make.
///
/// This is a property of the *credentials on the friend card*, not of the
/// contact's current presence. A card can be stale (their pass may have
/// expired since they shared it), so callers must present this as "they
/// shared internet delivery", never "they are online now" — the two are
/// separate axes and only the second needs live evidence.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ContactDelivery {
    /// The contact rides the same mailbox this phone fetches from: our own
    /// family's Cruise Pass. Internet delivery works in both directions and
    /// their presence is observable, because we hold the member credential.
    SharedMailbox,
    /// The contact shared internet delivery of their own (another family's
    /// pass, or a self-hosted relay). We can post to it; post-CP4 we cannot
    /// read their presence from it, because a friend card carries a
    /// post-only deposit credential.
    OwnMailbox { host: String },
    /// The contact shared no internet delivery. Only nearby paths -- direct
    /// link, or a phone carrying for us -- will ever reach them.
    NearbyOnly,
}

/// Classify what [ContactDelivery] a contact's card affords, given our own
/// relay configuration. Credentials are compared internally and never
/// returned; only the host is exposed, and only for a contact's own mailbox.
#[uniffi::export]
pub fn contact_delivery(
    contact_relay_url: Option<String>,
    contact_relay_token: Option<String>,
    own_relay_url: Option<String>,
    own_relay_token: Option<String>,
) -> ContactDelivery {
    let Some(contact) = relay_endpoint_from(contact_relay_url, contact_relay_token) else {
        return ContactDelivery::NearbyOnly;
    };
    if let Some(own) = relay_endpoint_from(own_relay_url, own_relay_token) {
        // Same mailbox when the host matches and the card's credential is
        // either our own member token (a legacy pre-CP4 card) or its deposit
        // attenuation (a current card).
        let same_host = own.url == contact.url;
        let same_family = contact.token == own.token
            || contact.token == relay_deposit_token_for(own.token.clone());
        if same_host && same_family {
            return ContactDelivery::SharedMailbox;
        }
    }
    ContactDelivery::OwnMailbox {
        host: relay_host_only(&contact.url),
    }
}

/// Which direction of a one-to-one conversation cannot carry beyond
/// Bluetooth range, decided entirely from local facts -- no network call, no
/// round trip, and no evidence of the other phone's current state.
///
/// The asymmetry this exists to explain: sending needs nothing of your own
/// (their friend card carries the credential that authorises a post into
/// *their* mailbox), but receiving needs a mailbox you poll, which needs a
/// pass of your own. So a person with no pass can reach everyone and be
/// reached by no one, and today both people believe they are connected.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ComposerReach {
    /// Say nothing. Either a path exists in both directions, or the contact
    /// is nearby right now, or we met them in person and nearby delivery was
    /// always the point.
    Fine,
    /// We have no mailbox of our own: we can post to them, but their replies
    /// have nowhere to land until we hold a pass or they come back in range.
    RepliesCannotReachMe,
    /// They shared no internet delivery: our messages wait for range.
    TheyCannotBeReached,
    /// Neither of us has a mailbox. Nothing crosses in either direction
    /// unless the phones are near each other.
    NeitherDirectionWorks,
}

/// What (if anything) the composer should say about a one-to-one chat.
///
/// `contact_nearby` must come from the same live link lookup the send path
/// uses -- when a direct BLE/LAN link exists, everything works and the
/// composer stays quiet. `added_while_nearby` is
/// `ContactProvenance::added_nearby`: adding someone in person carries an
/// implicit "we are standing together, nearby delivery is the point", so that
/// case stays silent rather than nagging about a limit both people chose.
/// Being introduced remotely carries the opposite implication -- the whole
/// encounter was internet-mediated -- so the absence of a mailbox is a genuine
/// surprise and gets said out loud.
#[uniffi::export]
pub fn composer_reach(
    delivery: ContactDelivery,
    own_relay_configured: bool,
    contact_nearby: bool,
    added_while_nearby: bool,
) -> ComposerReach {
    if contact_nearby || added_while_nearby {
        return ComposerReach::Fine;
    }
    let they_are_unreachable = delivery == ContactDelivery::NearbyOnly;
    match (own_relay_configured, they_are_unreachable) {
        (false, true) => ComposerReach::NeitherDirectionWorks,
        (false, false) => ComposerReach::RepliesCannotReachMe,
        (true, true) => ComposerReach::TheyCannotBeReached,
        (true, false) => ComposerReach::Fine,
    }
}

/// Host (and port) of a relay URL, with scheme and path stripped -- safe to
/// show a user who already holds the card. Never includes the credential.
fn relay_host_only(url: &str) -> String {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // OWN_URL / OWN_TOKEN are defined with the contact_delivery tests below.
    const OTHER_TOKEN: &str = "other-family-member-token";

    fn shares(contact_url: Option<&str>, contact_token: Option<&str>) -> bool {
        relay_contact_shares_own_family(
            contact_url.map(str::to_string),
            contact_token.map(str::to_string),
            Some(OWN_URL.to_string()),
            Some(OWN_TOKEN.to_string()),
        )
    }

    #[test]
    fn own_family_cards_of_both_token_classes_are_recognized() {
        // Post-CP4 card: our family's deposit token.
        assert!(shares(
            Some(OWN_URL),
            Some(&relay_deposit_token_for(OWN_TOKEN.into()))
        ));
        // Pre-CP4 card: the member token itself.
        assert!(shares(Some(OWN_URL), Some(OWN_TOKEN)));
    }

    #[test]
    fn another_familys_card_is_foreign_in_both_classes() {
        // The tester-pass case this scoping exists for: a real, working card
        // on the same relay host, but a different family's mailbox.
        assert!(!shares(
            Some(OWN_URL),
            Some(&relay_deposit_token_for(OTHER_TOKEN.into()))
        ));
        assert!(!shares(Some(OWN_URL), Some(OTHER_TOKEN)));
    }

    #[test]
    fn our_own_token_on_a_different_relay_host_is_foreign() {
        assert!(!shares(Some("https://other.example"), Some(OWN_TOKEN)));
    }

    #[test]
    fn the_pass_comparison_itself_is_false_whenever_either_side_has_none() {
        // Pure comparison: absent is not "same". What absence *means* is
        // decided by friend_introduction_eligible, tested below.
        assert!(!shares(None, None));
        assert!(!shares(Some(OWN_URL), None));
        assert!(!shares(None, Some(OWN_TOKEN)));
        // Blank-but-present fields are the same "no endpoint" state.
        assert!(!shares(Some("   "), Some("   ")));
        assert!(!relay_contact_shares_own_family(
            Some(OWN_URL.into()),
            Some(OTHER_TOKEN.into()),
            None,
            None,
        ));
    }

    fn eligible(
        contact: Option<(&str, &str)>,
        own: Option<(&str, &str)>,
        added_nearby: bool,
    ) -> bool {
        friend_introduction_eligible(
            contact.map(|c| c.0.to_string()),
            contact.map(|c| c.1.to_string()),
            own.map(|o| o.0.to_string()),
            own.map(|o| o.1.to_string()),
            added_nearby,
        )
    }

    #[test]
    fn with_a_pass_only_contacts_on_it_may_be_introduced() {
        let ours = (OWN_URL, OWN_TOKEN);
        let own_card = relay_deposit_token_for(OWN_TOKEN.into());
        assert!(eligible(Some((OWN_URL, &own_card)), Some(ours), false));
        // Another family's card, however real: never.
        let their_card = relay_deposit_token_for(OTHER_TOKEN.into());
        assert!(!eligible(Some((OWN_URL, &their_card)), Some(ours), true));
    }

    #[test]
    fn with_a_pass_a_contact_without_one_waits_until_they_join_it() {
        // The holiday-acquaintance case: meeting them in person is not enough
        // to make them family, and being nearby-scanned must not buy an
        // exception -- that is exactly how a relative mid-onboarding looks.
        let ours = (OWN_URL, OWN_TOKEN);
        assert!(!eligible(None, Some(ours), true));
        assert!(!eligible(None, Some(ours), false));
    }

    #[test]
    fn without_a_pass_meeting_in_person_is_the_only_boundary_left() {
        // No family boundary is drawn yet, so fall back to whether we
        // actually met. A remotely-added stranger stays out.
        assert!(eligible(None, None, true));
        assert!(!eligible(None, None, false));
    }

    #[test]
    fn without_a_pass_a_contact_who_has_one_belongs_to_a_family_we_cannot_see() {
        let theirs = relay_deposit_token_for(OTHER_TOKEN.into());
        assert!(!eligible(Some((OWN_URL, &theirs)), None, true));
    }

    #[test]
    fn a_deposit_token_is_never_mistaken_for_the_member_token_it_came_from() {
        // Guards the direction of the attenuation: holding our deposit token
        // means same family, but our deposit token must not match somebody
        // else's member token by accident.
        let ours = relay_deposit_token_for(OWN_TOKEN.into());
        assert_ne!(ours, OWN_TOKEN);
        assert!(!relay_contact_shares_own_family(
            Some(OWN_URL.into()),
            Some(OWN_TOKEN.into()),
            Some(OWN_URL.into()),
            Some(ours),
        ));
    }

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_relay_url(" relay.example/ ".into()),
            "https://relay.example"
        );
        assert_eq!(
            normalize_relay_url("http://127.0.0.1:8080/".into()),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn plain_http_relay_urls_are_rejected() {
        for insecure in [
            "http://relay.example",
            "http://relay.example:8080/",
            "http://192.168.1.50:8080",
            "http://10.0.0.7",
            // Loopback in the userinfo, attacker in the authority: the reason
            // the host is parsed instead of substring-matched.
            "http://127.0.0.1@attacker.example/",
            "http://localhost.attacker.example",
            "ws://relay.example",
            "ftp://relay.example",
        ] {
            assert_eq!(
                normalize_relay_url(insecure.into()),
                "",
                "expected {insecure} to be rejected"
            );
            assert!(relay_url_is_insecure(insecure.into()));
        }
    }

    #[test]
    fn https_and_loopback_http_survive() {
        for allowed in [
            "https://relay.example",
            "https://relay.example:8443/",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://127.5.5.5",
            "http://[::1]:8080",
            // The emulator's alias for its host machine, pinned by
            // RelayClientTest and unroutable off an emulator.
            "http://10.0.2.2:8080",
        ] {
            assert!(
                !normalize_relay_url(allowed.into()).is_empty(),
                "expected {allowed} to be accepted"
            );
            assert!(!relay_url_is_insecure(allowed.into()));
        }
    }

    #[test]
    fn scheme_case_is_normalized_so_prefix_checks_hold() {
        assert_eq!(
            normalize_relay_url("HTTPS://Relay.Example/".into()),
            "https://Relay.Example"
        );
        assert_eq!(
            normalize_relay_url("HtTp://relay.example".into()),
            "",
            "an uppercase scheme must not bypass the HTTPS gate"
        );
    }

    #[test]
    fn empty_input_is_not_reported_as_insecure() {
        assert!(!relay_url_is_insecure(String::new()));
        assert!(!relay_url_is_insecure("   ".into()));
        assert!(!relay_url_is_insecure("/".into()));
    }

    #[test]
    fn insecure_contact_endpoints_resolve_to_none() {
        // A friend card or a kind-9 relay-change notice naming an http:// host
        // must not become a usable endpoint just because it arrived sealed.
        assert_eq!(
            relay_endpoint_from(
                Some("http://relay.example".into()),
                Some("cmdep1-token".into())
            ),
            None
        );
        assert!(resolved_contact_relay(
            Some("http://contact.relay.example".into()),
            Some("cmdep1-token".into()),
            None,
            None,
        )
        .is_none());
    }

    /// Golden vector shared verbatim with relayd
    /// (`relayd/src/lib.rs::deposit_derivation_matches_core_golden_vector`):
    /// if either side's derivation drifts, its copy of this test fails.
    #[test]
    fn deposit_token_derivation_matches_golden_vector() {
        assert_eq!(
            relay_deposit_token_for("abc123".into()),
            "cmdep1-0uq69OqNyMo1Dd3vQcspqLlRY6bCCjTWvPyehXd6Ezs"
        );
        assert_eq!(
            relay_deposit_token_for("family-token".into()),
            "cmdep1-63hWvx1kHLKirfl9GV576eAi_rURpyZixpsCVUCXNJk"
        );
    }

    #[test]
    fn deposit_token_derivation_is_idempotent_and_trims() {
        let deposit = relay_deposit_token_for("abc123".into());
        assert_eq!(relay_deposit_token_for(deposit.clone()), deposit);
        assert_eq!(relay_deposit_token_for(" abc123 ".into()), deposit);
        assert_eq!(relay_deposit_token_for("  ".into()), "");
        assert!(relay_token_is_deposit(deposit));
        assert!(!relay_token_is_deposit("abc123".into()));
        assert!(!relay_token_is_deposit(String::new()));
    }

    #[test]
    fn send_path_prefers_member_credential_for_own_family_deposit() {
        // A family member's card carries the deposit form of our own member
        // token: sends must ride our member credential (member-class rate
        // buckets), not the tighter deposit allowance.
        let deposit = relay_deposit_token_for("token-own".into());
        let resolved = resolved_contact_relay(
            Some("https://own.relay.example".into()),
            Some(deposit.clone()),
            Some("own.relay.example/".into()),
            Some("token-own".into()),
        )
        .unwrap();
        assert_eq!(resolved.token, "token-own");

        // A cross-family deposit token is used as-is for sends: posting is
        // exactly what the deposit class allows.
        let resolved = resolved_contact_relay(
            Some("https://dana.relay.example".into()),
            Some(deposit.clone()),
            Some("https://own.relay.example".into()),
            Some("token-own".into()),
        )
        .unwrap();
        assert_eq!(resolved.url, "https://dana.relay.example");
        assert_eq!(resolved.token, deposit);
    }

    #[test]
    fn poll_path_never_resolves_to_a_deposit_credential() {
        let deposit = relay_deposit_token_for("token-own".into());

        // Same family → the member fallback is polled, exactly as pre-CP4.
        let resolved = resolved_contact_poll_relay(
            Some("https://own.relay.example".into()),
            Some(deposit.clone()),
            Some("https://own.relay.example".into()),
            Some("token-own".into()),
        )
        .unwrap();
        assert_eq!(resolved.token, "token-own");

        // Cross-family deposit → nothing to poll: deposit tokens cannot
        // fetch/ack/announce, so handing this endpoint to the sync engine
        // would only 403 on every pass.
        assert_eq!(
            resolved_contact_poll_relay(
                Some("https://dana.relay.example".into()),
                Some(deposit),
                Some("https://own.relay.example".into()),
                Some("token-own".into()),
            ),
            None
        );

        // Legacy member-class card token → still polled (pre-CP4
        // proxy-polling keeps working until the card is re-shared).
        let resolved = resolved_contact_poll_relay(
            Some("https://dana.relay.example".into()),
            Some("token-dana".into()),
            Some("https://own.relay.example".into()),
            Some("token-own".into()),
        )
        .unwrap();
        assert_eq!(resolved.token, "token-dana");
    }

    #[test]
    fn contact_relay_wins_over_fallback() {
        let resolved = resolved_contact_relay(
            Some(" dana.relay.example/ ".into()),
            Some(" token-dana ".into()),
            Some("https://own.relay.example".into()),
            Some("token-own".into()),
        )
        .unwrap();
        assert_eq!(resolved.url, "https://dana.relay.example");
        assert_eq!(resolved.token, "token-dana");
    }

    #[test]
    fn incomplete_contact_relay_falls_back_to_own_config() {
        let resolved = resolved_contact_relay(
            Some("dana.relay.example".into()),
            None,
            Some("https://own.relay.example/".into()),
            Some("token-own".into()),
        )
        .unwrap();
        assert_eq!(resolved.url, "https://own.relay.example");
        assert_eq!(resolved.token, "token-own");
    }

    #[test]
    fn no_usable_relay_resolves_to_none() {
        assert_eq!(
            resolved_contact_relay(None, None, Some("".into()), Some("  ".into())),
            None
        );
        assert_eq!(resolved_contact_relay(None, None, None, None), None);
    }

    #[test]
    fn post_and_fetch_wire_round_trip() {
        let body = relay_encode_post_envelope(vec![1; 16], 7, vec![2; 8], vec![3; 20], 99).unwrap();
        let item: PostEnvelopeWire = serde_json::from_slice(&body).unwrap();
        let response = serde_json::to_vec(&serde_json::json!({"envelopes": [{"id": 4,
            "msg_id": item.msg_id, "hop_ttl": item.hop_ttl, "recipient_hint": item.recipient_hint,
            "sealed": item.sealed, "expiry_ms": item.expiry_ms}], "next_cursor": 4}))
        .unwrap();
        let page = relay_decode_fetch_page(response).unwrap();
        assert_eq!(page.envelopes[0].msg_id, vec![1; 16]);
        assert_eq!(page.next_cursor, 4);
    }

    #[test]
    fn rejects_bad_lengths_and_base64() {
        assert!(relay_encode_post_envelope(vec![1; 15], 7, vec![2; 8], vec![3], 9).is_err());
        let bad = br#"{"envelopes":[{"id":1,"msg_id":"!","hop_ttl":7,"recipient_hint":"AgICAgICAgI","sealed":"Aw","expiry_ms":9}],"next_cursor":1}"#.to_vec();
        assert!(relay_decode_fetch_page(bad).is_err());
    }

    #[test]
    fn exposes_bounded_fetch_policy() {
        assert_eq!(relay_fetch_batch_limit(), 256);
        assert_eq!(relay_max_response_bytes(), 12 * 1024 * 1024);
        assert!(relay_build_fetch_path(vec![], 0, 256).is_ok());
        assert!(relay_build_fetch_path(vec![], 0, 257).is_err());
        assert!(relay_build_fetch_path(vec![], 0, 0).is_err());
        assert!(relay_build_fetch_path(vec![], -1, 1).is_err());
    }

    /// The batch limit and the decode bound have to stay consistent with each
    /// other and with the transport cap, or one of them silently stops being
    /// a bound. Pinned here so raising the row count again cannot quietly
    /// leave the memory ceiling behind.
    #[test]
    fn the_batch_limit_and_the_decode_bound_agree_with_the_transport_cap() {
        // The decode bound must never exceed the body cap: base64url expands
        // by 4/3, so a body under the cap can never decode to more than the
        // cap's worth of payload, and a decode bound above it would be dead
        // code pretending to be a limit.
        assert!(RELAY_FETCH_MAX_DECODED_BYTES <= RELAY_MAX_RESPONSE_BODY_BYTES);
        // A single maximum-size row must still fit, or a legitimate large
        // attachment chunk could never be fetched at all.
        assert!(RELAY_MAX_SEALED_BYTES <= RELAY_FETCH_MAX_DECODED_BYTES);
        // relayd's MAX_FETCH_LIMIT is 500 (relayd/src/lib.rs); asking for
        // more than the deployed server accepts would rely on its clamp.
        assert!(RELAY_FETCH_MAX_ROWS <= 500);
        assert_eq!(relay_fetch_batch_limit() as usize, RELAY_FETCH_MAX_ROWS);
    }

    #[test]
    fn an_oversize_page_shrinks_by_halving_and_bottoms_out_at_one_row() {
        // The recovery ladder from a full page: eight halvings reach one row,
        // and every rung is a limit the path builder will accept.
        let mut limit = relay_fetch_batch_limit();
        let mut ladder = vec![limit];
        while let Some(next) = relay_fetch_shrunk_limit(limit) {
            assert!(next < limit, "shrinking must make progress");
            assert!(relay_build_fetch_path(vec![], 0, next).is_ok());
            limit = next;
            ladder.push(limit);
        }
        assert_eq!(ladder, vec![256, 128, 64, 32, 16, 8, 4, 2, 1]);
    }

    #[test]
    fn a_single_row_page_that_is_still_too_big_stops_instead_of_spinning() {
        // Nothing smaller than one row can be asked for. Reporting "no
        // smaller ask exists" lets the caller surface the failure rather than
        // retry the identical request forever.
        assert_eq!(relay_fetch_shrunk_limit(1), None);
        assert_eq!(relay_fetch_shrunk_limit(0), None);
        // Odd limits still descend rather than sticking.
        assert_eq!(relay_fetch_shrunk_limit(3), Some(1));
        assert_eq!(relay_fetch_shrunk_limit(2), Some(1));
        // A limit above our own ceiling is clamped into range first, so the
        // retry is always something `relay_build_fetch_path` accepts.
        assert_eq!(relay_fetch_shrunk_limit(u32::MAX), Some(128));
    }

    /// A single maximum-size row must survive the smallest possible ask, or
    /// the shrink ladder would bottom out on a page that still cannot be
    /// decoded and the row would be unreachable forever.
    #[test]
    fn one_maximum_size_row_fits_the_body_cap_with_room_to_spare() {
        let encoded_msg_id = b64(&[1; 16]);
        let encoded_hint = b64(&[2; 8]);
        let sealed = b64(&vec![3; RELAY_MAX_SEALED_BYTES]);
        let body = serde_json::to_vec(&serde_json::json!({
            "envelopes": [{"id": 1, "msg_id": encoded_msg_id, "hop_ttl": 7,
                "recipient_hint": encoded_hint, "sealed": sealed, "expiry_ms": 9}],
            "next_cursor": 1
        }))
        .unwrap();
        assert!(body.len() < RELAY_MAX_RESPONSE_BODY_BYTES);
        let page = relay_decode_fetch_page(body).unwrap();
        assert_eq!(page.envelopes.len(), 1);
        assert_eq!(page.envelopes[0].sealed.len(), RELAY_MAX_SEALED_BYTES);
    }

    #[test]
    fn a_page_is_still_bounded_by_the_body_cap_after_the_row_count_rose() {
        // 256 rows of the maximum sealed size would be 128 MiB decoded, far
        // above anything the decoder may materialize. The body cap fires
        // first, before any JSON is parsed.
        let body = vec![b' '; RELAY_MAX_RESPONSE_BODY_BYTES + 1];
        assert!(relay_decode_fetch_page(body).is_err());

        // And the running decode check is still wired up: a page whose rows
        // sum past the bound is rejected even though each row is legal.
        let encoded_msg_id = b64(&[1; 16]);
        let encoded_hint = b64(&[2; 8]);
        let big_row = b64(&vec![3; RELAY_MAX_SEALED_BYTES]);
        let rows: Vec<_> = (1..=32)
            .map(|id| {
                serde_json::json!({
                    "id": id, "msg_id": encoded_msg_id, "hop_ttl": 7,
                    "recipient_hint": encoded_hint, "sealed": big_row, "expiry_ms": 9
                })
            })
            .collect();
        let body =
            serde_json::to_vec(&serde_json::json!({"envelopes": rows, "next_cursor": 32})).unwrap();
        // 32 × 512 KiB = 16 MiB of payload, which cannot survive either
        // check: the encoded body is already past the transport cap.
        assert!(relay_decode_fetch_page(body).is_err());
    }

    #[test]
    fn a_full_page_of_typical_rows_decodes() {
        // The realistic case the raise exists for: 256 ordinary ~1 KB rows
        // in one page, well inside every bound.
        let encoded_msg_id = b64(&[1; 16]);
        let encoded_hint = b64(&[2; 8]);
        let sealed = b64(&vec![3; 1024]);
        let rows: Vec<_> = (1..=RELAY_FETCH_MAX_ROWS as i64)
            .map(|id| {
                serde_json::json!({
                    "id": id, "msg_id": encoded_msg_id, "hop_ttl": 7,
                    "recipient_hint": encoded_hint, "sealed": sealed, "expiry_ms": 9
                })
            })
            .collect();
        let body = serde_json::to_vec(&serde_json::json!({
            "envelopes": rows, "next_cursor": RELAY_FETCH_MAX_ROWS as i64
        }))
        .unwrap();
        let page = relay_decode_fetch_page(body).unwrap();
        assert_eq!(page.envelopes.len(), RELAY_FETCH_MAX_ROWS);
        assert_eq!(page.next_cursor, RELAY_FETCH_MAX_ROWS as i64);
    }

    #[test]
    fn a_page_with_more_rows_than_the_limit_is_still_rejected() {
        let encoded_msg_id = b64(&[1; 16]);
        let encoded_hint = b64(&[2; 8]);
        let sealed = b64(&[3]);
        let rows: Vec<_> = (1..=RELAY_FETCH_MAX_ROWS as i64 + 1)
            .map(|id| {
                serde_json::json!({
                    "id": id, "msg_id": encoded_msg_id, "hop_ttl": 7,
                    "recipient_hint": encoded_hint, "sealed": sealed, "expiry_ms": 9
                })
            })
            .collect();
        let body = serde_json::to_vec(&serde_json::json!({
            "envelopes": rows, "next_cursor": RELAY_FETCH_MAX_ROWS as i64 + 1
        }))
        .unwrap();
        assert!(relay_decode_fetch_page(body).is_err());
    }

    #[test]
    fn rejects_oversized_or_non_monotonic_fetch_pages() {
        let encoded_msg_id = b64(&[1; 16]);
        let encoded_hint = b64(&[2; 8]);
        let oversized = b64(&vec![3; RELAY_MAX_SEALED_BYTES + 1]);
        let body = serde_json::to_vec(&serde_json::json!({
            "envelopes": [{"id": 1, "msg_id": encoded_msg_id, "hop_ttl": 7,
                "recipient_hint": encoded_hint, "sealed": oversized, "expiry_ms": 9}],
            "next_cursor": 1
        }))
        .unwrap();
        assert!(relay_decode_fetch_page(body).is_err());

        let sealed = b64(&[3]);
        let repeated = serde_json::to_vec(&serde_json::json!({
            "envelopes": [
                {"id": 2, "msg_id": encoded_msg_id, "hop_ttl": 7,
                    "recipient_hint": encoded_hint, "sealed": sealed, "expiry_ms": 9},
                {"id": 2, "msg_id": encoded_msg_id, "hop_ttl": 7,
                    "recipient_hint": encoded_hint, "sealed": sealed, "expiry_ms": 9}
            ],
            "next_cursor": 2
        }))
        .unwrap();
        assert!(relay_decode_fetch_page(repeated).is_err());
    }

    #[test]
    fn rejects_response_body_above_transport_cap_before_json_decode() {
        let body = vec![b' '; RELAY_MAX_RESPONSE_BODY_BYTES + 1];
        assert!(relay_decode_fetch_page(body).is_err());
    }

    // -- contact_delivery (honest reachability) -------------------------

    const OWN_URL: &str = "https://relay.example";
    const OWN_TOKEN: &str = "member-token-aaaa";

    #[test]
    fn no_card_relay_means_nearby_only() {
        assert_eq!(
            contact_delivery(None, None, Some(OWN_URL.into()), Some(OWN_TOKEN.into())),
            ContactDelivery::NearbyOnly
        );
        // A url with no token is not a usable endpoint either.
        assert_eq!(
            contact_delivery(
                Some(OWN_URL.into()),
                Some("   ".into()),
                Some(OWN_URL.into()),
                Some(OWN_TOKEN.into())
            ),
            ContactDelivery::NearbyOnly
        );
    }

    #[test]
    fn current_card_from_our_own_family_is_the_shared_mailbox() {
        let deposit = relay_deposit_token_for(OWN_TOKEN.to_string());
        assert_eq!(
            contact_delivery(
                Some(OWN_URL.into()),
                Some(deposit),
                Some(OWN_URL.into()),
                Some(OWN_TOKEN.into())
            ),
            ContactDelivery::SharedMailbox
        );
    }

    #[test]
    fn legacy_full_token_card_from_our_own_family_still_reads_as_shared() {
        assert_eq!(
            contact_delivery(
                Some(OWN_URL.into()),
                Some(OWN_TOKEN.into()),
                Some(OWN_URL.into()),
                Some(OWN_TOKEN.into())
            ),
            ContactDelivery::SharedMailbox
        );
    }

    #[test]
    fn another_familys_card_is_their_own_mailbox_and_exposes_only_the_host() {
        let theirs = relay_deposit_token_for("member-token-bbbb".to_string());
        let got = contact_delivery(
            Some("https://relay.example/".into()),
            Some(theirs.clone()),
            Some("https://other.example".into()),
            Some(OWN_TOKEN.into()),
        );
        match got {
            ContactDelivery::OwnMailbox { host } => {
                assert_eq!(host, "relay.example");
                assert!(
                    !host.contains(&theirs),
                    "host must never carry a credential"
                );
            }
            other => panic!("expected OwnMailbox, got {other:?}"),
        }
    }

    #[test]
    fn same_host_different_family_is_not_the_shared_mailbox() {
        // Two families hosted on the same relay must not be conflated: the
        // credential decides, not the hostname.
        let theirs = relay_deposit_token_for("member-token-cccc".to_string());
        assert!(matches!(
            contact_delivery(
                Some(OWN_URL.into()),
                Some(theirs),
                Some(OWN_URL.into()),
                Some(OWN_TOKEN.into())
            ),
            ContactDelivery::OwnMailbox { .. }
        ));
    }

    #[test]
    fn a_contact_can_have_delivery_when_we_have_none() {
        // We have no pass; they shared one. We can still post to them.
        assert!(matches!(
            contact_delivery(
                Some(OWN_URL.into()),
                Some("cmdep1-whatever".into()),
                None,
                None
            ),
            ContactDelivery::OwnMailbox { .. }
        ));
    }

    #[test]
    fn host_strips_scheme_path_and_port_is_kept() {
        assert_eq!(
            relay_host_only("https://relay.example:8443/api"),
            "relay.example:8443"
        );
        assert_eq!(relay_host_only("relay.example"), "relay.example");
    }

    // -- composer_reach (say it where the person is typing) --------------

    const THEIR_MAILBOX: ContactDelivery = ContactDelivery::SharedMailbox;

    #[test]
    fn no_pass_of_our_own_means_replies_have_nowhere_to_land() {
        // The Leanne case: she reaches David with his card, he replies, and
        // his replies cannot arrive. Her phone knows this locally.
        assert_eq!(
            composer_reach(THEIR_MAILBOX, false, false, false),
            ComposerReach::RepliesCannotReachMe
        );
    }

    #[test]
    fn a_contact_with_no_mailbox_cannot_be_reached_from_a_pass_holder() {
        assert_eq!(
            composer_reach(ContactDelivery::NearbyOnly, true, false, false),
            ComposerReach::TheyCannotBeReached
        );
    }

    #[test]
    fn two_phones_without_passes_reach_each_other_only_in_person() {
        assert_eq!(
            composer_reach(ContactDelivery::NearbyOnly, false, false, false),
            ComposerReach::NeitherDirectionWorks
        );
    }

    #[test]
    fn both_ends_holding_a_mailbox_says_nothing() {
        assert_eq!(
            composer_reach(THEIR_MAILBOX, true, false, false),
            ComposerReach::Fine
        );
    }

    #[test]
    fn a_contact_who_is_nearby_right_now_says_nothing() {
        // Every broken-path combination is silent while a direct link exists:
        // that path works, and it is the one a send would take.
        for delivery in [
            ContactDelivery::NearbyOnly,
            THEIR_MAILBOX,
            ContactDelivery::OwnMailbox {
                host: "relay.example".into(),
            },
        ] {
            for own_pass in [false, true] {
                assert_eq!(
                    composer_reach(delivery.clone(), own_pass, true, false),
                    ComposerReach::Fine
                );
            }
        }
    }

    #[test]
    fn meeting_in_person_keeps_the_composer_quiet_afterwards() {
        // Adding someone while standing next to them means nearby delivery
        // was the deal; nagging about it later is noise, not news.
        assert_eq!(
            composer_reach(ContactDelivery::NearbyOnly, false, false, true),
            ComposerReach::Fine
        );
        assert_eq!(
            composer_reach(THEIR_MAILBOX, false, false, true),
            ComposerReach::Fine
        );
    }

    fn some(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn a_usable_card_endpoint_routes_exactly_as_before() {
        let usable = resolved_contact_delivery_relay(
            some("https://theirs.example"),
            some("their-token"),
            some("https://ours.example"),
            some("our-token"),
            true,
        )
        .unwrap();
        assert_eq!(usable.url, "https://theirs.example");
        assert_eq!(usable.token, "their-token");
    }

    #[test]
    fn a_written_off_card_endpoint_falls_back_to_our_own() {
        // The whole point: one dead field must stop beating a working
        // alternative forever.
        let routed = resolved_contact_delivery_relay(
            some("https://dead.example"),
            some("their-token"),
            some("https://ours.example"),
            some("our-token"),
            false,
        )
        .unwrap();
        assert_eq!(routed.url, "https://ours.example");
        assert_eq!(routed.token, "our-token");
    }

    #[test]
    fn a_written_off_endpoint_with_no_alternative_posts_nowhere() {
        assert_eq!(
            resolved_contact_delivery_relay(
                some("https://dead.example"),
                some("their-token"),
                None,
                None,
                false,
            ),
            None
        );
    }

    #[test]
    fn falling_back_never_re_posts_to_the_host_we_just_wrote_off() {
        // Same host, different credential (a family member's card): retrying
        // it under our own token would be the same hammering with extra
        // steps.
        assert_eq!(
            resolved_contact_delivery_relay(
                some("https://same.example"),
                some("their-token"),
                some("https://same.example"),
                some("our-token"),
                false,
            ),
            None
        );
    }

    fn member(url: Option<String>, usable: bool, answering: bool) -> GroupRelayMember {
        GroupRelayMember {
            relay_url: url,
            relay_token: some("their-token"),
            endpoint_usable: usable,
            endpoint_answering: answering,
        }
    }

    #[test]
    fn a_group_posts_to_the_first_member_endpoint_that_is_worth_a_request() {
        // The first member carries no card endpoint, so they resolve to our
        // own mailbox and the walk stops there -- unchanged behaviour.
        let target = core_group_fanout_relay_target(
            vec![
                member(None, true, true),
                member(some("https://theirs.example"), true, true),
            ],
            some("https://ours.example"),
            some("our-token"),
        )
        .unwrap();
        assert_eq!(target.url, "https://ours.example");

        let target = core_group_fanout_relay_target(
            vec![member(some("https://theirs.example"), true, true)],
            some("https://ours.example"),
            some("our-token"),
        )
        .unwrap();
        assert_eq!(target.url, "https://theirs.example");
    }

    #[test]
    fn a_group_with_no_card_members_at_all_still_uses_our_own_mailbox() {
        let target =
            core_group_fanout_relay_target(vec![], some("https://ours.example"), some("our-token"))
                .unwrap();
        assert_eq!(target.url, "https://ours.example");
        assert_eq!(target.token, "our-token");
    }

    #[test]
    fn a_resting_member_never_redirects_the_group_to_our_own_mailbox() {
        // The bug this function exists to prevent. A member whose endpoint
        // had gone silent used to fall through to our own relay, where the
        // post succeeded and the envelope was marked relay-posted -- which is
        // terminal -- so their copy was never offered to the relay path
        // again. Posting nothing leaves it queued for a later pass and for
        // the BLE/LAN paths, so a host that comes back still receives it.
        assert_eq!(
            core_group_fanout_relay_target(
                vec![member(some("https://silent.example"), true, false)],
                some("https://ours.example"),
                some("our-token"),
            ),
            None
        );
    }

    #[test]
    fn a_rejected_member_still_falls_back_even_alongside_a_resting_one() {
        // The asymmetry, pinned: a 401 proves the card is wrong, so our own
        // mailbox is a real answer for that member and the group rides it.
        let target = core_group_fanout_relay_target(
            vec![
                member(some("https://silent.example"), true, false),
                member(some("https://revoked.example"), false, true),
            ],
            some("https://ours.example"),
            some("our-token"),
        )
        .unwrap();
        assert_eq!(target.url, "https://ours.example");
    }

    #[test]
    fn a_healthy_member_beside_a_resting_one_carries_the_whole_group() {
        let target = core_group_fanout_relay_target(
            vec![
                member(some("https://silent.example"), true, false),
                member(some("https://live.example"), true, true),
            ],
            some("https://ours.example"),
            some("our-token"),
        )
        .unwrap();
        assert_eq!(target.url, "https://live.example");
    }

    #[test]
    fn a_written_off_endpoint_drops_out_of_the_poll_set_without_falling_back() {
        // Polling our own mailbox under a contact's heading would fetch
        // nothing new -- it is already polled on its own account.
        assert_eq!(
            resolved_contact_delivery_poll_relay(
                some("https://dead.example"),
                some("their-token"),
                some("https://ours.example"),
                some("our-token"),
                false,
            ),
            None
        );
        assert!(resolved_contact_delivery_poll_relay(
            some("https://live.example"),
            some("their-token"),
            some("https://ours.example"),
            some("our-token"),
            true,
        )
        .is_some());
    }
}
