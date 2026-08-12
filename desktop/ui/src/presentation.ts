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
