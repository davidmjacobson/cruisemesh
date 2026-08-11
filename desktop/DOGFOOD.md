# CruiseMesh for Windows — dogfood

This unsigned Phase 2 build contains:

- `CruiseMesh.exe` — the Fluent messenger window.
- `cruisemesh-node.exe` — the always-on tray helper that owns identity,
  SQLite, LAN, relay, and delay-tolerant carry.

Keep both files in the same folder. Windows SmartScreen may warn because the
dogfood archive is intentionally unsigned.

## Start

Run `CruiseMesh.exe`. It attaches to an existing helper or starts the adjacent
`cruisemesh-node.exe` with no terminal. Closing the window leaves the tray node
running so it can continue receiving and carrying messages.

When upgrading from Phase 1, its older helper may still own the named pipe.
The window will ask you to right-click the **CruiseMesh Helper** tray icon and
confirm **Quit**. Leave the window open; within a few seconds it starts the
updated adjacent helper automatically. Your identity and message database stay
in place.

The window supports friend QR display and webcam scanning, pasted friend/deep
links, 1:1 and group text, replies, reactions, photos, voice recordings,
delivery/read ticks, local notifications, Shore Pass setup, connection status,
profile/fingerprint display, and encrypted `.cmbak` backup/restore.

Restoring an identity is a migration, not multi-device sync: retire the old
phone or PC before using the restored identity. Restore is decrypted,
inspected, and sanitized in a staging directory first. On restart, the prior
Windows identity and database are kept under
`%LOCALAPPDATA%\CruiseMesh\restore-previous-*` for recovery.

## Initial setup and diagnostics

The UI can import a Shore Pass and add friends. The CLI remains useful for
foreground diagnostics:

```powershell
$env:RUST_LOG='cruisemesh_node=info'
.\cruisemesh-node.exe run --foreground
```

Install login/crash-restart behavior:

```powershell
.\cruisemesh-node.exe install-autostart
```

Ship or hotel Wi-Fi is commonly classified Public. After deciding that inbound
CruiseMesh connections should be allowed on both Private and Public networks,
open an elevated PowerShell and run:

```powershell
.\cruisemesh-node.exe allow-firewall
```

The helper listens on TCP 45892 when available. LAN sessions still require an
accepted contact's exact Noise key. Keep the lid open (or configure “Do
nothing” on AC). The helper prevents automatic system sleep only while plugged
in.

Local state is under `%LOCALAPPDATA%\CruiseMesh`. Identity and relay member
credentials are protected with current-user DPAPI. The WebView cannot open the
database or access keys/tokens. Status and logs never print tokens, message
bodies, contact IDs, relay URLs, or LAN addresses.
