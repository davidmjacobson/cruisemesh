# Phase 2 chat IPC addendum

Phase 2 keeps `cruisemesh-node.exe` as the only identity, SQLite, transport,
and relay owner. `CruiseMesh.exe` is a Tauri/Fluent presentation process. It
attaches to the current-user-only `CruiseMeshNode` named pipe and starts the
adjacent node only when no owner exists. The UI never opens `messages.db` and
the Tauri crate does not link `cruisemesh-core`.

## Boundary

Every request is a tagged JSON line. The node rejects unknown commands and
fields, caps requests at 512 KiB, validates typed `person:`/`group:` IDs, and
caps initial chat history at the newest 50 stored rows. That bound keeps even
attachment-heavy histories below the UI host's 16 MiB response cap after JSON
and base64 expansion. Attachments remain under the core 180 KiB blob ceiling.

Read commands:

- `GetProtocolInfo` (currently protocol version 2)
- `GetAppSnapshot`
- `GetConversation { conversation_id }`
- the frozen Stage 1 status/card/event commands

Mutation commands:

- `SendText`, `SendAttachment`, `React`, `MarkRead`
- `CreateGroup`, `SetProfile`
- `ImportFriendCard`, `ImportRelaySetup`
- `CreateBackup`, `PreviewBackup`, `StageRestore`

The node maps frontend drafts to existing core authoring APIs. TypeScript never
constructs message bodies, envelopes, hints, signatures, receipts, group keys,
or relay documents. New authoring records its message ID in the shared inbound
dedupe authority, attempts the authenticated LAN route, and independently
nudges the core relay pass.

## Restore

`StageRestore` reads no more than the core backup cap, authenticates the
`.cmbak`, decodes its identity, opens/migrates a private SQLite copy, inspects
it, and runs core restore sanitization. Only then is a ready marker published.
The helper exits after replying. Before the next `BootstrapStore` opens,
staged files are renamed into place and every prior live identity/database
file is retained in a timestamped `restore-previous-*` directory. A partial or
invalid stage never touches the live store.
