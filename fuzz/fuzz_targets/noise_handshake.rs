#![no_main]

use cruisemesh_core::LanNoiseSession;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let local_private_key = vec![7; 32];

    let responder = LanNoiseSession::new(false, local_private_key.clone()).unwrap();
    let _ = responder.read_handshake_message(data.to_vec());

    let initiator = LanNoiseSession::new(true, local_private_key).unwrap();
    let _ = initiator.write_handshake_message();
    let _ = initiator.read_handshake_message(data.to_vec());
});

