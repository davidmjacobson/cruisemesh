#![no_main]

use cruisemesh_core::{
    relay_decode_fetch_page, relay_decode_post_response, relay_decode_presence_page,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let bytes = data.to_vec();
    let _ = relay_decode_post_response(bytes.clone());
    let _ = relay_decode_fetch_page(bytes.clone());
    let _ = relay_decode_presence_page(bytes);
});

