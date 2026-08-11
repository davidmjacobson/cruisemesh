import type { Message, Tick } from "./types";

export function tickLabel(tick?: Tick): string {
  if (tick === "read") return "Read";
  if (tick === "delivered") return "Delivered";
  return "Sent";
}

export function kindNumber(message: Message): number {
  if (message.kind === "text") return 1;
  if (message.kind === "group_invite") return 4;
  return 16;
}

export function friendWebLink(card: string): string {
  return `https://cruisemesh.app/f#${card}`;
}
