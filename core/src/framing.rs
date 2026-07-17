//! Canonical BLE link-layer fragmentation shared by Android and iOS.
//!
//! Every fragment uses a four-byte big-endian header: `index16 | total16`.
//! Keeping this wire format in Rust prevents the platform shells from drifting.

use std::sync::Mutex;

const FRAGMENT_HEADER_SIZE: usize = 4;
pub const ATT_HEADER_OVERHEAD: u16 = 3;
pub const DEFAULT_ATT_MTU: u16 = 23;
pub const MAX_ATT_VALUE_LEN: usize = 512;
pub const MAX_FRAGMENTS: usize = u16::MAX as usize;

#[uniffi::export]
pub fn ble_att_header_overhead() -> u16 {
    ATT_HEADER_OVERHEAD
}

#[uniffi::export]
pub fn ble_default_att_mtu() -> u16 {
    DEFAULT_ATT_MTU
}

#[uniffi::export]
pub fn ble_max_att_value_len() -> u16 {
    MAX_ATT_VALUE_LEN as u16
}

/// Split a frame into canonical BLE fragments. Returns `None` when the input
/// cannot be represented by the 16-bit fragment count rather than unwinding a
/// platform GATT callback with an exception.
#[uniffi::export]
pub fn fragment_ble_frame(frame: Vec<u8>, mtu_payload_size: u32) -> Option<Vec<Vec<u8>>> {
    let capped_payload = (mtu_payload_size as usize).min(MAX_ATT_VALUE_LEN);
    let chunk_size = capped_payload.saturating_sub(FRAGMENT_HEADER_SIZE).max(1);
    let total = frame.len().div_ceil(chunk_size).max(1);
    if total > MAX_FRAGMENTS {
        return None;
    }

    let mut fragments = Vec::with_capacity(total);
    for index in 0..total {
        let start = index * chunk_size;
        let end = (start + chunk_size).min(frame.len());
        let mut fragment = Vec::with_capacity(FRAGMENT_HEADER_SIZE + end - start);
        fragment.extend_from_slice(&(index as u16).to_be_bytes());
        fragment.extend_from_slice(&(total as u16).to_be_bytes());
        fragment.extend_from_slice(&frame[start..end]);
        fragments.push(fragment);
    }
    Some(fragments)
}

#[derive(Default)]
struct ReassemblyState {
    buffer: Option<Vec<u8>>,
    expected_total: u16,
    next_index: u16,
}

/// Ordered, single-frame-at-a-time BLE fragment reassembler.
#[derive(uniffi::Object)]
pub struct BleFrameReassembler {
    state: Mutex<ReassemblyState>,
}

#[uniffi::export]
impl BleFrameReassembler {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ReassemblyState::default()),
        }
    }

    /// Feed one fragment, returning the full frame only after its last ordered
    /// fragment. Malformed or desynchronized input drops the partial frame.
    pub fn accept(&self, fragment: Vec<u8>) -> Option<Vec<u8>> {
        if fragment.len() < FRAGMENT_HEADER_SIZE {
            return None;
        }
        let index = u16::from_be_bytes([fragment[0], fragment[1]]);
        let total = u16::from_be_bytes([fragment[2], fragment[3]]);
        let mut state = self.state.lock().expect("BLE reassembler mutex poisoned");

        if total == 0 || index >= total {
            *state = ReassemblyState::default();
            return None;
        }
        if index == 0 {
            state.buffer = Some(Vec::new());
            state.expected_total = total;
            state.next_index = 0;
        }
        if state.buffer.is_none() || index != state.next_index || total != state.expected_total {
            *state = ReassemblyState::default();
            return None;
        }

        state
            .buffer
            .as_mut()
            .expect("buffer checked above")
            .extend_from_slice(&fragment[FRAGMENT_HEADER_SIZE..]);
        state.next_index += 1;
        if state.next_index < state.expected_total {
            return None;
        }
        state.buffer.take()
    }
}

impl Default for BleFrameReassembler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: Vec<u8>, payload_size: u32) -> (Vec<Vec<u8>>, Vec<u8>) {
        let fragments = fragment_ble_frame(frame.clone(), payload_size).unwrap();
        let reassembler = BleFrameReassembler::new();
        let mut result = None;
        for fragment in &fragments {
            result = reassembler.accept(fragment.clone()).or(result);
        }
        (fragments, result.unwrap())
    }

    #[test]
    fn default_att_mtu_round_trips() {
        let frame: Vec<u8> = (0..100).collect();
        let (fragments, result) = round_trip(frame.clone(), 20);
        assert_eq!(fragments.len(), 7);
        assert_eq!(result, frame);
    }

    #[test]
    fn caps_each_fragment_at_attribute_limit() {
        let frame = vec![7; 4_000];
        let (fragments, result) = round_trip(frame.clone(), 514);
        assert!(fragments.iter().all(|fragment| fragment.len() <= 512));
        assert_eq!(result, frame);
    }

    #[test]
    fn photo_scale_frame_exceeds_old_u8_limit_and_round_trips() {
        let frame = vec![9; 170_000];
        let (fragments, result) = round_trip(frame.clone(), 514);
        assert!(fragments.len() > 255);
        assert_eq!(result, frame);
    }

    #[test]
    fn oversized_frame_is_rejected_without_panicking() {
        assert!(fragment_ble_frame(vec![0; MAX_FRAGMENTS + 1], 5).is_none());
    }

    #[test]
    fn dropped_middle_fragment_discards_partial_frame() {
        let fragments = fragment_ble_frame(vec![1; 50], 10).unwrap();
        let reassembler = BleFrameReassembler::new();
        assert!(reassembler.accept(fragments[0].clone()).is_none());
        assert!(reassembler.accept(fragments[2].clone()).is_none());
    }

    #[test]
    fn zero_total_is_invalid() {
        let reassembler = BleFrameReassembler::new();
        assert!(reassembler.accept(vec![0, 0, 0, 0, 1]).is_none());
    }
}
