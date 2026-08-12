import { invoke } from "@tauri-apps/api/core";
import type { AppSnapshot, AttachmentDraft, Conversation, Message } from "./types";

export const api = {
  snapshot: () => invoke<AppSnapshot>("get_app_snapshot"),
  conversation: (conversationId: string) =>
    invoke<Conversation>("get_conversation", { conversationId }),
  sendText: (conversationId: string, text: string, replyToId?: string) =>
    invoke<Message>("send_text", { conversationId, text, replyToId }),
  sendAttachment: (
    conversationId: string,
    draft: AttachmentDraft,
    replyToId?: string,
  ) => invoke<Message>("send_attachment", { conversationId, draft, replyToId }),
  react: (
    conversationId: string,
    target: { sender_id: string; lamport: number; kind: number },
    emoji: string,
  ) => invoke<void>("react", { conversationId, target, emoji }),
  markRead: (conversationId: string) =>
    invoke<{ advanced: boolean }>("mark_read", { conversationId }),
  createGroup: (name: string, memberIds: string[]) =>
    invoke<{ conversation_id: string }>("create_group", { name, memberIds }),
  importFriend: (text: string) =>
    invoke<{ name: string }>("import_friend", { text }),
  importRelay: (text: string) =>
    invoke<{ imported: boolean }>("import_relay", { text }),
  createBackup: (path: string, passphrase: string) =>
    invoke<{ bytes_written: number }>("create_backup", { path, passphrase }),
  previewBackup: (path: string, passphrase: string) =>
    invoke<BackupPreview>("preview_backup", { path, passphrase }),
  stageRestore: (path: string, passphrase: string) =>
    invoke<BackupPreview>("stage_restore", { path, passphrase }),
  setProfile: (displayName: string) =>
    invoke("set_profile", { displayName }),
  setPreferences: (preventSleepOnAc: boolean, shareOnline: boolean) =>
    invoke("set_preferences", { preventSleepOnAc, shareOnline }),
  initialActivation: () => invoke<string[]>("initial_activation"),
};

export interface BackupPreview {
  created_at_ms: number;
  display_name?: string;
  inventory: { contacts: number; groups: number; messages: number };
}
