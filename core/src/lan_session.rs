//! Authenticated encrypted stream sessions for the same-LAN transport.
//!
//! TCP and Bonjour only provide reachability. Before any CruiseMesh HELLO,
//! DIGEST, or envelope frame crosses a LAN socket, both peers complete a
//! Noise XX handshake using the X25519 agreement keys already exchanged in
//! their friend cards. The remote Noise static key must match an accepted
//! contact before the platform shell promotes the socket to a mesh link.
//!
//! After the handshake, ordinary CruiseMesh protocol frames are split into
//! bounded Noise transport records. TCP framing itself stays in the native
//! shells: each handshake message or encrypted record is prefixed with a
//! four-byte big-endian length. Keeping Noise and record reassembly in Rust
//! makes the security-sensitive stream behavior identical on Android and iOS.

use snow::{Builder, HandshakeState, TransportState};
use std::sync::Mutex;

use crate::CoreError;

/// Provisional default CruiseMesh TCP port. Bonjour advertises the actual
/// bound port, allowing a platform shell to fall back if this port is already
/// occupied locally.
pub const LAN_DEFAULT_TCP_PORT: u16 = 45_892;

/// DNS-SD service type shared by Android NSD and Apple Bonjour.
pub const LAN_SERVICE_TYPE: &str = "_cruisemesh._tcp.";

/// Hard ceiling for one decrypted CruiseMesh protocol frame over the LAN.
/// Current inline attachments are below 200 KiB; this leaves ample headroom
/// while bounding memory use from a trusted-but-buggy peer.
pub const LAN_MAX_FRAME_SIZE: u64 = 1024 * 1024;

const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const NOISE_PROLOGUE: &[u8] = b"CruiseMesh same-LAN transport v1";
const NOISE_MAX_MESSAGE_SIZE: usize = 65_535;
const NOISE_TAG_SIZE: usize = 16;
const RECORD_HEADER_SIZE: usize = 9;
/// The two record types the LAN link multiplexes: ordinary CruiseMesh
/// protocol frames and the blob plane's pull frames. Each type has its own
/// frame-id space and its own reassembler, so a transfer in progress never
/// disturbs the message plane — see specs/media-two-plane.md, "LAN
/// sub-channel: multiplexing".
const RECORD_TYPE_FRAME: u8 = 1;
const RECORD_TYPE_BLOB: u8 = 2;
const RECORD_PLAINTEXT_SIZE: usize = 60 * 1024;
const RECORD_CHUNK_SIZE: usize = RECORD_PLAINTEXT_SIZE - RECORD_HEADER_SIZE;

#[uniffi::export]
pub fn lan_default_tcp_port() -> u16 {
    LAN_DEFAULT_TCP_PORT
}

#[uniffi::export]
pub fn lan_service_type() -> String {
    LAN_SERVICE_TYPE.to_string()
}

#[uniffi::export]
pub fn lan_max_frame_size() -> u64 {
    LAN_MAX_FRAME_SIZE
}

/// Send-queue ordering for one outbound frame: lower goes first. Core holds
/// no socket, so the shells own the queue; what core owns is the rule that
/// mesh frames outrank blob frames, so a bulk transfer never delays the
/// message plane sharing the link.
///
/// This orders frames *waiting to be encrypted*. A Noise transport nonce is
/// implicit and sequential, so records must reach the wire in the order
/// [`LanNoiseSession::encrypt_frame`] and
/// [`LanNoiseSession::encrypt_blob_record`] produced them — already-encrypted
/// records must never be reordered. Head-of-line delay for a mesh frame is
/// therefore bounded by one blob frame's records, not by a whole transfer.
#[uniffi::export]
pub fn lan_record_priority(record_type: u8) -> u8 {
    match record_type {
        RECORD_TYPE_FRAME => 0,
        RECORD_TYPE_BLOB => 1,
        _ => 2,
    }
}

/// One decrypted record's plane and payload. `record_type` is
/// [`RECORD_TYPE_FRAME`] for a mesh protocol frame or [`RECORD_TYPE_BLOB`]
/// for an encoded blob-plane pull frame.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct LanRecord {
    pub record_type: u8,
    pub frame: Vec<u8>,
}

#[derive(uniffi::Object)]
pub struct LanNoiseSession {
    inner: Mutex<SessionInner>,
}

struct SessionInner {
    handshake: Option<HandshakeState>,
    transport: Option<TransportState>,
    remote_static: Option<Vec<u8>>,
    /// Noise's own transcript hash, captured the instant the handshake
    /// finishes. See [`LanNoiseSession::handshake_hash`] for why it is taken
    /// here rather than asked for later: `snow` exposes it on the handshake
    /// state, and `promote_if_finished` consumes that state.
    handshake_hash: Option<Vec<u8>>,
    next_outbound_frame_id: u32,
    next_outbound_blob_frame_id: u32,
    inbound: Option<InboundFrame>,
    inbound_blob: Option<InboundFrame>,
    /// Whether this peer advertised `CAP_MEDIA_BLOB` in its HELLO2. False
    /// until a shell says otherwise — see [`LanNoiseSession::set_peer_capabilities`].
    peer_speaks_blob_plane: bool,
}

struct InboundFrame {
    frame_id: u32,
    total: u16,
    next_index: u16,
    bytes: Vec<u8>,
}

#[uniffi::export]
impl LanNoiseSession {
    /// Create one side of a Noise XX connection using this device's existing
    /// 32-byte X25519 agreement private key.
    #[uniffi::constructor]
    pub fn new(initiator: bool, local_private_key: Vec<u8>) -> Result<Self, CoreError> {
        if local_private_key.len() != 32 {
            return Err(CoreError::InvalidKeyLength {
                expected: 32,
                actual: local_private_key.len() as u32,
            });
        }
        let params = NOISE_PARAMS
            .parse()
            .map_err(|error| CoreError::Crypto(format!("invalid LAN Noise parameters: {error}")))?;
        let builder = Builder::new(params)
            .prologue(NOISE_PROLOGUE)
            .map_err(noise_error)?
            .local_private_key(&local_private_key)
            .map_err(noise_error)?;
        let handshake = if initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(noise_error)?;

        Ok(Self {
            inner: Mutex::new(SessionInner {
                handshake: Some(handshake),
                transport: None,
                remote_static: None,
                handshake_hash: None,
                next_outbound_frame_id: 0,
                next_outbound_blob_frame_id: 0,
                inbound: None,
                inbound_blob: None,
                peer_speaks_blob_plane: false,
            }),
        })
    }

    /// Produce the next Noise XX handshake message. Callers follow the
    /// standard XX sequence: initiator write, responder write, initiator
    /// write, with the opposite side reading after each step.
    pub fn write_handshake_message(&self) -> Result<Vec<u8>, CoreError> {
        let mut inner = self.lock()?;
        let handshake = inner
            .handshake
            .as_mut()
            .ok_or_else(|| CoreError::Crypto("LAN handshake is already complete".to_string()))?;
        let mut output = vec![0u8; NOISE_MAX_MESSAGE_SIZE];
        let written = handshake
            .write_message(&[], &mut output)
            .map_err(noise_error)?;
        output.truncate(written);
        promote_if_finished(&mut inner)?;
        Ok(output)
    }

    /// Consume the next Noise XX handshake message. CruiseMesh does not put
    /// application data in handshake payloads; non-empty payloads fail closed.
    pub fn read_handshake_message(&self, message: Vec<u8>) -> Result<(), CoreError> {
        if message.len() > NOISE_MAX_MESSAGE_SIZE {
            return Err(CoreError::Malformed(
                "LAN handshake record is too large".to_string(),
            ));
        }
        let mut inner = self.lock()?;
        let handshake = inner
            .handshake
            .as_mut()
            .ok_or_else(|| CoreError::Crypto("LAN handshake is already complete".to_string()))?;
        let mut payload = vec![0u8; NOISE_MAX_MESSAGE_SIZE];
        let read = handshake
            .read_message(&message, &mut payload)
            .map_err(noise_error)?;
        if read != 0 {
            return Err(CoreError::Malformed(
                "LAN handshake carried unexpected application data".to_string(),
            ));
        }
        promote_if_finished(&mut inner)?;
        Ok(())
    }

    pub fn is_handshake_finished(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.transport.is_some())
            .unwrap_or(false)
    }

    /// The remote X25519 static public key once Noise has revealed it.
    /// Initiators can inspect this after reading message 2 and must reject an
    /// unknown key before sending message 3. Responders learn it after message
    /// 3, immediately before the session enters transport mode.
    pub fn remote_static_key(&self) -> Option<Vec<u8>> {
        let inner = self.inner.lock().ok()?;
        inner.remote_static.clone().or_else(|| {
            inner
                .handshake
                .as_ref()
                .and_then(|handshake| handshake.get_remote_static())
                .map(ToOwned::to_owned)
        })
    }

    /// Noise's transcript hash for this session, once the handshake has
    /// finished. `None` before that.
    ///
    /// This is the value a proof-of-siblinghood signature is bound to
    /// (`specs/multi-device-v1.md` §10 step 5). Both ends of one handshake
    /// compute the identical hash, and it commits to both ephemeral keys and
    /// both static keys — so a signature over it is worthless on any other
    /// session, cannot be replayed from a recorded one, and cannot be relayed
    /// by a machine in the middle: it only verifies on the session whose
    /// transcript it names.
    pub fn handshake_hash(&self) -> Option<Vec<u8>> {
        self.inner.lock().ok()?.handshake_hash.clone()
    }

    /// Encrypt one complete CruiseMesh protocol frame into one or more Noise
    /// records. Native shells prefix each returned record with a u32 BE length
    /// before writing it to TCP.
    pub fn encrypt_frame(&self, frame: Vec<u8>) -> Result<Vec<Vec<u8>>, CoreError> {
        self.encrypt_typed(RECORD_TYPE_FRAME, frame)
    }

    /// Encrypt one encoded blob-plane pull frame into blob records. Records of
    /// the two types may interleave freely on the wire; the shell's send queue
    /// keeps mesh records ahead of these (see [`lan_record_priority`]).
    pub fn encrypt_blob_record(&self, frame: Vec<u8>) -> Result<Vec<Vec<u8>>, CoreError> {
        self.encrypt_typed(RECORD_TYPE_BLOB, frame)
    }

    /// Decrypt one Noise record. Returns a complete CruiseMesh protocol frame
    /// after the final record, or `None` while a multi-record frame is still
    /// being assembled. This is the message plane only: a blob record is
    /// skipped the way any unknown record type is — `Ok(None)`, so the read
    /// loop drops it and keeps the link, which is exactly how a peer without
    /// the blob plane behaves.
    pub fn decrypt_record(&self, record: Vec<u8>) -> Result<Option<Vec<u8>>, CoreError> {
        Ok(self
            .decrypt_typed(record, false)?
            .map(|complete| complete.frame))
    }

    /// Decrypt one Noise record from either plane, naming the plane a
    /// completed frame came from.
    ///
    /// The blob lane opens only for a peer that advertised `CAP_MEDIA_BLOB`
    /// (see [`Self::set_peer_capabilities`]); a blob record from anyone else
    /// is skipped the way an unknown record type is.
    pub fn decrypt_record_typed(&self, record: Vec<u8>) -> Result<Option<LanRecord>, CoreError> {
        let accept_blob = self.lock()?.peer_speaks_blob_plane;
        self.decrypt_typed(record, accept_blob)
    }

    /// Record what the peer advertised in its HELLO2, once the shell has read
    /// it. Until this is called the session accepts message-plane records
    /// only.
    ///
    /// Authenticating the link is not the same as agreeing to a second one.
    /// The blob lane carries its own reassembler with its own
    /// [`LAN_MAX_FRAME_SIZE`] buffer, so accepting blob records from any
    /// authenticated peer would let a contact who never claimed the blob plane
    /// open a second megabyte of reassembly on every link — capability for
    /// something the peer never said it does. `CAP_MEDIA_BLOB` is what says it
    /// does, and it is the same bit
    /// ([`crate::media::peer_speaks_blob_plane`]) the requester side already
    /// consults before it opens a pull session, so both directions agree on
    /// one gate.
    ///
    /// Idempotent, and safe to call again if a peer re-advertises: the flag
    /// tracks the latest HELLO2 rather than accumulating.
    pub fn set_peer_capabilities(&self, capabilities: u32) -> Result<(), CoreError> {
        self.lock()?.peer_speaks_blob_plane = crate::media::peer_speaks_blob_plane(capabilities);
        Ok(())
    }

    /// Whether this session will currently accept blob records.
    pub fn accepts_blob_records(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.peer_speaks_blob_plane)
            .unwrap_or(false)
    }
}

impl LanNoiseSession {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, SessionInner>, CoreError> {
        self.inner
            .lock()
            .map_err(|_| CoreError::Crypto("LAN session state is unavailable".to_string()))
    }

    fn encrypt_typed(&self, record_type: u8, frame: Vec<u8>) -> Result<Vec<Vec<u8>>, CoreError> {
        if frame.len() as u64 > LAN_MAX_FRAME_SIZE {
            return Err(CoreError::Malformed(format!(
                "LAN frame exceeds {} byte limit",
                LAN_MAX_FRAME_SIZE
            )));
        }
        let mut inner = self.lock()?;
        let counter = if record_type == RECORD_TYPE_BLOB {
            &mut inner.next_outbound_blob_frame_id
        } else {
            &mut inner.next_outbound_frame_id
        };
        let frame_id = *counter;
        *counter = counter.wrapping_add(1);
        let total = max_of_one(frame.len().div_ceil(RECORD_CHUNK_SIZE));
        let total_u16 = u16::try_from(total)
            .map_err(|_| CoreError::Malformed("LAN frame requires too many records".to_string()))?;
        let transport = inner
            .transport
            .as_mut()
            .ok_or_else(|| CoreError::Crypto("LAN Noise handshake is not complete".to_string()))?;

        let mut records = Vec::with_capacity(total);
        for index in 0..total {
            let start = index * RECORD_CHUNK_SIZE;
            let end = usize::min(start + RECORD_CHUNK_SIZE, frame.len());
            let mut plaintext = Vec::with_capacity(RECORD_HEADER_SIZE + end.saturating_sub(start));
            plaintext.push(record_type);
            plaintext.extend_from_slice(&frame_id.to_be_bytes());
            plaintext.extend_from_slice(&(index as u16).to_be_bytes());
            plaintext.extend_from_slice(&total_u16.to_be_bytes());
            plaintext.extend_from_slice(&frame[start..end]);

            let mut encrypted = vec![0u8; plaintext.len() + NOISE_TAG_SIZE];
            let written = transport
                .write_message(&plaintext, &mut encrypted)
                .map_err(noise_error)?;
            encrypted.truncate(written);
            records.push(encrypted);
        }
        Ok(records)
    }

    /// Decrypt one Noise record into its plane's reassembler. A record that
    /// fails to parse resets only the lane it names, so a malformed blob
    /// record can never abort an in-flight mesh frame.
    fn decrypt_typed(
        &self,
        record: Vec<u8>,
        accept_blob: bool,
    ) -> Result<Option<LanRecord>, CoreError> {
        if record.len() > NOISE_MAX_MESSAGE_SIZE {
            return Err(CoreError::Malformed(
                "LAN encrypted record is too large".to_string(),
            ));
        }
        let mut inner = self.lock()?;
        let transport = inner
            .transport
            .as_mut()
            .ok_or_else(|| CoreError::Crypto("LAN Noise handshake is not complete".to_string()))?;
        let mut plaintext = vec![0u8; record.len()];
        let read = transport
            .read_message(&record, &mut plaintext)
            .map_err(noise_error)?;
        plaintext.truncate(read);
        // A record too short to carry a header names no lane to reset, and is
        // structurally broken rather than merely unfamiliar: both reassemblers
        // are left alone and it stays an error.
        if plaintext.len() < RECORD_HEADER_SIZE {
            return Err(CoreError::Malformed(
                "invalid LAN transport record".to_string(),
            ));
        }
        let record_type = plaintext[0];
        match record_type {
            RECORD_TYPE_FRAME => {}
            RECORD_TYPE_BLOB if accept_blob => {}
            // A type this session does not speak is *skipped*, not failed. The
            // record is authenticated, so an unfamiliar type means a peer on a
            // newer build rather than an attacker, and the three ways to reach
            // here are exactly that: a blob record at a build without the blob
            // plane, a blob record from a peer that never advertised
            // `CAP_MEDIA_BLOB` (`accept_blob` is false, so it opens no lane),
            // and a type some later version allocates. An error here
            // would read as fatal to every shell's read loop — each one
            // propagates a decrypt failure and drops the socket — turning
            // "reject the record, keep the link" into a message-plane outage
            // that repeats on every reconnect. `None` is the answer the caller
            // already handles for a partial frame: drop it and read on.
            _ => return Ok(None),
        }
        let lane = if record_type == RECORD_TYPE_BLOB {
            &mut inner.inbound_blob
        } else {
            &mut inner.inbound
        };

        let frame_id = u32::from_be_bytes(plaintext[1..5].try_into().expect("fixed slice"));
        let index = u16::from_be_bytes(plaintext[5..7].try_into().expect("fixed slice"));
        let total = u16::from_be_bytes(plaintext[7..9].try_into().expect("fixed slice"));
        if total == 0 || index >= total {
            *lane = None;
            return Err(CoreError::Malformed(
                "invalid LAN record sequence".to_string(),
            ));
        }
        let chunk = &plaintext[RECORD_HEADER_SIZE..];

        if index == 0 {
            *lane = Some(InboundFrame {
                frame_id,
                total,
                next_index: 0,
                bytes: Vec::with_capacity(usize::min(
                    total as usize * RECORD_CHUNK_SIZE,
                    LAN_MAX_FRAME_SIZE as usize,
                )),
            });
        }
        let inbound = lane.as_mut().ok_or_else(|| {
            CoreError::Malformed("LAN record arrived without a frame start".to_string())
        })?;
        if inbound.frame_id != frame_id || inbound.total != total || inbound.next_index != index {
            *lane = None;
            return Err(CoreError::Malformed(
                "out-of-order LAN transport record".to_string(),
            ));
        }
        if inbound.bytes.len() + chunk.len() > LAN_MAX_FRAME_SIZE as usize {
            *lane = None;
            return Err(CoreError::Malformed(
                "reassembled LAN frame is too large".to_string(),
            ));
        }
        inbound.bytes.extend_from_slice(chunk);
        inbound.next_index += 1;
        if inbound.next_index < inbound.total {
            return Ok(None);
        }
        Ok(lane.take().map(|complete| LanRecord {
            record_type,
            frame: complete.bytes,
        }))
    }
}

fn promote_if_finished(inner: &mut SessionInner) -> Result<(), CoreError> {
    let finished = inner
        .handshake
        .as_ref()
        .map(|handshake| handshake.is_handshake_finished())
        .unwrap_or(false);
    if !finished {
        return Ok(());
    }
    let handshake = inner
        .handshake
        .take()
        .ok_or_else(|| CoreError::Crypto("LAN handshake state disappeared".to_string()))?;
    inner.remote_static = handshake.get_remote_static().map(ToOwned::to_owned);
    // Taken before the handshake state is consumed: `snow` publishes the
    // transcript hash on `HandshakeState` alone, and `into_transport_mode`
    // eats it.
    inner.handshake_hash = Some(handshake.get_handshake_hash().to_vec());
    inner.transport = Some(handshake.into_transport_mode().map_err(noise_error)?);
    Ok(())
}

fn max_of_one(value: usize) -> usize {
    usize::max(1, value)
}

fn noise_error(error: snow::Error) -> CoreError {
    CoreError::Crypto(format!("LAN Noise session failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use x25519_dalek::{PublicKey, StaticSecret};

    fn key(byte: u8) -> ([u8; 32], [u8; 32]) {
        let secret = StaticSecret::from([byte; 32]);
        let public = PublicKey::from(&secret);
        (secret.to_bytes(), public.to_bytes())
    }

    /// A handshaken pair that has also exchanged HELLO2 capabilities naming
    /// the blob plane — the state a link is in before any blob record is
    /// legitimately on it.
    fn connected_pair() -> (LanNoiseSession, LanNoiseSession) {
        let (initiator, responder) = handshaken_pair();
        initiator
            .set_peer_capabilities(crate::protocol::CAP_MEDIA_BLOB)
            .unwrap();
        responder
            .set_peer_capabilities(crate::protocol::CAP_MEDIA_BLOB)
            .unwrap();
        (initiator, responder)
    }

    /// The handshake alone: no capabilities have been exchanged yet.
    fn handshaken_pair() -> (LanNoiseSession, LanNoiseSession) {
        let (initiator_sk, initiator_pk) = key(7);
        let (responder_sk, responder_pk) = key(11);
        let initiator = LanNoiseSession::new(true, initiator_sk.to_vec()).unwrap();
        let responder = LanNoiseSession::new(false, responder_sk.to_vec()).unwrap();

        let message_1 = initiator.write_handshake_message().unwrap();
        responder.read_handshake_message(message_1).unwrap();
        let message_2 = responder.write_handshake_message().unwrap();
        initiator.read_handshake_message(message_2).unwrap();
        assert_eq!(initiator.remote_static_key(), Some(responder_pk.to_vec()));
        let message_3 = initiator.write_handshake_message().unwrap();
        responder.read_handshake_message(message_3).unwrap();

        assert!(initiator.is_handshake_finished());
        assert!(responder.is_handshake_finished());
        assert_eq!(responder.remote_static_key(), Some(initiator_pk.to_vec()));
        (initiator, responder)
    }

    /// Seal one record by hand, so a test can put a header on the wire that
    /// this crate's own encrypt path would never produce — a peer is free to
    /// interleave the two planes record by record. Records must be sealed in
    /// the order they will be delivered: a Noise transport nonce is implicit
    /// and sequential.
    fn seal(
        session: &LanNoiseSession,
        record_type: u8,
        frame_id: u32,
        index: u16,
        total: u16,
        chunk: &[u8],
    ) -> Vec<u8> {
        let mut plaintext = vec![record_type];
        plaintext.extend_from_slice(&frame_id.to_be_bytes());
        plaintext.extend_from_slice(&index.to_be_bytes());
        plaintext.extend_from_slice(&total.to_be_bytes());
        plaintext.extend_from_slice(chunk);
        let mut inner = session.lock().unwrap();
        let transport = inner.transport.as_mut().unwrap();
        let mut encrypted = vec![0u8; plaintext.len() + NOISE_TAG_SIZE];
        let written = transport.write_message(&plaintext, &mut encrypted).unwrap();
        encrypted.truncate(written);
        encrypted
    }

    #[test]
    fn noise_xx_authenticates_both_static_keys_and_round_trips_a_frame() {
        let (initiator, responder) = connected_pair();
        let records = initiator.encrypt_frame(b"hello over LAN".to_vec()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            responder.decrypt_record(records[0].clone()).unwrap(),
            Some(b"hello over LAN".to_vec())
        );
    }

    #[test]
    fn large_frame_is_split_and_reassembled_without_exposing_plaintext() {
        let (initiator, responder) = connected_pair();
        let frame = vec![0xAB; 180 * 1024];
        let records = initiator.encrypt_frame(frame.clone()).unwrap();
        assert!(records.len() > 1);
        assert!(records
            .iter()
            .all(|record| !record.windows(32).any(|window| window == [0xAB; 32])));

        let mut recovered = None;
        for record in records {
            recovered = responder.decrypt_record(record).unwrap().or(recovered);
        }
        assert_eq!(recovered, Some(frame));
    }

    #[test]
    fn a_blob_record_round_trips_on_the_same_session() {
        let (initiator, responder) = connected_pair();
        let records = initiator
            .encrypt_blob_record(b"pull frame".to_vec())
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            responder.decrypt_record_typed(records[0].clone()).unwrap(),
            Some(LanRecord {
                record_type: RECORD_TYPE_BLOB,
                frame: b"pull frame".to_vec(),
            })
        );
    }

    #[test]
    fn mesh_and_blob_records_interleave_without_corrupting_either_frame() {
        let (initiator, responder) = connected_pair();
        // A 180 KiB transfer frame spans four records; a two-record mesh frame
        // is emitted between them, which is the courtesy the spec's acceptance
        // bound rests on. Both frames carry frame id 0.
        let blob = vec![0xCD; 180 * 1024];
        let mesh = vec![0xEF; 90 * 1024];
        let blob_chunks: Vec<&[u8]> = blob.chunks(RECORD_CHUNK_SIZE).collect();
        let mesh_chunks: Vec<&[u8]> = mesh.chunks(RECORD_CHUNK_SIZE).collect();
        assert_eq!(blob_chunks.len(), 4);
        assert_eq!(mesh_chunks.len(), 2);
        let wire = vec![
            seal(&initiator, RECORD_TYPE_BLOB, 0, 0, 4, blob_chunks[0]),
            seal(&initiator, RECORD_TYPE_FRAME, 0, 0, 2, mesh_chunks[0]),
            seal(&initiator, RECORD_TYPE_BLOB, 0, 1, 4, blob_chunks[1]),
            seal(&initiator, RECORD_TYPE_FRAME, 0, 1, 2, mesh_chunks[1]),
            seal(&initiator, RECORD_TYPE_BLOB, 0, 2, 4, blob_chunks[2]),
            seal(&initiator, RECORD_TYPE_BLOB, 0, 3, 4, blob_chunks[3]),
        ];

        let mut completed = Vec::new();
        for record in wire {
            if let Some(complete) = responder.decrypt_record_typed(record).unwrap() {
                completed.push(complete);
            }
        }
        assert_eq!(
            completed,
            vec![
                LanRecord {
                    record_type: RECORD_TYPE_FRAME,
                    frame: mesh,
                },
                LanRecord {
                    record_type: RECORD_TYPE_BLOB,
                    frame: blob,
                },
            ]
        );
    }

    #[test]
    fn the_two_record_types_have_independent_frame_id_spaces() {
        let (initiator, responder) = connected_pair();
        // Both planes number their first frame 0; only the record type keeps
        // them apart, and neither counter moves when the other plane sends.
        let blob = initiator.encrypt_blob_record(vec![1u8; 90 * 1024]).unwrap();
        let mesh = initiator.encrypt_frame(vec![2u8; 90 * 1024]).unwrap();
        assert_eq!(blob.len(), 2);
        assert_eq!(mesh.len(), 2);
        {
            let inner = initiator.lock().unwrap();
            assert_eq!(inner.next_outbound_frame_id, 1);
            assert_eq!(inner.next_outbound_blob_frame_id, 1);
        }

        // Each frame's records arrive contiguously, and the frame id they
        // share does not let one reassembler adopt the other's records.
        assert_eq!(
            responder.decrypt_record_typed(blob[0].clone()).unwrap(),
            None
        );
        assert_eq!(
            responder.decrypt_record_typed(blob[1].clone()).unwrap(),
            Some(LanRecord {
                record_type: RECORD_TYPE_BLOB,
                frame: vec![1u8; 90 * 1024],
            })
        );
        assert_eq!(
            responder.decrypt_record_typed(mesh[0].clone()).unwrap(),
            None
        );
        assert_eq!(
            responder.decrypt_record_typed(mesh[1].clone()).unwrap(),
            Some(LanRecord {
                record_type: RECORD_TYPE_FRAME,
                frame: vec![2u8; 90 * 1024],
            })
        );
    }

    #[test]
    fn a_rejected_blob_record_leaves_an_in_flight_mesh_frame_alone() {
        let (initiator, responder) = connected_pair();
        // A blob record with an impossible sequence arrives in the middle of
        // a mesh frame. It must reset the blob lane only: resetting the shared
        // state would make a malformed transfer abort an innocent message.
        let head = seal(&initiator, RECORD_TYPE_FRAME, 0, 0, 2, b"first half ");
        let junk = seal(&initiator, RECORD_TYPE_BLOB, 0, 0, 0, b"nonsense");
        let tail = seal(&initiator, RECORD_TYPE_FRAME, 0, 1, 2, b"second half");

        assert_eq!(responder.decrypt_record_typed(head).unwrap(), None);
        assert!(responder.decrypt_record_typed(junk).is_err());
        assert_eq!(
            responder.decrypt_record_typed(tail).unwrap(),
            Some(LanRecord {
                record_type: RECORD_TYPE_FRAME,
                frame: b"first half second half".to_vec(),
            })
        );
    }

    #[test]
    fn a_blob_record_is_skipped_by_the_message_plane_decoder() {
        // What a peer without the blob plane does, and the property that
        // actually matters: `Ok(None)`, not an error. Every shell read loop
        // treats a decrypt error as fatal and closes the socket, so an error
        // here would let a requester on a newer build knock the message plane
        // over by asking politely. Asserted mid-frame, because the record has
        // to be skipped without disturbing the mesh reassembler either.
        let (initiator, responder) = connected_pair();
        let head = seal(&initiator, RECORD_TYPE_FRAME, 0, 0, 2, b"still ");
        let blob = initiator
            .encrypt_blob_record(b"unwelcome".to_vec())
            .unwrap();
        let tail = seal(&initiator, RECORD_TYPE_FRAME, 0, 1, 2, b"talking");

        assert_eq!(responder.decrypt_record(head).unwrap(), None);
        assert_eq!(
            responder.decrypt_record(blob[0].clone()).unwrap(),
            None,
            "a blob record must be dropped, not fail the link"
        );
        assert_eq!(
            responder.decrypt_record(tail).unwrap(),
            Some(b"still talking".to_vec())
        );

        // And the link keeps working for whole frames afterwards.
        let records = initiator.encrypt_frame(b"and again".to_vec()).unwrap();
        assert_eq!(
            responder.decrypt_record(records[0].clone()).unwrap(),
            Some(b"and again".to_vec())
        );
    }

    #[test]
    fn a_blob_record_from_a_peer_that_never_advertised_the_bit_is_skipped() {
        // Authenticating the link is not agreeing to a second plane on it. A
        // contact whose HELLO2 never carried CAP_MEDIA_BLOB must not be able
        // to open the blob reassembler — that lane holds its own megabyte of
        // buffer, and the requester side already refuses to open a pull
        // session against a peer without the bit, so the accept side has to
        // agree or the gate is one-sided.
        let (initiator, responder) = handshaken_pair();
        assert!(!responder.accepts_blob_records());

        let head = seal(&initiator, RECORD_TYPE_FRAME, 0, 0, 2, b"still ");
        let blob = initiator
            .encrypt_blob_record(b"unadvertised".to_vec())
            .unwrap();
        let tail = seal(&initiator, RECORD_TYPE_FRAME, 0, 1, 2, b"talking");

        assert_eq!(responder.decrypt_record_typed(head).unwrap(), None);
        assert_eq!(
            responder.decrypt_record_typed(blob[0].clone()).unwrap(),
            None,
            "the blob lane stays shut, and the record is skipped rather than fatal"
        );
        assert_eq!(
            responder.decrypt_record_typed(tail).unwrap(),
            Some(LanRecord {
                record_type: RECORD_TYPE_FRAME,
                frame: b"still talking".to_vec(),
            }),
            "and the message plane is undisturbed"
        );

        // Capabilities a peer does advertise, minus the blob bit, are still no.
        responder
            .set_peer_capabilities(!crate::protocol::CAP_MEDIA_BLOB)
            .unwrap();
        assert!(!responder.accepts_blob_records());

        // And once HELLO2 does carry it, the same session opens the lane.
        responder
            .set_peer_capabilities(crate::protocol::CAP_MEDIA_BLOB)
            .unwrap();
        assert!(responder.accepts_blob_records());
        let allowed = initiator.encrypt_blob_record(b"welcome".to_vec()).unwrap();
        assert_eq!(
            responder.decrypt_record_typed(allowed[0].clone()).unwrap(),
            Some(LanRecord {
                record_type: RECORD_TYPE_BLOB,
                frame: b"welcome".to_vec(),
            })
        );
    }

    #[test]
    fn an_unknown_record_type_is_skipped_and_the_link_survives() {
        let (initiator, responder) = connected_pair();
        // A well-formed record naming an unallocated type, mid mesh frame. The
        // record is authenticated, so this is a peer from a later version, and
        // forward compatibility costs nothing here: skip it and read on.
        let head = seal(&initiator, RECORD_TYPE_FRAME, 0, 0, 2, b"before ");
        let future = seal(&initiator, 3, 0, 0, 1, b"from the future");
        let tail = seal(&initiator, RECORD_TYPE_FRAME, 0, 1, 2, b"after");

        assert_eq!(responder.decrypt_record_typed(head).unwrap(), None);
        assert_eq!(responder.decrypt_record_typed(future).unwrap(), None);
        assert_eq!(
            responder.decrypt_record_typed(tail).unwrap(),
            Some(LanRecord {
                record_type: RECORD_TYPE_FRAME,
                frame: b"before after".to_vec(),
            })
        );

        let records = initiator.encrypt_frame(b"still talking".to_vec()).unwrap();
        assert_eq!(
            responder.decrypt_record(records[0].clone()).unwrap(),
            Some(b"still talking".to_vec())
        );
    }

    #[test]
    fn mesh_frames_outrank_blob_frames_in_the_send_queue() {
        assert_eq!(lan_record_priority(RECORD_TYPE_FRAME), 0);
        assert_eq!(lan_record_priority(RECORD_TYPE_BLOB), 1);
        assert_eq!(lan_record_priority(9), 2);

        // A queue sorted by priority encrypts every waiting mesh frame first
        // while preserving each plane's own order.
        let mut queued = [
            (RECORD_TYPE_BLOB, "blob 0"),
            (RECORD_TYPE_FRAME, "mesh 0"),
            (RECORD_TYPE_BLOB, "blob 1"),
            (RECORD_TYPE_FRAME, "mesh 1"),
        ];
        queued.sort_by_key(|(record_type, _)| lan_record_priority(*record_type));
        assert_eq!(
            queued.iter().map(|(_, label)| *label).collect::<Vec<_>>(),
            vec!["mesh 0", "mesh 1", "blob 0", "blob 1"]
        );
    }

    #[test]
    fn tampered_record_fails_closed() {
        let (initiator, responder) = connected_pair();
        let mut record = initiator
            .encrypt_frame(b"private".to_vec())
            .unwrap()
            .remove(0);
        let last = record.len() - 1;
        record[last] ^= 1;
        assert!(responder.decrypt_record(record).is_err());
    }

    /// What the own-device proof is bound to: one value, agreed by both ends
    /// of one handshake and by nothing else.
    #[test]
    fn handshake_hash_is_shared_by_both_ends_and_unique_to_a_session() {
        let (initiator, responder) = connected_pair();
        let hash = initiator.handshake_hash().expect("finished handshake");
        assert_eq!(hash.len(), 32);
        assert_eq!(responder.handshake_hash(), Some(hash.clone()));

        let (again, _) = connected_pair();
        assert_ne!(
            again.handshake_hash(),
            Some(hash),
            "ephemeral keys make every session's transcript its own"
        );
    }

    #[test]
    fn handshake_hash_is_absent_until_the_handshake_finishes() {
        let (secret, _) = key(5);
        let session = LanNoiseSession::new(true, secret.to_vec()).unwrap();
        assert_eq!(session.handshake_hash(), None);
        let _ = session.write_handshake_message().unwrap();
        assert_eq!(session.handshake_hash(), None);
    }

    #[test]
    fn wrong_key_length_is_rejected() {
        assert!(LanNoiseSession::new(true, vec![0; 31]).is_err());
    }

    #[test]
    fn application_frames_are_rejected_before_handshake_completion() {
        let (secret, _) = key(3);
        let session = LanNoiseSession::new(true, secret.to_vec()).unwrap();
        assert!(session.encrypt_frame(b"too early".to_vec()).is_err());
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation_or_encryption() {
        let (initiator, _) = connected_pair();
        assert!(initiator
            .encrypt_frame(vec![0; LAN_MAX_FRAME_SIZE as usize + 1])
            .is_err());
    }
}
