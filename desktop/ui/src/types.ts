export type Tick = "sent" | "delivered" | "read";
export type ConversationKind = "person" | "group";

export interface Profile {
  display_name: string;
  friend_link: string;
  fingerprint_words: string[];
}

export interface NodeStatus {
  relay_configured: boolean;
  contacts: number;
  reduced_mode: boolean;
}

export interface Preferences {
  prevent_sleep_on_ac: boolean;
  share_online: boolean;
}

export interface Diagnostics {
  helper_version: string;
  listening_port: number;
  data_directory: string;
  logs_directory: string;
}

export interface Contact {
  id: string;
  display_name: string;
  connected_lan: boolean;
  internet_delivery_configured: boolean;
  fingerprint_words: string[];
}

export interface ConversationSummary {
  id: string;
  kind: ConversationKind;
  title: string;
  member_count: number;
  connected_lan: boolean;
  unread_count: number;
  preview?: string;
  timestamp_ms?: number;
  tick?: Tick;
}

export interface AppSnapshot {
  profile: Profile;
  node: NodeStatus;
  preferences: Preferences;
  diagnostics: Diagnostics;
  lan_peers: number;
  contacts: Contact[];
  conversations: ConversationSummary[];
  attachment_max_blob_bytes: number;
}

export interface Attachment {
  mime_type: string;
  duration_ms: number;
  data_base64: string;
  caption: string;
}

export interface Reaction {
  emoji: string;
  count: number;
  own: boolean;
}

export interface Message {
  id: string;
  sender_id: string;
  sender_name: string;
  own: boolean;
  lamport: number;
  timestamp_ms: number;
  kind: "text" | "image" | "audio" | "group_invite" | "unsupported_attachment";
  text?: string;
  attachment?: Attachment;
  reply_to_id?: string;
  reactions: Reaction[];
  tick?: Tick;
}

export interface Conversation {
  id: string;
  kind: ConversationKind;
  title: string;
  member_count: number;
  messages: Message[];
}

export interface AttachmentDraft {
  kind: "image" | "audio";
  mime_type: string;
  duration_ms: number;
  data_base64: string;
  caption: string;
}
