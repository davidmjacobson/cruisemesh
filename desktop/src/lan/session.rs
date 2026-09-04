use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{bail, Context, Result};
use cruisemesh_core::{
    core_lan_network_id_for_ipv4, core_own_capabilities, digest_through_lamport_for_sender,
    encode_digest, encode_envelope_frame, encode_hello, encode_hello2, encode_lan_endpoint,
    encode_transport_probe, own_identity_lan_peer, parse_frame, recent_hints_for, Contact,
    CoreInboundSource, CoreMeshRouterState, CoreOwnIdentityLanPeer, CoreTransport, Frame, Identity,
    LanEndpointContent, LanNoiseSession, MessageStore, CARRIED_SPRAY_BUDGET_BYTES,
    OWN_OUTBOUND_SPRAY_BUDGET_BYTES, OWN_RECEIPT_SPRAY_BUDGET_BYTES, RECEIPT_TYPE_DELIVERED,
    RECEIPT_TYPE_READ,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot, Mutex},
    time::timeout,
};

use crate::{lan::endpoint_cache::EndpointCache, mesh::inbound::InboundExecutor};

const MAX_NOISE_RECORD: usize = 65_535;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const RECEIPT_QUERY_LIMIT: u64 = 256;
type PeerSenders = HashMap<String, (Vec<u8>, mpsc::Sender<Vec<u8>>)>;

pub struct PeerHub {
    router: Arc<CoreMeshRouterState>,
    senders: Mutex<PeerSenders>,
}

impl PeerHub {
    pub fn new(identity: &Identity) -> Self {
        let router = Arc::new(CoreMeshRouterState::new());
        router.set_local_user_id(identity.user_id.clone());
        Self {
            router,
            senders: Mutex::new(HashMap::new()),
        }
    }

    async fn register(
        &self,
        address: &str,
        peer_user_id: Vec<u8>,
        sender: mpsc::Sender<Vec<u8>>,
    ) -> bool {
        let mut senders = self.senders.lock().await;
        if senders
            .values()
            .any(|(connected_user_id, _)| connected_user_id == &peer_user_id)
        {
            return false;
        }
        self.router
            .on_connected(address.to_string(), CoreTransport::Lan);
        self.router
            .on_hello(address.to_string(), peer_user_id.clone());
        senders.insert(address.to_string(), (peer_user_id, sender));
        true
    }

    async fn unregister(&self, address: &str) {
        self.senders.lock().await.remove(address);
        self.router.on_disconnected(address.to_string());
    }

    pub async fn broadcast_except(&self, excluded: &str, frame: Vec<u8>) {
        let senders: Vec<_> = self
            .senders
            .lock()
            .await
            .iter()
            .filter(|(address, _)| address.as_str() != excluded)
            .map(|(_, (_, sender))| sender.clone())
            .collect();
        for sender in senders {
            let _ = sender.send(frame.clone()).await;
        }
    }

    pub fn connected_peer_count(&self) -> usize {
        self.router.identified_routes().len()
    }

    pub fn connected_user_ids(&self) -> Vec<Vec<u8>> {
        self.router
            .identified_routes()
            .into_iter()
            .map(|route| route.user_id)
            .collect()
    }

    pub async fn send_to_peer(&self, peer_user_id: &[u8], frame: Vec<u8>) -> bool {
        let sender = self
            .senders
            .lock()
            .await
            .values()
            .find(|(user_id, _)| user_id == peer_user_id)
            .map(|(_, sender)| sender.clone());
        match sender {
            Some(sender) => sender.send(frame).await.is_ok(),
            None => false,
        }
    }
}

#[derive(Clone)]
pub struct SessionServices {
    pub identity: Identity,
    pub store: Arc<MessageStore>,
    pub inbound: InboundExecutor,
    pub hub: Arc<PeerHub>,
    pub endpoints: EndpointCache,
    pub instance_token: Vec<u8>,
    pub listen_port: Arc<AtomicU16>,
    pub relay_nudge: Arc<tokio::sync::Notify>,
}

pub async fn run_session(
    stream: TcpStream,
    initiator: bool,
    expected_peer: Option<Vec<u8>>,
    services: SessionServices,
) -> Result<Vec<u8>> {
    run_session_with_authenticated_signal(stream, initiator, expected_peer, services, None).await
}

pub(crate) async fn run_session_with_authenticated_signal(
    stream: TcpStream,
    initiator: bool,
    expected_peer: Option<Vec<u8>>,
    services: SessionServices,
    authenticated: Option<oneshot::Sender<Vec<u8>>>,
) -> Result<Vec<u8>> {
    let address = stream.peer_addr()?.to_string();
    let local_address = stream.local_addr()?;
    let (stream, noise, peer) = timeout(
        HANDSHAKE_TIMEOUT,
        authenticate(stream, initiator, expected_peer, &services),
    )
    .await
    .context("LAN Noise handshake timed out")??;
    let contact = match peer {
        LanPeer::Contact(contact) => contact,
        LanPeer::OwnDevice => return run_own_device_session(stream, noise, authenticated).await,
    };
    let peer_user_id = contact.user_id.clone();
    let (mut reader, mut writer) = stream.into_split();
    let (send, mut receive) = mpsc::channel::<Vec<u8>>(256);
    let registered = services
        .hub
        .register(&address, peer_user_id.clone(), send.clone())
        .await;
    if let Some(authenticated) = authenticated {
        let _ = authenticated.send(peer_user_id.clone());
    }
    if !registered {
        return Ok(peer_user_id);
    }

    let writer_noise = noise.clone();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = receive.recv().await {
            for record in writer_noise.encrypt_frame(frame)? {
                write_packet(&mut writer, &record).await?;
            }
        }
        Ok::<(), anyhow::Error>(())
    });

    send.send(encode_hello(services.identity.user_id.clone()))
        .await?;
    send.send(encode_hello2(
        services.identity.user_id.clone(),
        core_own_capabilities(),
    )?)
    .await?;
    let listen_port = services.listen_port.load(Ordering::Relaxed);
    if listen_port != 0 {
        if let Ok(endpoint) = encode_lan_endpoint(
            services.instance_token.clone(),
            local_address.ip().to_string(),
            listen_port,
        ) {
            send.send(endpoint).await?;
        }
    }
    send_digest(&send, &services, &peer_user_id).await?;

    let probe_sender = send.clone();
    let probe_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await;
        loop {
            interval.tick().await;
            if probe_sender
                .send(encode_transport_probe(rand::random(), false))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let result = read_frames(
        &mut reader,
        noise,
        &address,
        &local_address.ip().to_string(),
        &peer_user_id,
        &send,
        &services,
    )
    .await;
    services.hub.unregister(&address).await;
    drop(send);
    probe_task.abort();
    writer_task.abort();
    result.map(|_| peer_user_id)
}

/// Keep a link to one of this person's own devices open, without treating it
/// as a peer (`specs/multi-device-v1.md` §10 step 5).
///
/// A sibling shares this person's user id, so a route to it is a route back to
/// this node: it is deliberately not registered with [`PeerHub`], is sent no
/// HELLO, no digest and no endpoint, and has none of its frames processed.
/// That is also what keeps both standing invariants trivially safe on this arm
/// — nothing here can ack a relay copy, and no endpoint of anyone's is recorded
/// or forwarded. Transport probes are answered so the far end can see the link
/// is alive; every other frame is dropped, because the one frame a sibling link
/// exists to carry is the own-roster notice, and this node has no own roster to
/// converge yet (see the `Frame::OwnRoster` arm below).
///
/// The empty user id it reports is what a sibling has: no user id at all.
/// Discovery reads it as "this address answered", which is what it proves — the
/// phones credit a sweep for a sibling the same way and for the same reason.
async fn run_own_device_session(
    stream: TcpStream,
    noise: Arc<LanNoiseSession>,
    authenticated: Option<oneshot::Sender<Vec<u8>>>,
) -> Result<Vec<u8>> {
    if let Some(authenticated) = authenticated {
        let _ = authenticated.send(Vec::new());
    }
    let (mut reader, mut writer) = stream.into_split();
    loop {
        let record = read_packet(&mut reader).await?;
        let Some(bytes) = noise.decrypt_record(record)? else {
            continue;
        };
        if let Frame::TransportProbe {
            nonce,
            response: false,
        } = parse_frame(bytes)?
        {
            for record in noise.encrypt_frame(encode_transport_probe(nonce, true))? {
                write_packet(&mut writer, &record).await?;
            }
        }
    }
}

async fn authenticate(
    mut stream: TcpStream,
    initiator: bool,
    expected_peer: Option<Vec<u8>>,
    services: &SessionServices,
) -> Result<(TcpStream, Arc<LanNoiseSession>, LanPeer)> {
    let noise = Arc::new(LanNoiseSession::new(
        initiator,
        services.identity.agree_sk.clone(),
    )?);
    if initiator {
        let message = noise.write_handshake_message()?;
        write_packet(&mut stream, &message).await?;
        noise.read_handshake_message(read_packet(&mut stream).await?)?;
        let peer = admit_peer(
            &services.store,
            &services.identity,
            noise.remote_static_key(),
            expected_peer,
            proven_own_device_id(),
        )?;
        let message = noise.write_handshake_message()?;
        write_packet(&mut stream, &message).await?;
        Ok((stream, noise, peer))
    } else {
        noise.read_handshake_message(read_packet(&mut stream).await?)?;
        let message = noise.write_handshake_message()?;
        write_packet(&mut stream, &message).await?;
        noise.read_handshake_message(read_packet(&mut stream).await?)?;
        let peer = admit_peer(
            &services.store,
            &services.identity,
            noise.remote_static_key(),
            expected_peer,
            proven_own_device_id(),
        )?;
        Ok((stream, noise, peer))
    }
}

/// What a finished handshake's far end turned out to be, and the only two
/// things this node will carry a session for.
enum LanPeer {
    /// An accepted contact: a peer, with a user id, a route and a digest.
    Contact(Contact),
    /// One of this person's own devices (`specs/multi-device-v1.md` §10 step
    /// 5). Admitted, but deliberately not a peer — see
    /// [`run_own_device_session`].
    OwnDevice,
}

/// The device id §10 step 5's own-device proof named on this session, or `None`
/// when no proof was opened.
///
/// `None` always, today, and not as a shortcut: minting a proof takes a device
/// signing key and opening one takes an own roster, and this node has neither
/// until it links as a device of this person's. That is the same work-package
/// the `Frame::OwnRoster` arm is waiting on, and it is where an opened proof
/// will arrive from — [`admit_peer`] needs no change when it does.
///
/// The phones are in the same position by a different route: their clone arm is
/// symmetric and exchanges no proof frame, so they too hand the rule `None`.
/// Until one end can prove itself, every peer presenting this identity's key is
/// a clone, which is exactly what the rule should say about a peer that cannot
/// name itself.
fn proven_own_device_id() -> Option<Vec<u8>> {
    None
}

/// Decide what a finished Noise handshake is talking to: an accepted contact, a
/// device of this person's own, or nothing this node will carry.
///
/// The own-identity arm used to be a bare `bail!` with an unconditional clone
/// warning — the same test the phones ran before they had a person/device
/// split, and the wrong one once a person can have several devices: §6 makes
/// the inbox key person-scoped, so a deliberately linked sibling can hold the
/// very key that test reads as a clone. The verdict now comes from core's
/// [`own_identity_lan_peer`], which both phone shells' own rule mirrors, so a
/// sibling is admitted in silence and a clone is refused and recorded exactly
/// as before.
fn admit_peer(
    store: &MessageStore,
    identity: &Identity,
    remote_static: Option<Vec<u8>>,
    expected_peer: Option<Vec<u8>>,
    proven_peer_device_id: Option<Vec<u8>>,
) -> Result<LanPeer> {
    let remote_static = remote_static.context("Noise did not reveal the remote static key")?;
    match own_identity_lan_peer(
        &identity.agree_pk,
        &remote_static,
        // Read only once the key test has passed, and `None` when it cannot be
        // read at all: an unreadable projection must fail loud about a peer
        // holding this identity, never about a stranger on the same Wi-Fi.
        || store.own_device_fleet().ok(),
        proven_peer_device_id,
    ) {
        CoreOwnIdentityLanPeer::NotOurIdentity => {}
        CoreOwnIdentityLanPeer::Sibling => return Ok(LanPeer::OwnDevice),
        CoreOwnIdentityLanPeer::Clone => {
            let _ = store.record_identity_clone_warning(identity.user_id.clone(), now_ms());
            bail!("Noise static key is this device's own identity");
        }
    }
    let contact = store
        .list_contacts()?
        .into_iter()
        .find(|contact| contact.agree_pk == remote_static)
        .context("Noise static key is not an accepted contact")?;
    if expected_peer.is_some_and(|expected| expected != contact.user_id) {
        bail!("discovered endpoint authenticated as a different contact");
    }
    Ok(LanPeer::Contact(contact))
}

async fn read_frames<R: AsyncRead + Unpin>(
    reader: &mut R,
    noise: Arc<LanNoiseSession>,
    address: &str,
    local_host: &str,
    peer_user_id: &[u8],
    send: &mpsc::Sender<Vec<u8>>,
    services: &SessionServices,
) -> Result<()> {
    loop {
        let record = read_packet(reader).await?;
        let Some(bytes) = noise.decrypt_record(record)? else {
            continue;
        };
        match parse_frame(bytes.clone())? {
            Frame::Hello { user_id } => {
                if user_id != peer_user_id {
                    bail!("HELLO identity conflicts with authenticated Noise key");
                }
                if !services.hub.router.on_hello(address.to_string(), user_id) {
                    bail!("HELLO changed identity on a live link");
                }
            }
            Frame::Hello2 {
                user_id,
                capabilities,
            } => {
                if user_id != peer_user_id
                    || !services
                        .hub
                        .router
                        .on_hello2(address.to_string(), user_id, capabilities)
                {
                    bail!("HELLO2 conflicts with authenticated Noise key");
                }
            }
            Frame::Envelope { .. } => {
                let processed = services
                    .inbound
                    .process(CoreInboundSource::Mesh, bytes, now_ms())
                    .await?;
                if let Some(relay) = processed.relay_frame {
                    services.hub.broadcast_except(address, relay).await;
                }
                if processed.carried
                    || processed.disposition == cruisemesh_core::CoreInboundDisposition::Consumed
                {
                    services.relay_nudge.notify_one();
                }
            }
            Frame::Digest {
                chat_id,
                entries,
                recent_msg_ids,
            } => {
                if chat_id != peer_user_id {
                    bail!("DIGEST identity does not match authenticated peer");
                }
                respond_to_digest(
                    address,
                    peer_user_id,
                    entries,
                    recent_msg_ids,
                    send,
                    services,
                )
                .await?;
            }
            Frame::TransportProbe { nonce, response } if !response => {
                send.send(encode_transport_probe(nonce, true)).await?;
            }
            Frame::LanEndpoint {
                instance_token,
                host,
                port,
            } => {
                if !cruisemesh_core::lan_hosts_share_local_network(
                    local_host.to_owned(),
                    host.clone(),
                ) {
                    continue;
                }
                let network_id = match local_network_id(local_host) {
                    Some(network_id) => network_id,
                    None => continue,
                };
                let _ = services.endpoints.record(
                    peer_user_id.to_vec(),
                    LanEndpointContent {
                        instance_token,
                        network_id,
                        host,
                        port,
                        expires_at_ms: now_ms().saturating_add(7 * 24 * 60 * 60 * 1_000),
                    },
                    now_ms(),
                )?;
            }
            Frame::TransportProbe { .. } => {}
            // §10 step 5's own-roster notice. The desktop is not a linked
            // device yet (WP7): it has no own roster to converge, and no person
            // identity of its own to run the sender test `encode_own_roster`
            // requires. Dropped rather than half-handled, because applying a
            // roster without that test is the one thing that frame forbids.
            Frame::OwnRoster { .. } => {}
        }
    }
}

async fn send_digest(
    send: &mpsc::Sender<Vec<u8>>,
    services: &SessionServices,
    peer_user_id: &[u8],
) -> Result<()> {
    let entries = services.store.chat_digest(peer_user_id.to_vec())?;
    let known = services.store.core_digest_advertised_msg_ids()?;
    send.send(encode_digest(
        services.identity.user_id.clone(),
        entries,
        known,
    )?)
    .await?;
    Ok(())
}

async fn respond_to_digest(
    address: &str,
    peer_user_id: &[u8],
    entries: Vec<cruisemesh_core::DigestEntry>,
    recent_msg_ids: Vec<Vec<u8>>,
    send: &mpsc::Sender<Vec<u8>>,
    services: &SessionServices,
) -> Result<()> {
    let now = now_ms();
    services.store.core_confirm_carried_deliveries(
        peer_user_id.to_vec(),
        recent_msg_ids.clone(),
        true,
        now,
    )?;

    let through = digest_through_lamport_for_sender(entries, services.identity.user_id.clone());
    for envelope in services.store.outbound_envelopes_after(
        peer_user_id.to_vec(),
        services.identity.user_id.clone(),
        through,
    )? {
        send.send(frame_outbound(envelope)).await?;
    }
    for receipt_type in [RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ] {
        if let Some(envelope) = services.store.outgoing_receipt_envelope(
            peer_user_id.to_vec(),
            peer_user_id.to_vec(),
            receipt_type,
        )? {
            send.send(frame_receipt(envelope)).await?;
        }
    }

    let lane = services
        .hub
        .router
        .carried_lane_for(address.to_string(), now);
    let plan = services.store.core_digest_spray_plan(
        services.identity.user_id.clone(),
        peer_user_id.to_vec(),
        recent_hints_for(peer_user_id.to_vec(), now),
        recent_msg_ids,
        now,
        if lane.skip {
            0
        } else {
            CARRIED_SPRAY_BUDGET_BYTES
        },
        OWN_OUTBOUND_SPRAY_BUDGET_BYTES,
        OWN_RECEIPT_SPRAY_BUDGET_BYTES,
        RECEIPT_QUERY_LIMIT,
        services
            .hub
            .router
            .peer_acked_hidden_kinds(address.to_string()),
        services.hub.router.hidden_offered_for(address.to_string()),
        lane.after,
    )?;
    for frame in plan
        .carried_frames
        .iter()
        .chain(plan.own_outbound_frames.iter())
        .chain(plan.own_receipt_frames.iter())
    {
        send.send(frame.clone()).await?;
    }
    services
        .hub
        .router
        .record_hidden_offered(address.to_string(), plan.offered_hidden_msg_ids);
    services.hub.router.record_carried_progress(
        address.to_string(),
        plan.next_carried_cursor,
        plan.carried_exhausted,
        now,
    );
    Ok(())
}

fn frame_outbound(envelope: cruisemesh_core::OutboundEnvelope) -> Vec<u8> {
    encode_envelope_frame(
        envelope.msg_id,
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint,
        envelope.sealed,
    )
}

fn frame_receipt(envelope: cruisemesh_core::OutgoingReceiptEnvelope) -> Vec<u8> {
    encode_envelope_frame(
        envelope.msg_id,
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint,
        envelope.sealed,
    )
}

async fn read_packet<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_NOISE_RECORD {
        bail!("invalid LAN record length {length}");
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn write_packet<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_NOISE_RECORD {
        bail!("invalid outbound LAN record length {}", bytes.len());
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn local_network_id(host: &str) -> Option<Vec<u8>> {
    core_lan_network_id_for_ipv4(host.to_owned()).map(String::into_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cruisemesh_core::{generate_identity, OwnDeviceFleet, RosterVersion, DEVICE_ID_LEN};

    #[tokio::test]
    async fn packet_framing_is_big_endian_and_bounded() {
        let (mut left, mut right) = tokio::io::duplex(128);
        let writing = tokio::spawn(async move { write_packet(&mut left, b"noise").await });
        assert_eq!(read_packet(&mut right).await.unwrap(), b"noise");
        writing.await.unwrap().unwrap();
    }

    #[test]
    fn trust_gate_matches_only_an_accepted_static_key() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let peer = generate_identity();
        store
            .upsert_contact(Contact {
                user_id: peer.user_id.clone(),
                name: "Emma".into(),
                sign_pk: peer.sign_pk.clone(),
                agree_pk: peer.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .unwrap();
        let us = generate_identity();
        assert_eq!(
            contact_of(admit_peer(&store, &us, Some(peer.agree_pk), None, None).unwrap()).user_id,
            peer.user_id
        );
        assert!(admit_peer(&store, &us, Some(vec![9; 32]), None, None).is_err());
        assert!(admit_peer(&store, &us, Some(us.agree_pk.clone()), None, None).is_err());
        assert!(store
            .has_identity_clone_warning(us.user_id.clone())
            .unwrap());
    }

    fn contact_of(peer: LanPeer) -> Contact {
        match peer {
            LanPeer::Contact(contact) => contact,
            LanPeer::OwnDevice => panic!("expected an accepted contact"),
        }
    }

    fn linked_fleet(own: &[u8], sibling: &[u8]) -> OwnDeviceFleet {
        OwnDeviceFleet {
            own_device_id: Some(own.to_vec()),
            device_ids: vec![own.to_vec(), sibling.to_vec()],
            projected_from: RosterVersion {
                recovery_epoch: 0,
                seq: 1,
            },
        }
    }

    /// A device this person's own roster names, which proved itself on this
    /// session, is admitted as a sibling: no clone warning, and no refusal.
    ///
    /// The proof stands in for what §10 step 5 will hand [`admit_peer`] once
    /// this node links as a device — [`proven_own_device_id`] returns `None`
    /// until then, so nothing on today's wire reaches this arm. What is pinned
    /// here is the decision, which is core's, and the admission it drives.
    #[test]
    fn a_proven_sibling_is_admitted_and_never_warned_about() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let us = generate_identity();
        let own = vec![0x01; DEVICE_ID_LEN];
        let sibling = vec![0x02; DEVICE_ID_LEN];
        store
            .set_own_device_fleet(linked_fleet(&own, &sibling))
            .unwrap();

        let peer = admit_peer(
            &store,
            &us,
            // A sibling holds the person-scoped inbox key -- generation 0 of
            // which is this identity's own agreement key -- so it presents the
            // very key the old bare test read as a clone.
            Some(us.agree_pk.clone()),
            None,
            Some(sibling),
        )
        .unwrap();
        assert!(matches!(peer, LanPeer::OwnDevice));
        assert!(
            !store
                .has_identity_clone_warning(us.user_id.clone())
                .unwrap(),
            "a device this person linked on purpose must not be reported as a clone"
        );
    }

    /// The case the guard exists for, unchanged: this identity on a device the
    /// roster cannot name is refused, and the warning is recorded.
    #[test]
    fn a_clone_is_still_refused_and_recorded_on_a_linked_node() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let us = generate_identity();
        let own = vec![0x01; DEVICE_ID_LEN];
        let sibling = vec![0x02; DEVICE_ID_LEN];
        store
            .set_own_device_fleet(linked_fleet(&own, &sibling))
            .unwrap();

        // A peer this identity's roster never heard of.
        assert!(admit_peer(
            &store,
            &us,
            Some(us.agree_pk.clone()),
            None,
            Some(vec![0x03; DEVICE_ID_LEN]),
        )
        .is_err());
        assert!(store
            .has_identity_clone_warning(us.user_id.clone())
            .unwrap());

        // And a peer that proved nothing at all -- which is every peer on
        // today's wire, a restored `.cmbak` running beside its source included.
        let store = MessageStore::open(":memory:".into()).unwrap();
        store
            .set_own_device_fleet(linked_fleet(&own, &sibling))
            .unwrap();
        assert!(admit_peer(&store, &us, Some(us.agree_pk.clone()), None, None).is_err());
        assert!(store
            .has_identity_clone_warning(us.user_id.clone())
            .unwrap());
    }
}
