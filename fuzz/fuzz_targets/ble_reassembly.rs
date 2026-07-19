#![no_main]

use cruisemesh_core::BleFrameReassembler;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let single = BleFrameReassembler::new();
    let _ = single.accept(data.to_vec());

    let sequence = BleFrameReassembler::new();
    for fragment in data.chunks(512) {
        let _ = sequence.accept(fragment.to_vec());
    }
});

