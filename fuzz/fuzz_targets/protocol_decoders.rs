#![no_main]

use cruisemesh_core::{
    core_parse_lan_endpoint, core_parse_lan_endpoint_link, decode_attachment_payload,
    decode_extended_message_body, decode_friend_directory_content, decode_group_invite_content,
    decode_group_metadata_update, decode_identity_bytes, decode_introduced_friend_request,
    decode_lan_endpoint_content, decode_message_body, decode_profile_sync_content,
    decode_reaction_payload, decode_receipt_content, parse_frame, parse_friend_card,
    parse_friend_text,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let bytes = data.to_vec();
    let _ = parse_frame(bytes.clone());
    let _ = decode_message_body(bytes.clone());
    let _ = decode_extended_message_body(bytes.clone());
    let _ = decode_receipt_content(bytes.clone());
    let _ = decode_profile_sync_content(bytes.clone());
    let _ = decode_lan_endpoint_content(bytes.clone());
    let _ = decode_friend_directory_content(bytes.clone());
    let _ = decode_introduced_friend_request(bytes.clone());
    let _ = decode_group_invite_content(bytes.clone());
    let _ = decode_group_metadata_update(bytes.clone());
    let _ = decode_attachment_payload(bytes.clone());
    let _ = decode_reaction_payload(bytes.clone());
    let _ = decode_identity_bytes(bytes);

    let text = String::from_utf8_lossy(data).into_owned();
    let _ = parse_friend_card(text.clone());
    let _ = parse_friend_text(text.clone());

    // FC5: core_parse_lan_endpoint/core_parse_lan_endpoint_link (untrusted
    // text -- typed/pasted host:port strings and lan# link fragments,
    // bracket/base64 slicing) had no fuzz coverage.
    let _ = core_parse_lan_endpoint(text.clone(), 45892);
    let _ = core_parse_lan_endpoint_link(Some(text));
    let _ = core_parse_lan_endpoint_link(None);
});

