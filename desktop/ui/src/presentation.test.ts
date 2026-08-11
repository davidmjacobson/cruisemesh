import { describe, expect, it } from "vitest";
import { friendWebLink, kindNumber, tickLabel } from "./presentation";
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

  it("encodes friend cards in the canonical browser link fragment", () => {
    expect(friendWebLink("CMFRIEND3:abc")).toBe(
      "https://cruisemesh.app/f#CMFRIEND3:abc",
    );
  });
});
