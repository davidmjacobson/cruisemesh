import { describe, expect, it } from "vitest";
import { connectionSummary, contactRouteLabel, friendWebLink, kindNumber, tickLabel, tickVisual } from "./presentation";
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
});
