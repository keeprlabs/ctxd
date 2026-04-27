/**
 * Conformance tests against `docs/api/conformance/*`.
 *
 * Three corpora:
 *
 *   - signatures/*.json — verifySignature() against canonical fixtures
 *     produced by the Rust signer. Pinned to the daemon's
 *     canonical-bytes routine.
 *   - wire/{name}.json + wire/{name}.msgpack.hex — encode the named-
 *     field JSON shape via @msgpack/msgpack (after converting to the
 *     positional array rmp-serde emits) and compare hex byte-for-byte.
 *   - events/*.json — parse into the TS Event type and confirm
 *     eventToWire() round-trips.
 */
import { readFileSync, readdirSync } from "node:fs";
import * as path from "node:path";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import { encode } from "@msgpack/msgpack";
import { describe, expect, it } from "vitest";

import {
  type Event,
  eventToWire,
  parseEvent,
  verifySignature,
} from "../src/index.js";
import {
  normalizeRequestForWire,
  normalizeResponseForWire,
} from "../src/wire-codec.js";
import { workspaceRoot } from "./common.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

function conformanceDir(sub: string): string {
  return path.join(workspaceRoot(), "docs", "api", "conformance", sub);
}

function listJson(dir: string): string[] {
  return readdirSync(dir)
    .filter((f) => f.endsWith(".json"))
    .map((f) => path.join(dir, f))
    .sort();
}

describe("signature conformance", () => {
  const fixtures = listJson(conformanceDir("signatures"));

  it("corpus has at least 3 fixtures", () => {
    expect(fixtures.length).toBeGreaterThanOrEqual(3);
  });

  for (const fixturePath of fixtures) {
    const stem = path.basename(fixturePath, ".json");
    it(`signature/${stem} matches expected`, async () => {
      const j = JSON.parse(readFileSync(fixturePath, "utf8"));
      const event = parseEvent(j.event);
      // The fixture stores the signature outside the event so that a
      // tampered fixture can carry a *different* signature than would
      // be embedded. We assemble it here.
      event.signature = j.signature;
      const expected = Boolean(j.expected);
      const actual = await verifySignature(event, j.public_key_hex);
      expect(actual).toBe(expected);
    });
  }
});

describe("wire-protocol conformance", () => {
  const dir = conformanceDir("wire");
  // Pair up `{stem}.json` with `{stem}.msgpack.hex`.
  const all = readdirSync(dir).sort();
  const stems = new Set<string>();
  for (const name of all) {
    if (name.endsWith(".json")) stems.add(name.slice(0, -".json".length));
    else if (name.endsWith(".msgpack.hex"))
      stems.add(name.slice(0, -".msgpack.hex".length));
  }

  it("corpus has at least 5 fixture pairs", () => {
    expect(stems.size).toBeGreaterThanOrEqual(5);
  });

  for (const stem of [...stems].sort()) {
    const jsonPath = path.join(dir, `${stem}.json`);
    const hexPath = path.join(dir, `${stem}.msgpack.hex`);
    it(`wire/${stem}.msgpack matches corpus`, () => {
      const expectedBytes = Buffer.from(
        readFileSync(hexPath, "utf8").trim(),
        "hex",
      );
      const fixture = JSON.parse(readFileSync(jsonPath, "utf8"));
      const wireValue = stem.endsWith("_response")
        ? normalizeResponseForWire(fixture)
        : normalizeRequestForWire(fixture);
      const actualBytes = Buffer.from(encode(wireValue));
      expect(actualBytes.toString("hex")).toBe(expectedBytes.toString("hex"));
    });
  }
});

describe("event-schema conformance", () => {
  const dir = conformanceDir("events");
  const fixtures = listJson(dir);

  it("corpus has at least 3 fixtures", () => {
    expect(fixtures.length).toBeGreaterThanOrEqual(3);
  });

  for (const fixturePath of fixtures) {
    const stem = path.basename(fixturePath, ".json");
    it(`event/${stem} round-trips structurally`, () => {
      const raw = JSON.parse(readFileSync(fixturePath, "utf8"));
      const event: Event = parseEvent(raw);
      const re = eventToWire(event);
      // Stable-stringify both sides so order doesn't matter.
      expect(stableStringify(re)).toBe(stableStringify(raw));
    });
  }

  it("signed.json verifies against signed.pubkey.hex", async () => {
    const signedPath = path.join(dir, "signed.json");
    const pubkeyPath = path.join(dir, "signed.pubkey.hex");
    const event = parseEvent(JSON.parse(readFileSync(signedPath, "utf8")));
    const pubkeyHex = readFileSync(pubkeyPath, "utf8").trim();
    expect(await verifySignature(event, pubkeyHex)).toBe(true);
  });
});

/** Stable JSON for object equality across key orderings. */
function stableStringify(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value))
    return "[" + value.map(stableStringify).join(",") + "]";
  const obj = value as Record<string, unknown>;
  const keys = Object.keys(obj).sort();
  return (
    "{" +
    keys
      .map((k) => JSON.stringify(k) + ":" + stableStringify(obj[k]))
      .join(",") +
    "}"
  );
}
