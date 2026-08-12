import { describe, expect, it } from "vitest";
import {
  connectionSummary,
  contactRouteLabel,
  formatDay,
  formatDurationMs,
  friendWebLink,
  isNewDay,
  kindNumber,
  tickLabel,
  tickVisual,
  userCopy,
  voiceProgress,
} from "./presentation";
import type { Message } from "./types";

function message(kind: Message["kind"]): Message {
  return {
    id: "id",
    sender_id: "sender",
    sender_name: "Emma",
    lamport: 1,
    timestamp_ms: 1,
    kind,
    own: false,
    reactions: [],
  };
}

describe("messenger presentation protocol", () => {
  it("maps rendered message kinds to the core reaction discriminator", () => {
    expect(kindNumber(message("text"))).toBe(1);
    expect(kindNumber(message("group_invite"))).toBe(4);
    expect(kindNumber(message("image"))).toBe(16);
    expect(kindNumber(message("audio"))).toBe(16);
  });

  it("provides accessible tick labels", () => {
    expect(tickLabel("sent")).toBe("Sent");
    expect(tickLabel("delivered")).toBe("Delivered");
    expect(tickLabel("read")).toBe("Read");
  });

  it("matches the mobile receipt glyph progression", () => {
    expect(tickVisual("sent")).toEqual({ count: 1, filled: false });
    expect(tickVisual("delivered")).toEqual({ count: 2, filled: false });
    expect(tickVisual("read")).toEqual({ count: 2, filled: true });
  });

  it("describes only connection evidence Windows actually has", () => {
    expect(connectionSummary(2, false).title).toBe("Connected nearby");
    expect(connectionSummary(0, true).title).toBe("Internet delivery is ready");
    expect(connectionSummary(0, false).tone).toBe("waiting");
    expect(contactRouteLabel(true, false)).toBe("Nearby on Wi-Fi");
    expect(contactRouteLabel(false, true)).toBe("Shore Pass available");
    expect(contactRouteLabel(false, false)).toBe("Not nearby");
  });

  it("encodes friend cards in the canonical browser link fragment", () => {
    expect(friendWebLink("CMFRIEND3:abc")).toBe(
      "https://cruisemesh.app/f#CMFRIEND3:abc",
    );
  });

  it("formats voice times the way the phones do", () => {
    expect(formatDurationMs(0)).toBe("0:00");
    expect(formatDurationMs(4_200)).toBe("0:04");
    expect(formatDurationMs(12_400)).toBe("0:12");
    expect(formatDurationMs(61_000)).toBe("1:01");
    expect(voiceProgress(3_000, 12_000)).toBe(0.25);
    expect(voiceProgress(20, 0)).toBe(0);
    expect(voiceProgress(-5, 10)).toBe(0);
    expect(voiceProgress(20, 10)).toBe(1);
  });

  it("inserts a date chip when the calendar day changes", () => {
    const monday = Date.parse("2026-08-10T10:00:00");
    const laterMonday = Date.parse("2026-08-10T22:00:00");
    const tuesday = Date.parse("2026-08-11T09:00:00");
    expect(isNewDay(monday)).toBe(true);
    expect(isNewDay(laterMonday, monday)).toBe(false);
    expect(isNewDay(tuesday, laterMonday)).toBe(true);
    expect(formatDay(monday, tuesday)).toBe("Yesterday");
    expect(formatDay(tuesday, tuesday)).toBe("Today");
  });

  it("keeps Windows internals and protocol names out of family-facing copy", () => {
    const surface = Object.values(userCopy).join("\n");
    for (const jargon of ["DPAPI", "CMRELAY", "CMFRIEND", "WebView2", "TCP", "LAN listener"]) {
      expect(surface).not.toContain(jargon);
    }
  });
});
