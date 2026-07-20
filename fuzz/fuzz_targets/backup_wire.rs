#![no_main]

use cruisemesh_core::{decode_identity_bytes, open_backup};
use libfuzzer_sys::fuzz_target;

// FC5: `.cmbak` backup files are attacker/user-selected input parsed by
// `open_backup` (header/framing, then an AES-GCM decrypt, then the inner TLV
// decoder in `decode_inner`) and `decode_identity_bytes` (the fixed-width
// identity record inside a backup). Neither had fuzz coverage.
//
// `open_backup`'s cost is dominated by its own defense: the PBKDF2 work
// factor is read from the (attacker-controlled) header and bounds-checked to
// 100_000..=1_200_000 iterations before a key is derived, so almost every
// input that gets past the 40-byte header/magic/version check pays a real,
// deliberately expensive KDF before failing at the AES-GCM MAC check. That's
// the point of the KDF, not a fuzzing gap -- there's no cheaper path through
// the public API, and adding one would mean shipping a weakened code path.
// A fixed passphrase still lets the fuzzer freely explore magic bytes,
// version, KDF id, iteration count, salt/nonce, and ciphertext/tag framing;
// it just won't ever find the passphrase, which is the intended property.
//
// `decode_inner` (the length-prefixed Cursor TLV walk that runs after a
// successful decrypt: display name, sqlite blob, avatar, relay url/token)
// is private and only reachable after that decrypt, so it isn't reachable
// standalone through the public API. `decode_identity_bytes` is: it decodes
// the fixed 144-byte identity record with no length-prefixed sub-fields, so
// it's exercised directly here for that class of bug regardless of AEAD
// framing.
fuzz_target!(|data: &[u8]| {
    let _ = open_backup("fuzz-passphrase".to_string(), data.to_vec());
    let _ = decode_identity_bytes(data.to_vec());
});
