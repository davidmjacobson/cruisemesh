import type { Message, Tick } from "./types";

export function tickLabel(tick?: Tick): string {
  if (tick === "read") return "Read";
  if (tick === "delivered") return "Delivered";
  return "Sent";
}

export function tickVisual(tick?: Tick): { count: 1 | 2; filled: boolean } | undefined {
  if (!tick) return undefined;
  return {
    count: tick === "sent" ? 1 : 2,
    filled: tick === "read",
  };
}

export function connectionSummary(lanPeers: number, relayConfigured: boolean): { title: string; detail: string; tone: "good" | "waiting" } {
  if (lanPeers > 0) {
    return {
      title: "Connected nearby",
      detail: `${lanPeers} authenticated ${lanPeers === 1 ? "device is" : "devices are"} active on Wi-Fi.`,
      tone: "good",
    };
  }
  if (relayConfigured) {
    return {
      title: "Internet delivery is ready",
      detail: "No one is nearby right now; Shore Pass continues delivery in the background.",
      tone: "good",
    };
  }
  return {
    title: "Waiting for someone nearby",
    detail: "CruiseMesh is listening on local Wi-Fi. Add Shore Pass for internet delivery.",
    tone: "waiting",
  };
}

export function contactRouteLabel(connectedLan: boolean, internetDeliveryConfigured: boolean): string {
  if (connectedLan) return "Nearby on Wi-Fi";
  if (internetDeliveryConfigured) return "Shore Pass available";
  return "Not nearby";
}

export function kindNumber(message: Message): number {
  if (message.kind === "text") return 1;
  if (message.kind === "group_invite") return 4;
  return 16;
}

export function friendWebLink(card: string): string {
  return `https://cruisemesh.app/f#${card}`;
}

/** Family-facing copy. Keep protocol and Windows internals out of these strings. */
export const userCopy = {
  shorePassHelp:
    "Paste a Shore Pass or a setup link from cruisemesh.app. It stays on this Windows account and is never shown in this window.",
  emptyConversation:
    "Messages go over Wi-Fi when someone is nearby, or over Shore Pass when they are not.",
  addFriendHelp: "Scan their CruiseMesh QR code, or paste a friend link.",
  cameraScanUnavailable:
    "This computer cannot scan QR codes from the camera. Paste the link instead.",
  playVoiceMessage: "Play voice message",
  pauseVoiceMessage: "Pause voice message",
  voicePlaybackFailed: "Could not play that voice message",
  recordingLabel: "Recording",
  sendVoiceMessage: "Send voice message",
  cancelRecording: "Cancel",
  recordVoiceMessage: "Record voice message",
  recordingTooShort: "Hold a little longer to send a voice message.",
  voiceTooLong: "That voice message is too long to send. Try a shorter one.",
  photoTooLarge: "That photo is too large to send.",
  wifiPort: "Wi-Fi port",
  termsTitle: "Before you start",
  termsBody:
    "CruiseMesh is a family messenger. Messages stay between the people you add. Please read the terms and privacy policy, then choose the name friends will see.",
  termsAgree: "I agree to the terms of use",
  continueSetup: "Continue",
  termsLink: "Terms of use",
  privacyLink: "Privacy policy",
  addThisFriend: "Add this friend?",
  alreadyKnown: "This person is already in your contacts.",
  sharedBy: "Shared by",
  expiredCode: "That code has expired. Ask for a new one.",
  safetyWords: "Have them check these words match their name in CruiseMesh.",
  muteNotifications: "Mute notifications",
  blockContact: "Block contact",
  blockExplain:
    "They won't be able to message you, and they won't appear as a suggested friend. They won't be told. Scanning their card again unblocks them.",
  reportContact: "Report contact",
  deleteContact: "Delete contact",
  nickname: "Nickname",
  nicknameHint: "Only you see this name.",
  shareContact: "Share contact",
  shareContactHelp: "Show this code to the person standing with you. Do not send it in a message.",
  friendsOfFriends: "Friends of friends",
  friendsOfFriendsDetail:
    "Let friends introduce you, and let you share a contact by showing a code.",
  addMembers: "Add members",
  renameGroup: "Rename group",
  waitingToConnect: "Waiting to connect",
  connect: "Connect",
  notNow: "Not now",
  dontAskAgain: "Don't ask again",
  loadOlder: "Load earlier messages",
  draw: "Draw",
  undo: "Undo",
  clearDrawing: "Clear",
  usePhoto: "Use photo",
  helpSupport: "Help and support",
} as const;

export function formatDurationMs(ms: number): string {
  const totalSec = Math.max(0, Math.round(ms / 1000));
  const min = Math.floor(totalSec / 60);
  const sec = totalSec % 60;
  return `${min}:${sec.toString().padStart(2, "0")}`;
}

export function voiceProgress(positionMs: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return Math.min(1, Math.max(0, positionMs / durationMs));
}

export function isNewDay(currentMs: number, previousMs?: number): boolean {
  if (previousMs === undefined) return true;
  const current = new Date(currentMs);
  const previous = new Date(previousMs);
  return (
    current.getFullYear() !== previous.getFullYear() ||
    current.getMonth() !== previous.getMonth() ||
    current.getDate() !== previous.getDate()
  );
}

export function formatDay(timestampMs: number, nowMs = Date.now()): string {
  const date = new Date(timestampMs);
  const now = new Date(nowMs);
  const startOf = (value: Date) => Date.UTC(value.getFullYear(), value.getMonth(), value.getDate());
  const diffDays = Math.round((startOf(now) - startOf(date)) / 86_400_000);
  if (diffDays === 0) return "Today";
  if (diffDays === 1) return "Yesterday";
  return date.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
}
