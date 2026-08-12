import {
  Avatar,
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  MessageBar,
  MessageBarBody,
  Spinner,
  Text,
  Textarea,
  Tooltip,
} from "@fluentui/react-components";
import {
  Add24Regular,
  ArrowReply24Regular,
  Attach24Regular,
  Chat24Regular,
  Checkmark16Regular,
  CheckmarkCircle16Regular,
  Dismiss16Regular,
  Group24Regular,
  Mic24Regular,
  Navigation24Regular,
  QrCode24Regular,
  Settings24Regular,
} from "@fluentui/react-icons";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { QRCodeSVG } from "qrcode.react";
import { FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { api } from "./api";
import { prepareAttachment } from "./media";
import { friendWebLink, kindNumber, tickLabel } from "./presentation";
import type {
  AppSnapshot,
  Conversation,
  ConversationSummary,
  Message,
  Tick,
} from "./types";

type DialogName = "friend" | "card" | "group" | null;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function formatTime(timestamp?: number): string {
  if (!timestamp) return "";
  const date = new Date(timestamp);
  const today = new Date();
  return date.toDateString() === today.toDateString()
    ? date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
    : date.toLocaleDateString([], { month: "short", day: "numeric" });
}

function TickIcon({ tick }: { tick?: Tick }) {
  if (!tick) return null;
  return (
    <span className={`tick tick-${tick}`} aria-label={tickLabel(tick)} title={tickLabel(tick)}>
      {tick === "sent" ? <Checkmark16Regular /> : <CheckmarkCircle16Regular />}
    </span>
  );
}

export function App() {
  const [snapshot, setSnapshot] = useState<AppSnapshot>();
  const [selectedId, setSelectedId] = useState<string>();
  const [conversation, setConversation] = useState<Conversation>();
  const [settings, setSettings] = useState(false);
  const [dialog, setDialog] = useState<DialogName>(null);
  const [friendText, setFriendText] = useState("");
  const [groupName, setGroupName] = useState("");
  const [groupMembers, setGroupMembers] = useState<Set<string>>(new Set());
  const [relayText, setRelayText] = useState("");
  const [backupPassphrase, setBackupPassphrase] = useState("");
  const [profileName, setProfileName] = useState("");
  const [draft, setDraft] = useState("");
  const [reply, setReply] = useState<Message>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const previousUnread = useRef<Map<string, number> | undefined>(undefined);
  const messagesEnd = useRef<HTMLDivElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  const refreshSnapshot = useCallback(async (notify = false) => {
    try {
      const next = await api.snapshot();
      if (notify && previousUnread.current) {
        for (const row of next.conversations) {
          const old = previousUnread.current.get(row.id) ?? 0;
          if (
            row.unread_count > old &&
            row.id !== selectedId &&
            (!document.hasFocus() || document.hidden)
          ) {
            let granted = await isPermissionGranted();
            if (!granted) granted = (await requestPermission()) === "granted";
            if (granted) sendNotification({ title: row.title, body: row.preview || "New message" });
          }
        }
      }
      previousUnread.current = new Map(next.conversations.map((row) => [row.id, row.unread_count]));
      setSnapshot(next);
      setProfileName((current) => current || next.profile.display_name);
      setError(undefined);
      if (!selectedId && next.conversations.length) setSelectedId(next.conversations[0].id);
    } catch (nextError) {
      setError(errorText(nextError));
    }
  }, [selectedId]);

  const refreshConversation = useCallback(async () => {
    if (!selectedId) {
      setConversation(undefined);
      return;
    }
    try {
      const next = await api.conversation(selectedId);
      setConversation(next);
      if (next.kind === "person") await api.markRead(next.id);
    } catch (nextError) {
      setError(errorText(nextError));
    }
  }, [selectedId]);

  useEffect(() => {
    void refreshSnapshot();
    const timer = window.setInterval(() => void refreshSnapshot(true), 3_000);
    return () => window.clearInterval(timer);
  }, [refreshSnapshot]);

  useEffect(() => {
    void refreshConversation();
    const timer = window.setInterval(() => void refreshConversation(), 2_000);
    return () => window.clearInterval(timer);
  }, [refreshConversation]);

  useEffect(() => {
    messagesEnd.current?.scrollIntoView({ block: "end" });
  }, [conversation?.messages.length, selectedId]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    const activate = (args: string[]) => {
      const link = args.find((value) => value.toLowerCase().startsWith("cruisemesh:"));
      if (link) {
        setFriendText(link);
        setDialog("friend");
      }
    };
    void api.initialActivation().then(activate).catch((nextError) => setError(errorText(nextError)));
    void listen<string[]>("activation", (event) => activate(event.payload))
      .then((value) => (unlisten = value));
    return () => unlisten?.();
  }, []);

  async function send(event: FormEvent) {
    event.preventDefault();
    if (!selectedId || !draft.trim() || busy) return;
    setBusy(true);
    try {
      await api.sendText(selectedId, draft, reply?.id);
      setDraft("");
      setReply(undefined);
      await Promise.all([refreshConversation(), refreshSnapshot()]);
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
    }
  }

  async function attach(file?: File) {
    if (!file || !selectedId || !snapshot) return;
    setBusy(true);
    try {
      const prepared = await prepareAttachment(file, snapshot.attachment_max_blob_bytes);
      await api.sendAttachment(selectedId, prepared, reply?.id);
      setReply(undefined);
      await Promise.all([refreshConversation(), refreshSnapshot()]);
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
      if (fileInput.current) fileInput.current.value = "";
    }
  }

  async function addFriend() {
    setBusy(true);
    try {
      await api.importFriend(friendText);
      setFriendText("");
      setDialog(null);
      await refreshSnapshot();
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
    }
  }

  async function createGroup() {
    setBusy(true);
    try {
      const result = await api.createGroup(groupName, [...groupMembers]);
      setGroupName("");
      setGroupMembers(new Set());
      setDialog(null);
      await refreshSnapshot();
      setSelectedId(result.conversation_id);
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
    }
  }

  async function importRelay() {
    setBusy(true);
    try {
      await api.importRelay(relayText);
      setRelayText("");
      await refreshSnapshot();
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
    }
  }

  async function exportBackup() {
    if (backupPassphrase.length < 10) {
      setError("Backup passphrase must be at least 10 characters.");
      return;
    }
    const path = await save({
      defaultPath: "cruisemesh-backup.cmbak",
      filters: [{ name: "CruiseMesh backup", extensions: ["cmbak"] }],
    });
    if (!path) return;
    setBusy(true);
    try {
      const result = await api.createBackup(path, backupPassphrase);
      setError(undefined);
      window.alert(`Encrypted backup saved (${Math.ceil(result.bytes_written / 1024)} KiB).`);
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
    }
  }

  async function restoreBackup() {
    if (backupPassphrase.length < 10) {
      setError("Enter the backup passphrase first.");
      return;
    }
    const path = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "CruiseMesh backup", extensions: ["cmbak"] }],
    });
    if (!path) return;
    setBusy(true);
    try {
      const preview = await api.previewBackup(path, backupPassphrase);
      const accepted = window.confirm(
        `Restore ${preview.display_name || "this identity"}?\n\n` +
          `${preview.inventory.contacts} contacts, ${preview.inventory.groups} groups, ` +
          `${preview.inventory.messages} messages.\n\n` +
          "CruiseMesh will restart. The current identity and database will be retained in a recovery folder.",
      );
      if (!accepted) return;
      await api.stageRestore(path, backupPassphrase);
      setConversation(undefined);
      setSnapshot(undefined);
      window.setTimeout(() => void refreshSnapshot(), 1_500);
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
    }
  }

  async function updateProfile() {
    if (!profileName.trim()) return;
    setBusy(true);
    try {
      await api.setProfile(profileName);
      await refreshSnapshot();
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      setBusy(false);
    }
  }

  async function react(message: Message, emoji: string) {
    if (!selectedId) return;
    try {
      await api.react(
        selectedId,
        { sender_id: message.sender_id, lamport: message.lamport, kind: kindNumber(message) },
        emoji,
      );
      await refreshConversation();
    } catch (nextError) {
      setError(errorText(nextError));
    }
  }

  if (!snapshot) {
    return (
      <main className="loading" aria-live="polite">
        {!error && <Spinner label="Starting CruiseMesh…" />}
        {error && (
          <>
            <Text size={600} weight="semibold">CruiseMesh Helper needs attention</Text>
            <MessageBar intent="warning"><MessageBarBody>{error}</MessageBarBody></MessageBar>
            <Button appearance="primary" onClick={() => void refreshSnapshot()}>Try again</Button>
          </>
        )}
      </main>
    );
  }

  return (
    <div className="app-shell">
      <aside className="rail" aria-label="Primary navigation">
        <Button
          appearance={!settings ? "primary" : "subtle"}
          icon={<Chat24Regular />}
          aria-label="Conversations"
          onClick={() => setSettings(false)}
        />
        <Button
          appearance={settings ? "primary" : "subtle"}
          icon={<Settings24Regular />}
          aria-label="Settings and connection details"
          onClick={() => setSettings(true)}
        />
      </aside>

      <aside className="conversation-pane" aria-label="Conversations">
        <header className="pane-header">
          <div>
            <Text size={600} weight="semibold">CruiseMesh</Text>
            <div className="node-line">
              <span className={`presence ${snapshot.lan_peers ? "online" : ""}`} />
              {snapshot.lan_peers ? `${snapshot.lan_peers} nearby` : snapshot.node.relay_configured ? "Shore Pass ready" : "Local-only"}
            </div>
          </div>
          <div className="header-actions">
            <Tooltip content="Add friend" relationship="label">
              <Button appearance="subtle" icon={<Add24Regular />} onClick={() => setDialog("friend")} />
            </Tooltip>
            <Tooltip content="New group" relationship="label">
              <Button appearance="subtle" icon={<Group24Regular />} onClick={() => setDialog("group")} />
            </Tooltip>
          </div>
        </header>
        <nav className="conversation-list">
          {snapshot.conversations.length === 0 ? (
            <div className="empty-list">
              <Navigation24Regular />
              <Text weight="semibold">Your conversations start here</Text>
              <Text size={200}>Add a friend by QR code or link.</Text>
              <Button appearance="primary" onClick={() => setDialog("friend")}>Add friend</Button>
            </div>
          ) : snapshot.conversations.map((row) => (
            <ConversationRow
              key={row.id}
              row={row}
              selected={row.id === selectedId && !settings}
              onClick={() => { setSettings(false); setSelectedId(row.id); setReply(undefined); }}
            />
          ))}
        </nav>
      </aside>

      <main className="content-pane">
        {error && (
          <MessageBar intent="error" className="error-bar">
            <MessageBarBody>{error}</MessageBarBody>
            <Button appearance="transparent" icon={<Dismiss16Regular />} aria-label="Dismiss error" onClick={() => setError(undefined)} />
          </MessageBar>
        )}
        {settings ? (
          <SettingsView
            snapshot={snapshot}
            relayText={relayText}
            setRelayText={setRelayText}
            importRelay={importRelay}
            showCard={() => setDialog("card")}
            busy={busy}
            backupPassphrase={backupPassphrase}
            setBackupPassphrase={setBackupPassphrase}
            exportBackup={() => void exportBackup()}
            restoreBackup={() => void restoreBackup()}
            profileName={profileName}
            setProfileName={setProfileName}
            updateProfile={() => void updateProfile()}
          />
        ) : conversation ? (
          <section className="chat" aria-label={`Conversation with ${conversation.title}`}>
            <header className="chat-header">
              <Avatar name={conversation.title} color="colorful" />
              <div>
                <Text size={500} weight="semibold">{conversation.title}</Text>
                <div className="subtle">
                  {conversation.kind === "group" ? `${conversation.member_count} members` : selectedSummary(snapshot, conversation.id)?.connected_lan ? "Nearby on Wi-Fi" : "CruiseMesh contact"}
                </div>
                {conversation.kind === "person" && <div className="fingerprint compact" aria-label={`Fingerprint ${snapshot.contacts.find((contact) => contact.id === conversation.id)?.fingerprint_words.join(" ") || "unavailable"}`}>{snapshot.contacts.find((contact) => contact.id === conversation.id)?.fingerprint_words.join(" · ")}</div>}
              </div>
            </header>
            <div className="message-list" role="log" aria-live="polite">
              {conversation.messages.length === 0 && (
                <div className="empty-chat">
                  <Avatar name={conversation.title} size={72} color="colorful" />
                  <Text size={500} weight="semibold">Start a private conversation</Text>
                  <Text>The node will choose Wi-Fi or Shore Pass automatically.</Text>
                </div>
              )}
              {conversation.messages.map((message) => (
                <MessageBubble
                  key={`${message.sender_id}:${message.lamport}`}
                  message={message}
                  messages={conversation.messages}
                  onReply={() => setReply(message)}
                  onReact={(emoji) => void react(message, emoji)}
                />
              ))}
              <div ref={messagesEnd} />
            </div>
            <form className="composer" onSubmit={send}>
              {reply && (
                <div className="reply-banner">
                  <ArrowReply24Regular />
                  <div><strong>Replying to {reply.sender_name}</strong><br />{reply.text || (reply.kind === "image" ? "Photo" : "Voice message")}</div>
                  <Button appearance="subtle" icon={<Dismiss16Regular />} aria-label="Cancel reply" onClick={() => setReply(undefined)} />
                </div>
              )}
              <div className="composer-row">
                <input
                  ref={fileInput}
                  className="visually-hidden"
                  type="file"
                  accept="image/*,audio/*"
                  onChange={(event) => void attach(event.target.files?.[0])}
                />
                <Tooltip content="Attach photo or recording" relationship="label">
                  <Button type="button" appearance="subtle" icon={<Attach24Regular />} disabled={busy} onClick={() => fileInput.current?.click()} />
                </Tooltip>
                <VoiceButton disabled={busy} onRecorded={(file) => void attach(file)} onError={(value) => setError(value)} />
                <Textarea
                  aria-label="Message"
                  placeholder="Message"
                  resize="none"
                  value={draft}
                  onChange={(_, data) => setDraft(data.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && !event.shiftKey) {
                      event.preventDefault();
                      event.currentTarget.form?.requestSubmit();
                    }
                  }}
                />
                <Button type="submit" appearance="primary" disabled={busy || !draft.trim()}>Send</Button>
              </div>
            </form>
          </section>
        ) : (
          <div className="empty-chat"><Chat24Regular fontSize={48} /><Text size={500}>Choose a conversation</Text></div>
        )}
      </main>

      <FriendDialog
        open={dialog === "friend"}
        text={friendText}
        setText={setFriendText}
        busy={busy}
        onClose={() => setDialog(null)}
        onImport={() => void addFriend()}
        onShowCard={() => setDialog("card")}
      />
      <CardDialog open={dialog === "card"} snapshot={snapshot} onClose={() => setDialog(null)} />
      <GroupDialog
        open={dialog === "group"}
        snapshot={snapshot}
        name={groupName}
        setName={setGroupName}
        members={groupMembers}
        setMembers={setGroupMembers}
        busy={busy}
        onClose={() => setDialog(null)}
        onCreate={() => void createGroup()}
      />
    </div>
  );
}

function selectedSummary(snapshot: AppSnapshot, id: string) {
  return snapshot.conversations.find((row) => row.id === id);
}

function ConversationRow({ row, selected, onClick }: { row: ConversationSummary; selected: boolean; onClick: () => void }) {
  return (
    <button className={`conversation-row ${selected ? "selected" : ""}`} onClick={onClick} aria-current={selected ? "page" : undefined}>
      <Avatar name={row.title} color="colorful" badge={row.connected_lan ? { status: "available" } : undefined} />
      <span className="conversation-copy">
        <span className="conversation-title"><strong>{row.title}</strong><span>{formatTime(row.timestamp_ms)}</span></span>
        <span className="conversation-preview"><TickIcon tick={row.tick} />{row.preview || (row.kind === "group" ? `${row.member_count} members` : "No messages yet")}</span>
      </span>
      {row.unread_count > 0 && <Badge appearance="filled" color="brand" aria-label={`${row.unread_count} unread`}>{row.unread_count}</Badge>}
    </button>
  );
}

function MessageBubble({ message, messages, onReply, onReact }: { message: Message; messages: Message[]; onReply: () => void; onReact: (emoji: string) => void }) {
  const replied = message.reply_to_id ? messages.find((item) => item.id === message.reply_to_id) : undefined;
  const source = message.attachment ? `data:${message.attachment.mime_type};base64,${message.attachment.data_base64}` : "";
  const [imageFailed, setImageFailed] = useState(false);
  return (
    <article className={`message ${message.own ? "own" : "other"}`}>
      {!message.own && <span className="sender-name">{message.sender_name}</span>}
      <div className="bubble">
        {replied && <div className="quoted"><strong>{replied.sender_name}</strong><br />{replied.text || (replied.kind === "image" ? "Photo" : "Voice message")}</div>}
        {message.kind === "text" && <div className="message-text">{message.text}</div>}
        {message.kind === "image" && !imageFailed && <img className="attachment-image" src={source} alt={message.attachment?.caption || "Shared photo"} onError={() => setImageFailed(true)} />}
        {message.kind === "image" && imageFailed && <div className="attachment-error">This photo could not be displayed.</div>}
        {message.kind === "audio" && <audio controls preload="metadata" src={source}>Voice message</audio>}
        {message.kind === "group_invite" && <div>Group created</div>}
        {message.kind === "unsupported_attachment" && <div className="attachment-error">{message.text || "This attachment could not be displayed."}</div>}
        {message.attachment?.caption && <div>{message.attachment.caption}</div>}
        <div className="message-meta"><time>{formatTime(message.timestamp_ms)}</time><TickIcon tick={message.tick} /></div>
      </div>
      {message.reactions.length > 0 && <div className="reaction-list">{message.reactions.map((reaction) => <Badge key={reaction.emoji} appearance={reaction.own ? "filled" : "outline"}>{reaction.emoji} {reaction.count}</Badge>)}</div>}
      <div className="message-actions">
        {["👍", "❤️", "😂"].map((emoji) => <button key={emoji} aria-label={`React ${emoji}`} onClick={() => onReact(emoji)}>{emoji}</button>)}
        <button aria-label="Reply" onClick={onReply}><ArrowReply24Regular /></button>
      </div>
    </article>
  );
}

function VoiceButton({ disabled, onRecorded, onError }: { disabled: boolean; onRecorded: (file: File) => void; onError: (error: string) => void }) {
  const [recording, setRecording] = useState(false);
  const recorder = useRef<MediaRecorder | undefined>(undefined);
  const chunks = useRef<Blob[]>([]);
  async function toggle() {
    if (recorder.current && recording) {
      recorder.current.stop();
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const next = new MediaRecorder(stream);
      chunks.current = [];
      next.ondataavailable = (event) => { if (event.data.size) chunks.current.push(event.data); };
      next.onstop = () => {
        const blob = new Blob(chunks.current, { type: next.mimeType || "audio/webm" });
        stream.getTracks().forEach((track) => track.stop());
        setRecording(false);
        onRecorded(new File([blob], "voice-message.webm", { type: blob.type }));
      };
      next.start();
      recorder.current = next;
      setRecording(true);
    } catch (error) {
      onError(errorText(error));
    }
  }
  return <Tooltip content={recording ? "Stop recording" : "Record voice message"} relationship="label"><Button type="button" appearance={recording ? "primary" : "subtle"} icon={<Mic24Regular />} disabled={disabled} onClick={() => void toggle()} /></Tooltip>;
}

function SettingsView({ snapshot, relayText, setRelayText, importRelay, showCard, busy, backupPassphrase, setBackupPassphrase, exportBackup, restoreBackup, profileName, setProfileName, updateProfile }: { snapshot: AppSnapshot; relayText: string; setRelayText: (value: string) => void; importRelay: () => void; showCard: () => void; busy: boolean; backupPassphrase: string; setBackupPassphrase: (value: string) => void; exportBackup: () => void; restoreBackup: () => void; profileName: string; setProfileName: (value: string) => void; updateProfile: () => void }) {
  return (
    <section className="settings-view" aria-labelledby="settings-title">
      <header><Text id="settings-title" size={700} weight="semibold">Profile & settings</Text><Text>Your identity stays on this Windows account.</Text></header>
      <div className="settings-card"><Avatar name={snapshot.profile.display_name} size={64} color="colorful" /><div><Input value={profileName} onChange={(_, data) => setProfileName(data.value)} aria-label="Display name" /><div className="fingerprint" aria-label={`Fingerprint ${snapshot.profile.fingerprint_words.join(" ")}`}>{snapshot.profile.fingerprint_words.join(" · ")}</div><div className="subtle">{snapshot.profile.formatted_user_id}</div></div><Button appearance="secondary" disabled={busy || !profileName.trim() || profileName.trim() === snapshot.profile.display_name} onClick={updateProfile}>Save profile</Button><Button icon={<QrCode24Regular />} onClick={showCard}>My friend card</Button></div>
      <div className="settings-card column"><Text size={500} weight="semibold">Connection details</Text><div className="detail-grid"><span>Nearby Wi-Fi peers</span><strong>{snapshot.lan_peers}</strong><span>Internet delivery</span><strong>{snapshot.node.relay_configured ? "Ready" : "Not configured"}</strong><span>Contacts</span><strong>{snapshot.node.contacts}</strong></div></div>
      <div className="settings-card column"><Text size={500} weight="semibold">Shore Pass</Text><Text>Paste a CMRELAY1 card or cruisemesh.app relay setup link. The member credential is protected with Windows DPAPI and never exposed to this window.</Text><Textarea value={relayText} onChange={(_, data) => setRelayText(data.value)} placeholder="Paste Shore Pass" /><Button appearance="primary" disabled={busy || !relayText.trim()} onClick={importRelay}>Import Shore Pass</Button></div>
      <div className="settings-card column"><Text size={500} weight="semibold">Encrypted backup & restore</Text><Text>Portable .cmbak files can migrate this identity between Android, iOS, and Windows. Do not run the restored identity on two devices at once.</Text><Input type="password" value={backupPassphrase} onChange={(_, data) => setBackupPassphrase(data.value)} placeholder="Passphrase (10+ characters)" aria-label="Backup passphrase" /><div className="settings-actions"><Button appearance="primary" disabled={busy || backupPassphrase.length < 10} onClick={exportBackup}>Save encrypted backup</Button><Button appearance="secondary" disabled={busy || backupPassphrase.length < 10} onClick={restoreBackup}>Restore from backup</Button></div></div>
      <div className="settings-card column"><Text size={500} weight="semibold">Advanced</Text><Text>The helper keeps running from the tray when this window closes. Diagnostics never include names, message bodies, keys, or relay credentials.</Text></div>
    </section>
  );
}

function FriendDialog({ open, text, setText, busy, onClose, onImport, onShowCard }: { open: boolean; text: string; setText: (value: string) => void; busy: boolean; onClose: () => void; onImport: () => void; onShowCard: () => void }) {
  const [scanning, setScanning] = useState(false);
  return <Dialog open={open} onOpenChange={(_, data) => { if (!data.open) { setScanning(false); onClose(); } }}><DialogSurface><DialogBody><DialogTitle>Add a friend</DialogTitle><DialogContent className="dialog-stack"><Text>Scan their CruiseMesh QR code, or paste a friend link or CMFRIEND card.</Text>{scanning ? <CameraScanner onValue={(value) => { setText(value); setScanning(false); }} onCancel={() => setScanning(false)} /> : <Button appearance="secondary" icon={<QrCode24Regular />} onClick={() => setScanning(true)}>Scan with webcam</Button>}<Textarea autoFocus={!scanning} value={text} onChange={(_, data) => setText(data.value)} placeholder="https://cruisemesh.app/f#CMFRIEND3:…" /><Button appearance="subtle" icon={<QrCode24Regular />} onClick={onShowCard}>Show my card instead</Button></DialogContent><DialogActions><Button appearance="secondary" onClick={onClose}>Cancel</Button><Button appearance="primary" disabled={busy || !text.trim()} onClick={onImport}>Add friend</Button></DialogActions></DialogBody></DialogSurface></Dialog>;
}

function CameraScanner({ onValue, onCancel }: { onValue: (value: string) => void; onCancel: () => void }) {
  const video = useRef<HTMLVideoElement>(null);
  const onValueRef = useRef(onValue);
  const [cameraError, setCameraError] = useState<string>();
  useEffect(() => {
    onValueRef.current = onValue;
  }, [onValue]);
  useEffect(() => {
    let stopped = false;
    let stream: MediaStream | undefined;
    let timer = 0;
    async function start() {
      const Detector = (window as unknown as { BarcodeDetector?: new (options: { formats: string[] }) => { detect(source: HTMLVideoElement): Promise<Array<{ rawValue: string }>> } }).BarcodeDetector;
      if (!Detector) {
        setCameraError("This WebView2 version does not provide QR scanning. Paste the link instead.");
        return;
      }
      try {
        stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" } });
        if (!video.current || stopped) return;
        video.current.srcObject = stream;
        await video.current.play();
        const detector = new Detector({ formats: ["qr_code"] });
        const scan = async () => {
          if (stopped || !video.current) return;
          try {
            const values = await detector.detect(video.current);
            if (values[0]?.rawValue) {
              onValueRef.current(values[0].rawValue);
              return;
            }
          } catch {
            // A frame can be unavailable while the camera warms up; retry.
          }
          timer = window.setTimeout(() => void scan(), 250);
        };
        void scan();
      } catch (error) {
        setCameraError(errorText(error));
      }
    }
    void start();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
      stream?.getTracks().forEach((track) => track.stop());
    };
  }, []);
  return <div className="camera-scanner">{cameraError ? <MessageBar intent="warning"><MessageBarBody>{cameraError}</MessageBarBody></MessageBar> : <video ref={video} muted playsInline aria-label="Webcam QR scanner" />}<Button appearance="subtle" onClick={onCancel}>Stop scanning</Button></div>;
}

function CardDialog({ open, snapshot, onClose }: { open: boolean; snapshot: AppSnapshot; onClose: () => void }) {
  const webLink = friendWebLink(snapshot.profile.friend_link);
  return <Dialog open={open} onOpenChange={(_, data) => { if (!data.open) onClose(); }}><DialogSurface><DialogBody><DialogTitle>My friend card</DialogTitle><DialogContent className="card-dialog"><div className="qr"><QRCodeSVG value={webLink} size={236} level="M" marginSize={2} /></div><Text>Scan with CruiseMesh, or share this link:</Text><Input readOnly value={webLink} aria-label="Friend link" /></DialogContent><DialogActions><Button appearance="primary" onClick={onClose}>Done</Button></DialogActions></DialogBody></DialogSurface></Dialog>;
}

function GroupDialog({ open, snapshot, name, setName, members, setMembers, busy, onClose, onCreate }: { open: boolean; snapshot: AppSnapshot; name: string; setName: (value: string) => void; members: Set<string>; setMembers: (value: Set<string>) => void; busy: boolean; onClose: () => void; onCreate: () => void }) {
  return <Dialog open={open} onOpenChange={(_, data) => { if (!data.open) onClose(); }}><DialogSurface><DialogBody><DialogTitle>New group</DialogTitle><DialogContent className="dialog-stack"><Input value={name} onChange={(_, data) => setName(data.value)} placeholder="Group name" aria-label="Group name" />{snapshot.contacts.map((contact) => <Checkbox key={contact.id} checked={members.has(contact.id)} label={contact.display_name} onChange={(_, data) => { const next = new Set(members); data.checked ? next.add(contact.id) : next.delete(contact.id); setMembers(next); }} />)}{snapshot.contacts.length === 0 && <Text>Add a friend before creating a group.</Text>}</DialogContent><DialogActions><Button appearance="secondary" onClick={onClose}>Cancel</Button><Button appearance="primary" disabled={busy || !name.trim() || members.size === 0} onClick={onCreate}>Create</Button></DialogActions></DialogBody></DialogSurface></Dialog>;
}
