/**
 * Pure-unit tests for verifySignature() corner cases.
 *
 * Covers the edges the conformance corpus doesn't (unsigned events,
 * malformed pubkey, wrong-length pubkey).
 */
import { describe, expect, it } from "vitest";

import { type Event, SigningError, verifySignature } from "../src/index.js";

function makeUnsignedEvent(): Event {
  return {
    specversion: "1.0",
    id: "01900000-0000-7000-8000-000000000099",
    source: "ctxd://test",
    subject: "/t/u",
    type: "demo",
    time: "2026-01-01T00:00:00Z",
    datacontenttype: "application/json",
    data: {},
  };
}

describe("verifySignature corner cases", () => {
  it("returns false for an unsigned event", async () => {
    const ev = makeUnsignedEvent();
    const ok = await verifySignature(ev, "00".repeat(32));
    expect(ok).toBe(false);
  });

  it("throws SigningError for malformed pubkey hex", async () => {
    const ev = makeUnsignedEvent();
    await expect(verifySignature(ev, "not-hex!!")).rejects.toBeInstanceOf(
      SigningError,
    );
  });

  it("throws SigningError for wrong-length pubkey", async () => {
    const ev = makeUnsignedEvent();
    const short = "ab".repeat(30); // 30 bytes, not 32
    await expect(verifySignature(ev, short)).rejects.toBeInstanceOf(
      SigningError,
    );
  });

  it("returns false for a wrong-length signature on the event", async () => {
    const ev = makeUnsignedEvent();
    ev.signature = "deadbeef"; // 4 bytes, not 64
    const ok = await verifySignature(ev, "00".repeat(32));
    expect(ok).toBe(false);
  });
});
