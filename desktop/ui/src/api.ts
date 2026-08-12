import { invoke } from "@tauri-apps/api/core";
import type {
  AppSnapshot,
  AttachmentDraft,
  Conversation,
  FriendPreview,
  Message,
} from "./types";

export const api = {
  snapshot: () => invoke<AppSnapshot>("get_app_snapshot"),
  conversation: (conversationId: string, beforeTimestampMs?: number) =>
    invoke<Conversation>("get_conversation", {
      conversationId,
      beforeTimestampMs,
    }),
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
  previewFriend: (text: string) => invoke<FriendPreview>("preview_friend", { text }),
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
  setProfilePhoto: (dataBase64: string) =>
    invoke("set_profile_photo", { dataBase64 }),
  setPreferences: (
    preventSleepOnAc: boolean,
    shareOnline: boolean,
    friendsOfFriends?: boolean,
  ) =>
    invoke("set_preferences", {
      preventSleepOnAc,
      shareOnline,
      friendsOfFriends,
    }),
  deleteContact: (conversationId: string) =>
    invoke("delete_contact", { conversationId }),
  setNickname: (conversationId: string, nickname?: string) =>
    invoke("set_nickname", { conversationId, nickname }),
  setBlocked: (conversationId: string, blocked: boolean) =>
    invoke("set_blocked", { conversationId, blocked }),
  setMuted: (conversationId: string, muted: boolean) =>
    invoke("set_muted", { conversationId, muted }),
  reportContact: (conversationId: string) =>
    invoke<{ mailto: string; address: string }>("report_contact", { conversationId }),
  renameGroup: (conversationId: string, name: string) =>
    invoke("rename_group", { conversationId, name }),
  addGroupMembers: (conversationId: string, memberIds: string[]) =>
    invoke("add_group_members", { conversationId, memberIds }),
  shareContact: (conversationId: string) =>
    invoke<{ name: string; code: string }>("share_contact", { conversationId }),
  acceptPendingShared: (requesterId: string) =>
    invoke<{ name: string }>("accept_pending_shared", { requesterId }),
  dismissPendingShared: (requesterId: string, suppress: boolean) =>
    invoke("dismiss_pending_shared", { requesterId, suppress }),
  acceptTerms: () => invoke("accept_terms"),
  initialActivation: () => invoke<string[]>("initial_activation"),
};

export interface BackupPreview {
  created_at_ms: number;
  display_name?: string;
  inventory: { contacts: number; groups: number; messages: number };
}
