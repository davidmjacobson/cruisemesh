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
const RECORD_TYPE_FRAME: u8 = 1;
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

#[derive(uniffi::Object)]
pub struct LanNoiseSession {
    inner: Mutex<SessionInner>,
}

struct SessionInner {
    handshake: Option<HandshakeState>,
    transport: Option<TransportState>,
    remote_static: Option<Vec<u8>>,
    next_outbound_frame_id: u32,
    inbound: Option<InboundFrame>,
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
                next_outbound_frame_id: 0,
                inbound: None,
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

    /// Encrypt one complete CruiseMesh protocol frame into one or more Noise
    /// records. Native shells prefix each returned record with a u32 BE length
    /// before writing it to TCP.
    pub fn encrypt_frame(&self, frame: Vec<u8>) -> Result<Vec<Vec<u8>>, CoreError> {
        if frame.len() as u64 > LAN_MAX_FRAME_SIZE {
            return Err(CoreError::Malformed(format!(
                "LAN frame exceeds {} byte limit",
                LAN_MAX_FRAME_SIZE
            )));
        }
        let mut inner = self.lock()?;
        let frame_id = inner.next_outbound_frame_id;
        inner.next_outbound_frame_id = inner.next_outbound_frame_id.wrapping_add(1);
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
            plaintext.push(RECORD_TYPE_FRAME);
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

    /// Decrypt one Noise record. Returns a complete CruiseMesh protocol frame
    /// after the final record, or `None` while a multi-record frame is still
    /// being assembled.
    pub fn decrypt_record(&self, record: Vec<u8>) -> Result<Option<Vec<u8>>, CoreError> {
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
        if plaintext.len() < RECORD_HEADER_SIZE || plaintext[0] != RECORD_TYPE_FRAME {
            inner.inbound = None;
            return Err(CoreError::Malformed(
                "invalid LAN transport record".to_string(),
            ));
        }

        let frame_id = u32::from_be_bytes(plaintext[1..5].try_into().expect("fixed slice"));
        let index = u16::from_be_bytes(plaintext[5..7].try_into().expect("fixed slice"));
        let total = u16::from_be_bytes(plaintext[7..9].try_into().expect("fixed slice"));
        if total == 0 || index >= total {
            inner.inbound = None;
            return Err(CoreError::Malformed(
                "invalid LAN record sequence".to_string(),
            ));
        }
        let chunk = &plaintext[RECORD_HEADER_SIZE..];

        if index == 0 {
            inner.inbound = Some(InboundFrame {
                frame_id,
                total,
                next_index: 0,
                bytes: Vec::with_capacity(usize::min(
                    total as usize * RECORD_CHUNK_SIZE,
                    LAN_MAX_FRAME_SIZE as usize,
                )),
            });
        }
        let inbound = inner.inbound.as_mut().ok_or_else(|| {
            CoreError::Malformed("LAN record arrived without a frame start".to_string())
        })?;
        if inbound.frame_id != frame_id || inbound.total != total || inbound.next_index != index {
            inner.inbound = None;
            return Err(CoreError::Malformed(
                "out-of-order LAN transport record".to_string(),
            ));
        }
        if inbound.bytes.len() + chunk.len() > LAN_MAX_FRAME_SIZE as usize {
            inner.inbound = None;
            return Err(CoreError::Malformed(
                "reassembled LAN frame is too large".to_string(),
            ));
        }
        inbound.bytes.extend_from_slice(chunk);
        inbound.next_index += 1;
        if inbound.next_index < inbound.total {
            return Ok(None);
        }
        Ok(inner.inbound.take().map(|complete| complete.bytes))
    }
}

impl LanNoiseSession {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, SessionInner>, CoreError> {
        self.inner
            .lock()
            .map_err(|_| CoreError::Crypto("LAN session state is unavailable".to_string()))
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

    fn connected_pair() -> (LanNoiseSession, LanNoiseSession) {
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
