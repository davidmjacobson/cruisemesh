# Local Encrypted Backup & Restore — Design Spec

Status: proposal (v1)
Audience: implementer (Claude / Sonnet), reviewer (David)
Platform: Android first. iOS parity is a follow-up (identical bundle format).

---

## 1. Problem

Uninstalling the app — or swapping between a debug and a release build (they have
different signing keys, so Android forces an uninstall to switch) — **wipes
everything**: identity, contacts, groups, and message history. There is currently
no way to carry an account across a reinstall or to a new device.

We want a **manual, user-driven, encrypted backup file** that the user can save
wherever they like (Nextcloud, Drive, a USB copy) and later **restore during
first-run onboarding** so their account comes back intact.

Non-goal for v1: automatic or cloud backup (see §9).

---

## 2. The core difficulty: the identity key is deliberately un-exportable

Data lives in two stores:

| Data | Where | Notes |
|------|-------|-------|
| **Identity**: `userId`(16) + `signPk`/`signSk`(32 ea, Ed25519) + `agreePk`/`agreeSk`(32 ea, X25519) = **144 bytes** | SharedPreferences `cruisemesh_identity`, AES-GCM-encrypted | wrapping key lives in **Android Keystore**, alias `cruisemesh_identity_key` |
| **Messages + contacts + groups + contact avatars + relay/receipt queue state** | SQLite `filesDir/cruisemesh.sqlite` (Rust core `MessageStore`) | opened lazily by `AppStore.get()` |
| **Own profile + relay fallback**: display name, own avatar/epoch, relay URL/token, presence-sharing preference | SharedPreferences + `filesDir/profile/avatar.jpg` | included by inner format v2; v1 backups omit these fields |

The trap: the Keystore wrapping key is **non-exportable and hardware-bound — it
never leaves secure hardware and is destroyed on uninstall.** So backing up the
*encrypted* SharedPreferences blob is worthless: after reinstall there is no key
to decrypt it. (`IdentityStore.load` already handles this exact case by discarding
the undecryptable blob and minting a fresh identity.)

**Therefore the backup must contain the _decrypted_ 144-byte identity**, re-wrapped
under a key derived from a **user passphrase** — something that survives leaving
the device. On restore, `IdentityStore.save()` re-encrypts it under the *new*
device's fresh Keystore key. `encodeIdentity` / `decodeIdentity`
(`IdentityStore.kt`) already give us the exact 144-byte wire form to embed.

### 2.1 Security trade-off — make this call consciously

DESIGN.md §6.2 currently guarantees secret keys are hardware-bound and never
exportable. **This feature intentionally relaxes that.** The backup file holds the
raw Ed25519/X25519 secret keys; anyone with the file **and** the passphrase can
fully impersonate the user and read all history. This is unavoidable — the point
of the feature is to escape single-install lock-in, which requires the keys to
leave the Keystore. Mitigations:

- Passphrase-derived encryption with a slow KDF (§4), never the Keystore.
- Enforce a minimum passphrase strength; show a blunt one-time warning that the
  file *is* the account and a weak passphrase = a stolen identity.
- Keep it fully **manual and offline** — no automatic upload anywhere (§9).
- `android:allowBackup="false"` is already set, so Google's auto-backup is not
  quietly shipping a (broken, undecryptable) copy today. Keep it false.

---

## 3. What goes in a backup

v1 uses a **raw SQLite snapshot** for the message store (complete, schema-faithful,
minimal code) plus the extracted identity. Logical per-row export is deferred
(§9) — the raw file already captures messages, contacts, groups, avatars, and
queue state in one shot, and the core runs migrations on `open()` so a snapshot
restored into a same-or-newer app version just works.

Plaintext bundle (before encryption) = a small header + two payload blobs:

```
BackupPlaintext {
  identity:   144 bytes   // encodeIdentity(): userId|signPk|signSk|agreePk|agreeSk
  sqlite:     N bytes     // consistent snapshot of cruisemesh.sqlite (§6.1)
}
```

Inner format v1 restored identity + message store only. Inner format v2 also
preserves the user's display name, own avatar and avatar epoch, relay fallback
URL/token, and presence-sharing preference. The decoder remains backward
compatible with v1 files; their omitted settings retain fresh-install defaults.
The onboarding-completed flag is deliberately re-established by a successful
restore rather than stored in the file.

---

## 4. File format `.cmbak`

Single binary file, all integers big-endian:

```
magic        "CMBAK1\0"        7 bytes
version      u8                = 1
kdf_id       u8                1 = Argon2id (preferred), 2 = scrypt fallback
kdf_params   16 bytes          Argon2id: mem_kib u32 | iters u32 | parallelism u32 | reserved u32
salt         16 bytes          CSPRNG
nonce        12 bytes          CSPRNG, AES-GCM IV
ciphertext   variable          AES-256-GCM( key, nonce, plaintext, aad=header[0..44] )
tag          16 bytes          GCM auth tag
```

- **KDF**: Argon2id (mem ≈ 64 MiB, iters ≈ 3, parallelism 1 — tune on a mid device
  to ~0.5–1 s). Argon2 needs a dependency; if we'd rather stay dependency-light,
  scrypt via `SCrypt` (Bouncy Castle, already transitively available through JNA?
  verify) or PBKDF2-HMAC-SHA256 with ≥600k iters as a last resort. Store the choice
  in `kdf_id` so old files stay readable if we upgrade later.
- **Cipher**: AES-256-GCM (`javax.crypto`, no extra deps). Header bytes are the
  GCM AAD so the format/params can't be tampered with.
- Wrong passphrase → GCM tag verification fails → surface a clean
  "incorrect passphrase or corrupt file" error, never a crash.

The `plaintext` inside is length-framed:

```
inner_version u8 = 1
identity_len  u16 (= 144)   identity bytes
sqlite_len    u32           sqlite bytes
src_version_code u32        app versionCode that wrote it (downgrade guard, §7)
created_at_ms u64
```

---

## 5. Pure-Kotlin testable seam

Follow the existing pattern (`OverlayPlacement`, `ConversationLayout`,
`MeshStatusTextLogic`): put all byte-wrangling in a **plain Kotlin object with no
Android/Compose deps**, unit-tested to the boundary.

```
identity/backup/BackupCodec.kt      // object: encode/decode header + inner framing
identity/backup/BackupCrypto.kt     // KDF + AES-GCM seal/open (takes bytes, returns bytes)
identity/backup/BackupService.kt    // Android glue: reads IdentityStore + sqlite, SAF I/O
```

`BackupCodec` and the framing in `BackupCrypto` are covered by
`BackupCodecTest` / `BackupCryptoTest`:
- round-trips arbitrary identity+sqlite bytes,
- wrong passphrase fails to open,
- one flipped ciphertext/header byte fails the tag,
- truncated file / bad magic / unknown version → typed error, no exception leak,
- known-answer vector for the KDF so params don't silently drift.

---

## 6. Backup (export) flow

Entry point: **Settings → "Back up account"**.

1. Prompt for a passphrase (enter twice, strength meter, min length). Explain in
   one sentence that this file is the account and can't be recovered without the
   passphrase.
2. `BackupService.buildBundle(context, passphrase)`:
   a. `identity = encodeIdentity(IdentityStore.load(context)!!)` — 144 bytes.
   b. Take a consistent SQLite snapshot (§6.1).
   c. Frame → `BackupCrypto.seal(passphrase, plaintext)` → `.cmbak` bytes.
3. Launch SAF `ACTION_CREATE_DOCUMENT` (mime `application/octet-stream`, suggested
   name `cruisemesh-backup-<yyyyMMdd-HHmm>.cmbak`). User picks the destination
   (Nextcloud, Drive, Downloads…). No storage permission needed.
4. Write bytes to the returned `Uri`; confirm success.

### 6.1 Consistent SQLite snapshot

The store may be open (`AppStore` holds a live `MessageStore`). Copying the file
mid-write can capture a torn WAL. Options, simplest first:

- **Preferred**: run `VACUUM INTO '<temp>'` against the DB to get a single clean,
  fully-checkpointed file — but that requires a SQL entry point the Rust core
  doesn't currently expose. **Add a tiny core method** `MessageStore.backup_to(path)`
  (Rust: `VACUUM INTO ?1`, or `sqlite3_backup` API) — cleanest, and gives iOS the
  same primitive for free. **Recommended.**
- **Fallback without touching core**: force a WAL checkpoint by closing the store
  (`AppStore` needs a `close()` that disposes the `MessageStore` and nulls the
  singleton), then copy `cruisemesh.sqlite` (plus any lingering `-wal`/`-shm`)
  while closed, then reopen. Workable but requires quiescing mesh I/O first.

Prefer adding `backup_to` to the core; it's ~10 lines of Rust and avoids
stop-the-world.

---

## 7. Restore (import) flow

Restore is only safe **before** an identity/store exists, so it belongs in
**onboarding**, as a "Restore from backup" branch on the first screen (alongside
"Create new identity"). `OnboardingStore.isCompleted()` is false and no
`cruisemesh.sqlite` exists at this point — clean slate.

1. User taps "Restore from backup" → SAF `ACTION_OPEN_DOCUMENT` → pick `.cmbak`.
2. Prompt for passphrase.
3. `BackupCrypto.open(passphrase, bytes)`:
   - bad magic/version → "not a CruiseMesh backup".
   - `src_version_code > BuildConfig.VERSION_CODE` → **refuse** ("backup is from a
     newer app version; update first") — never restore a newer schema into an
     older core.
   - tag failure → "incorrect passphrase or corrupt file".
4. On success:
   a. Write `sqlite` bytes to `filesDir/cruisemesh.sqlite` (assert it doesn't
      already exist; we're in fresh onboarding). Do **not** open the store first.
   b. `IdentityStore.save(context, decodeIdentity(identity))` — re-wraps the keys
      under this device's fresh Keystore key.
   c. `OnboardingStore.markCompleted(context)`.
   d. `AppStore.get()` now opens the restored DB; core migrations run on open.
5. Land in the normal chat list — contacts, groups, and history are back, and the
   restored identity means peers still recognize the user.

**Restoring into an already-set-up install** (Settings-triggered overwrite) is
explicitly out of scope for v1: it means destroying a live identity + DB and
reopening the store, with failure modes (half-applied restore) that need
transactional care. Onboarding-only keeps v1 safe.

---

## 8. Edge cases & failure handling

- **Wrong passphrase** — GCM tag fails, typed error, retryable, no data touched.
- **Truncated / corrupt / non-CMBAK file** — magic + length checks reject before
  any KDF work; clear message.
- **Downgrade** — `src_version_code` guard in §7 step 3.
- **Backup with no identity yet** — export button hidden/disabled until onboarding
  is complete and `IdentityStore.load` is non-null.
- **Huge history** — sqlite could be tens of MB; do KDF + crypto + I/O off the main
  thread (coroutine `Dispatchers.Default`/`IO`), show progress, stream to the SAF
  `OutputStream` rather than holding two copies in memory if it gets large.
- **SAF cancel** — user backs out of the file picker: no-op, no partial file.
- **allowBackup** — leave `android:allowBackup="false"`; this feature replaces the
  need for it and avoids shipping undecryptable blobs to Google.

---

## 9. Deferred (not v1)

- **Automatic / scheduled backup** and **any cloud auto-upload** — raises "keys sit
  in a cloud you don't control" questions; must be a separate, explicit decision.
- **Logical (per-row) export** via `listContacts` / `listGroups` /
  `messagesForChat` / `insertMessage` — schema-independent and inspectable, but more
  code and easy to miss a table (avatars, reactions, receipts). Raw snapshot first.
- **In-place restore over an existing account** (§7).
- **QR / device-to-device transfer** — same crypto core, different transport.
- **iOS implementation** — reuse the exact `.cmbak` format and inner framing;
  Keychain plays the Keystore role, `sqlite3_backup` the snapshot role.

---

## 10. Work checklist

- [ ] Core: add `MessageStore.backup_to(path)` (VACUUM INTO / sqlite3_backup),
      regenerate uniffi bindings.
- [ ] `BackupCodec.kt` + `BackupCodecTest.kt` (framing, header, error types).
- [ ] `BackupCrypto.kt` + `BackupCryptoTest.kt` (Argon2id/scrypt KDF + AES-GCM,
      round-trip, tamper, wrong-passphrase, KAT).
- [ ] `BackupService.kt` (identity + snapshot + SAF read/write, off-main-thread).
- [ ] `AppStore.close()` (only if we go the fallback snapshot route).
- [ ] Settings entry: "Back up account" + passphrase-set UI + strength meter +
      warning copy.
- [ ] Onboarding entry: "Restore from backup" branch + passphrase-enter UI.
- [ ] Decide KDF dependency (Argon2id lib vs scrypt/BouncyCastle vs PBKDF2).
- [ ] Update DESIGN.md §6.2 to note the deliberate key-export relaxation.
