/**
 * Pure encoding helpers for the wire protocol.
 *
 * Lives separate from `wire.node.ts` so the conformance tests + the
 * browser stub can both reach it without pulling in `node:net`. The
 * shape of every message is pinned by `docs/api/conformance/wire/*`
 * — those fixtures are the test oracle.
 *
 * Wire framing:
 *
 *   - 4-byte big-endian length prefix.
 *   - Body is a single MessagePack value.
 *
 * `rmp-serde` externally-tagged enum encoding:
 *
 *   - `Request::Ping` (nullary) -> bare string `"Ping"`.
 *   - `Request::Pub { subject, event_type, data }` ->
 *     `{"Pub": [subject, event_type, data]}` — a one-key map whose
 *     value is a *positional array* in field declaration order, NOT
 *     a map. Same shape for every struct-form variant.
 *   - `Response::Ok { data }` -> `{"Ok": [data]}`.
 *   - `Response::Pong` -> `"Pong"`.
 */
import { encode, decode } from "@msgpack/msgpack";

/** 16 MiB ceiling. Mirrors `MAX_FRAME_BYTES` in the Rust wire crate. */
export const MAX_FRAME_BYTES = 16 * 1024 * 1024;

/**
 * Field declaration order for each struct-form Request variant.
 *
 * Source of truth: `crates/ctxd-wire/src/messages.rs`. Kept in sync
 * with the Python SDK's identical table — both translate the
 * named-field JSON conformance fixtures into the positional array
 * `rmp-serde` actually puts on the wire.
 */
export const REQUEST_FIELD_ORDER: Record<string, readonly string[]> = {
  Pub: ["subject", "event_type", "data"],
  Sub: ["subject_pattern"],
  Query: ["subject_pattern", "view"],
  Grant: ["subject", "operations", "expiry"],
  Revoke: ["cap_id"],
  PeerHello: ["peer_id", "public_key", "offered_cap", "subjects"],
  PeerWelcome: ["peer_id", "public_key", "offered_cap", "subjects"],
  PeerReplicate: ["origin_peer_id", "event"],
  PeerAck: ["origin_peer_id", "event_id"],
  PeerCursorRequest: ["peer_id", "subject_pattern"],
  PeerCursor: ["peer_id", "subject_pattern", "last_event_id", "last_event_time"],
  PeerFetchEvents: ["event_ids"],
};

/** Field declaration order for each struct-form Response variant. */
export const RESPONSE_FIELD_ORDER: Record<string, readonly string[]> = {
  Ok: ["data"],
  Event: ["event"],
  Error: ["message"],
};

/** Externally-tagged Request: bare-string variant or one-key tagged map. */
export type WireRequest = string | Record<string, unknown>;

/**
 * Encode a Request to msgpack bytes (no framing).
 *
 * Accepts either a one-key map `{VariantName: <inner>}` (struct
 * variant — the inner is already the positional array we want on the
 * wire) or a bare string variant name (nullary variant).
 */
export function encodeRequest(req: WireRequest): Uint8Array {
  return encode(req);
}

/** Decode a single msgpack response body. */
export function decodeResponse(payload: Uint8Array): unknown {
  return decode(payload);
}

/**
 * Convert a named-field struct dict (the JSON conformance shape) into
 * the positional array `rmp-serde` actually emits.
 */
export function structToPositional(
  variant: string,
  inner: Record<string, unknown>,
  fieldOrder: Record<string, readonly string[]>,
): unknown[] {
  const fields = fieldOrder[variant];
  if (!fields) {
    throw new Error(`unknown variant ${variant} for positional conversion`);
  }
  return fields.map((f) => inner[f]);
}

/**
 * Normalize a Request fixture value into the wire-shape the encoder
 * accepts: bare-string nullary variants pass through; map-form struct
 * variants get their inner converted to a positional array.
 */
export function normalizeRequestForWire(value: unknown): unknown {
  if (typeof value === "string") return value;
  if (
    value &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).length === 1
  ) {
    const [variant] = Object.keys(value);
    if (variant === undefined) return value;
    const inner = (value as Record<string, unknown>)[variant];
    if (
      inner &&
      typeof inner === "object" &&
      !Array.isArray(inner) &&
      variant in REQUEST_FIELD_ORDER
    ) {
      return {
        [variant]: structToPositional(
          variant,
          inner as Record<string, unknown>,
          REQUEST_FIELD_ORDER,
        ),
      };
    }
  }
  return value;
}

/** Same as {@link normalizeRequestForWire} for Response variants. */
export function normalizeResponseForWire(value: unknown): unknown {
  if (typeof value === "string") return value;
  if (
    value &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).length === 1
  ) {
    const [variant] = Object.keys(value);
    if (variant === undefined) return value;
    const inner = (value as Record<string, unknown>)[variant];
    if (
      inner &&
      typeof inner === "object" &&
      !Array.isArray(inner) &&
      variant in RESPONSE_FIELD_ORDER
    ) {
      return {
        [variant]: structToPositional(
          variant,
          inner as Record<string, unknown>,
          RESPONSE_FIELD_ORDER,
        ),
      };
    }
  }
  return value;
}

/**
 * Build a 4-byte big-endian length-prefix header.
 */
export function frameHeader(payloadLen: number): Uint8Array {
  if (payloadLen > MAX_FRAME_BYTES) {
    throw new Error(`frame too large to send: ${payloadLen} bytes`);
  }
  const header = new Uint8Array(4);
  // DataView gives us a portable big-endian write without bit-twiddling.
  new DataView(header.buffer).setUint32(0, payloadLen, false);
  return header;
}

/** Read the length prefix from a 4-byte header. */
export function readFrameLength(header: Uint8Array): number {
  if (header.length !== 4) {
    throw new Error(`frame header must be 4 bytes, got ${header.length}`);
  }
  return new DataView(
    header.buffer,
    header.byteOffset,
    header.byteLength,
  ).getUint32(0, false);
}

/**
 * Construct the positional-array wire shape for a struct Request
 * variant. Convenience for callers building Pub / Query / Grant /
 * Revoke / Sub requests directly.
 */
export function makeRequest(
  variant: keyof typeof REQUEST_FIELD_ORDER,
  inner: Record<string, unknown>,
): WireRequest {
  return {
    [variant]: structToPositional(variant, inner, REQUEST_FIELD_ORDER),
  };
}

/**
 * Unwrap a `Response::Ok`'s data, or throw on non-Ok variants.
 *
 * `rmp-serde` emits `Response::Ok { data }` as `{"Ok": [data]}`. We
 * tolerate `{"Ok": {"data": ...}}` for codecs that produce map-form
 * structs (defensive — the daemon doesn't, but a future adapter might).
 */
export function expectOk(resp: unknown): unknown {
  if (resp === "Pong") return null;
  if (resp && typeof resp === "object" && !Array.isArray(resp)) {
    const r = resp as Record<string, unknown>;
    if ("Ok" in r) {
      const inner = r.Ok;
      if (Array.isArray(inner) && inner.length === 1) return inner[0];
      if (inner && typeof inner === "object" && !Array.isArray(inner)) {
        const o = inner as Record<string, unknown>;
        return "data" in o ? o.data : o;
      }
      return inner;
    }
    if ("Error" in r) {
      const inner = r.Error;
      if (Array.isArray(inner) && inner.length === 1) {
        throw new Error(String(inner[0]));
      }
      if (inner && typeof inner === "object" && !Array.isArray(inner)) {
        const o = inner as Record<string, unknown>;
        throw new Error(String(o.message ?? JSON.stringify(o)));
      }
      throw new Error(String(inner));
    }
  }
  throw new Error(`expected Ok, got ${JSON.stringify(resp)}`);
}
