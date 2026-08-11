# CruiseMesh Helper — Windows dogfood

This unsigned Stage 1 build is a headless LAN, relay, and delay-tolerant carry
node. It has its own identity; do not restore a phone backup onto it. Windows
SmartScreen may warn because the dogfood archive is intentionally unsigned.

## Set up

Open PowerShell in the extracted folder:

```powershell
.\cruisemesh-node.exe import-relay '<Shore Pass text or link>'
.\cruisemesh-node.exe show-card
```

Scan the printed friend link on each phone while internet is available. For a
fully offline setup, export each phone's friend card and import it with:

```powershell
.\cruisemesh-node.exe import-friend '<phone friend card or link>'
```

Run once in the foreground to inspect logs:

```powershell
$env:RUST_LOG='cruisemesh_node=info'
.\cruisemesh-node.exe run --foreground
```

Then install login/crash-restart behavior:

```powershell
.\cruisemesh-node.exe install-autostart
```

Ship or hotel Wi-Fi is commonly classified Public. After deciding that inbound
CruiseMesh connections should be allowed on both Private and Public networks,
open an elevated PowerShell and run:

```powershell
.\cruisemesh-node.exe allow-firewall
```

The helper listens on TCP 45892 when available. It advertises only an opaque
DNS-SD token, protocol version, and port; LAN sessions still require an
accepted contact's exact Noise key. Keep the lid open (or configure “Do
nothing” on AC). The helper prevents automatic system sleep only while plugged
in.

Local state is under `%LOCALAPPDATA%\CruiseMesh`. Identity and the relay member
credential are protected with current-user DPAPI. `status` and logs never print
tokens, message bodies, contact IDs, relay URLs, or LAN addresses.
