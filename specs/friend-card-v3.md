# Friend card v3 (`CMFRIEND3:`) — compact link form

Status: **phase 1 — parser shipped, emitter still v2.** The flip to emitting
v3 is a deliberate later change (see §Rollout).

## Why

A shared friend card today is `https://cruisemesh.app/f#CMFRIEND2:<base64url>`,
about 265 characters for a typical card on the hosted relay. Only ~96 bytes of
that is irreducible entropy (two 32-byte public keys + a 32-byte relay token).
The rest is compressible padding, all of it in the v2 binary layout:

* the relay URL is the same constant string (`https://relay.cruisemesh.app`)
  for every card on the hosted service — ~31 payload bytes each time;
* the relay token is minted as 64 lowercase-hex characters and embedded as
  that ASCII string, so 32 bytes of entropy costs 64 payload bytes before the
  outer base64 even starts.

v3 fixes both: the hosted-relay URL becomes a one-byte tag, and hex tokens are
carried as raw bytes. An unsigned hosted-relay card drops to ~175 characters
(~35% shorter), and the QR code loses a density tier. Cards naming a
self-hosted relay or a non-hex token still encode, just without the savings. A
*signed* card (see §Self-signature) adds its 65-byte signature field, so it is
larger — but v3's whole reason to exist is making room for that binding while
still beating v2 on the common unsigned case.

This is the same playbook as v1→v2: a new emitted form, and
`parse_friend_text` accepts every form ever emitted, forever.

## Wire layout

The link body is `CMFRIEND3:` + base64url (no padding) of:

```
sign_pk[32] ‖ agree_pk[32] ‖ name_len:u8 ‖ name[name_len] ‖ relay_url_field ‖ relay_token_field ‖ signature_field
```

`sign_pk`, `agree_pk`, and `name` are exactly as in v2 (name is UTF-8, capped
at 128 bytes by `validate_friend_card`). The trailing `signature_field` is the
v3 addition over the v2 layout — the primary card's self-signature (see
§Self-signature).

**relay_url_field** — first byte is a tag:

| tag | meaning | payload after tag |
|---|---|---|
| `0x00` | no relay URL | none |
| `0x01` | explicit URL | `len:u16_be ‖ utf8[len]` |
| `0x02` | official relay | none — decodes to exactly `https://relay.cruisemesh.app` |

**relay_token_field** — first byte is a tag:

| tag | meaning | payload after tag |
|---|---|---|
| `0x00` | no token | none |
| `0x01` | verbatim string token | `len:u16_be ‖ utf8[len]` |
| `0x02` | packed hex token | `len:u8 ‖ raw[len]` — decodes to the 2×len-char lowercase-hex string of those bytes |

**signature_field** — first byte is a tag:

| tag | meaning | payload after tag |
|---|---|---|
| `0x00` | unsigned card | none |
| `0x01` | self-signature | `raw[64]` — the Ed25519 signature (fixed length, no prefix) |

Any other tag value is a hard parse error.

## Self-signature (card integrity)

The v3 addition over v2 is the card owner's own Ed25519 signature, binding the
agreement key and relay fields to the signing key / UserID that the verbal
safety words cover. Without it, the fingerprint (derived from `sign_pk` only)
does not detect an `agree_pk` or relay swap made on a tamperable sharing
channel, so a substituted agreement key would silently seal mail to an
attacker.

* **Signed bytes** are wire-format independent: a fixed domain separator
  (`FRIEND_CARD_SIGN_DOMAIN`, distinct from every other signing domain) followed
  by the length-framed fields in the order
  `sign_pk ‖ agree_pk ‖ relay_url ‖ relay_token ‖ name`, each length-prefixed,
  with an explicit presence byte on the two optional relay fields (so a `None`
  relay URL never collides with a `Some("")` one). The signature does not cover
  itself. The same bytes are signed whether the card later ships as JSON (friend
  requests) or as this `CMFRIEND3:` binary — this field is only how the binary
  link carries it. See `friend_card_signed_bytes` in `core/src/identity.rs`.
* **Encode:** `make_friend_card` signs under the owner's key over the *final*
  field values (the relay token is already deposit-attenuated), so the signature
  covers exactly what ships. The `0x01` payload is exactly 64 bytes;
  `validate_friend_card` rejects any other present-signature length.
* **Decode (import):** a card carrying a signature is verified against its own
  `sign_pk`. A valid signature is accepted; a **present-but-invalid** signature
  (wrong length or failed verification) is rejected with `SignatureInvalid` and
  is **never** downgraded to unsigned. A card with tag `0x00` (or a legacy
  v1/v2 card, which has no signature field at all) imports as unsigned — the
  fleet still holds those.
* **Residual / rollout:** legacy unsigned cards remain `agree_pk`-substitutable
  via the fingerprint gap, and until a future *require-signed* flip an attacker
  can strip the signature and present an unsigned card. Signed cards are the
  MITM-proof path; requiring signatures everywhere is a separate later step,
  tracked with the emit flip below.

## Encoder rules (canonical, lossless)

The encoder MUST be deterministic and the round trip MUST be exact:
`decode(encode(card)) == card`, byte-for-byte on every field, for every card
that `validate_friend_card` accepts. Compressed forms are used only when
expansion is provably byte-identical:

* Use URL tag `0x02` iff `relay_url == Some("https://relay.cruisemesh.app")`
  — exact string equality against the canonical constant (single source of
  truth shared with `OFFICIAL_RELAY_HOST` in `relay_setup.rs`; do not
  duplicate the literal). Any other value — different case, trailing slash,
  port, anything — uses `0x01` verbatim. Do not normalize inside the encoder;
  a false negative only costs bytes.
* Use token tag `0x02` iff the token is non-empty, of even length ≤ 510,
  and consists solely of lowercase hex digits `[0-9a-f]`. Uppercase or mixed
  case MUST fall back to `0x01` (re-encoding would change the token and break
  relay auth). Otherwise `0x01` verbatim.

## Decoder rules (liberal, hardened)

* Every read bounds-checked; truncation, unknown tags, or trailing bytes are
  errors, never panics — the same standard the v2 decoder is held to.
* `0x02` token with `len == 0` is an error (a token is never empty).
* Non-minimal encodings are accepted: an official URL spelled out via `0x01`,
  or an all-hex token carried via `0x01`, decode fine. Only the encoder is
  strict.
* The decoded card passes through `validate_friend_card` and then
  self-signature verification before being returned. A present-but-invalid
  signature is rejected (`SignatureInvalid`), never downgraded to unsigned
  (see §Self-signature).

## Parse order

`parse_friend_text` tries prefixes in order: `CMFRIEND3:`, `CMFRIEND2:`,
`CMFRIEND1:`, then raw JSON. All forms remain accepted forever. The prefix
extraction (`extract_link_body`) is substring-based including the trailing
colon, so the three prefixes cannot shadow each other.

## Emitter gating

`make_friend_link` keeps emitting `CMFRIEND2:` in this phase, gated on a
single private const in `identity.rs`:

```rust
/// Flip to true only after the fleet parses CMFRIEND3 (see specs/friend-card-v3.md §Rollout).
const EMIT_FRIEND_LINK_V3: bool = false;
```

The v3 encoder is fully implemented and tested now, so the flip is a one-line
diff with no new code paths. Tests exercise the v3 emit path directly (not
through the const), plus one test pinning that `make_friend_link` currently
still emits the `CMFRIEND2:` prefix — so the eventual flip is caught as a
deliberate test update, not an accident.

## What must be true before the flip (phase 2, separate PR)

* Both platforms have shipped a release whose `parse_friend_text` accepts
  `CMFRIEND3:`, and the fleet has had time to update.
* An old build receiving a v3 link opens the add-friend flow and shows its
  generic "not a CruiseMesh friend card" error — no crash, but a dead end;
  that is why the parser ships first.

## Test checklist (phase 1)

* Round-trip property across the field matrix: name present/empty/128-byte
  UTF-8 multibyte; relay URL absent / official / self-hosted; token absent /
  64-char lowercase hex / uppercase hex (must take the `0x01` path and
  round-trip verbatim) / odd-length hex-ish / non-hex string.
* Exact-equality round trip: every decoded field byte-identical to the input.
* Full-link parsing: `https://cruisemesh.app/f#CMFRIEND3:…`, bare link,
  link embedded in prose, link split across lines (whitespace filtering).
* Adversarial decode: truncation at every field boundary, unknown
  URL/token/signature tags, zero-length `0x02` token, trailing bytes, name
  length past buffer — all clean errors.
* Self-signature: a signed card round-trips through the binary byte-for-byte
  and verifies on decode; a card with `agree_pk` tampered inside the signed
  binary is rejected with `SignatureInvalid`; an unsigned (`0x00`) card decodes
  as unsigned. (The JSON-level signature checks — valid/legacy/tampered/
  present-but-invalid — live alongside these in `identity.rs`.)
* Size: a typical hosted-relay card (8-char name, official URL, 64-hex-char
  token) yields a full https link ≤ 190 chars and ≥ 30% shorter than the v2
  link for the same card (the one-byte signature field for an unsigned card is
  within this bound).
* `make_friend_link` still emits `CMFRIEND2:` (flip tripwire).
* `deep_link_route` + `core_detect_links` treat a `/f#CMFRIEND3:` URL the
  same as a v2 one.
* Fuzz: `fuzz_targets/protocol_decoders.rs` already drives
  `parse_friend_text`, which now reaches the v3 decoder; add a v3 seed to the
  corpus if one exists for the text targets.
