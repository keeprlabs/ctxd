/**
 * CloudEvents-shaped `Event` type + canonical-bytes helper.
 *
 * The shape mirrors `crates/ctxd-core/src/event.rs` and the JSON
 * Schema at `docs/api/events.schema.json`. We keep IDs as plain
 * strings (UUIDv7 lexical form) rather than parsing into a UUID type;
 * coercing risks normalizing to canonical lowercase and drifting from
 * the daemon's wire encoding.
 *
 * The wire form omits empty optional fields (matches the Rust
 * `skip_serializing_if`). The canonical signing form is stricter:
 * always includes `parents` (sorted, possibly `[]`), always includes
 * `attestation` (possibly `null`), and excludes `predecessorhash` +
 * `signature`.
 */

/**
 * A CloudEvents v1.0 event with ctxd extensions.
 *
 * `type` is the wire field name (CloudEvents discriminator). We expose
 * it under that name to match the on-the-wire JSON exactly — callers
 * who want a TypeScript-friendly alias can destructure as
 * `{ type: eventType, ...rest }`.
 */
export interface Event {
  /** CloudEvents spec version. Always `"1.0"` for v0.3. */
  specversion: string;
  /** Globally-unique UUIDv7 string. */
  id: string;
  /** Identifies the context in which the event happened. */
  source: string;
  /** Subject path the event is filed under. */
  subject: string;
  /** Event type discriminator (e.g. `ctx.note`). */
  type: string;
  /** RFC3339 timestamp of when the event was created. */
  time: string;
  /** Content type of `data`. */
  datacontenttype: string;
  /** Event payload. JSON of any shape. */
  data: unknown;
  /** SHA-256 hash of the predecessor event's canonical form. */
  predecessorhash?: string | null;
  /** Ed25519 signature over the canonical form, hex-encoded. */
  signature?: string | null;
  /** Parent event ids (UUIDv7 strings). */
  parents?: string[];
  /** Optional TEE attestation blob, hex-encoded. */
  attestation?: string | null;
}

/**
 * Produce the wire-shape JSON object for `event` (omitting empty
 * optional fields).
 *
 * - `predecessorhash`, `signature`, `attestation` are omitted when
 *   `null` / `undefined`.
 * - `parents` is omitted when empty (matches the Rust
 *   `skip_serializing_if = "Vec::is_empty"`).
 */
export function eventToWire(event: Event): Record<string, unknown> {
  const out: Record<string, unknown> = {
    specversion: event.specversion,
    id: event.id,
    source: event.source,
    subject: event.subject,
    type: event.type,
    time: event.time,
    datacontenttype: event.datacontenttype,
    data: event.data,
  };
  if (event.predecessorhash != null) {
    out.predecessorhash = event.predecessorhash;
  }
  if (event.signature != null) {
    out.signature = event.signature;
  }
  if (event.parents && event.parents.length > 0) {
    out.parents = [...event.parents];
  }
  if (event.attestation != null) {
    out.attestation = event.attestation;
  }
  return out;
}

/**
 * Parse an unknown value into an `Event`. Throws on missing required
 * fields. We deliberately avoid a heavyweight schema validator (zod,
 * io-ts) to keep the dependency surface tight — the wire shape is
 * narrow and stable, and a hand-written guard is easier to audit.
 */
export function parseEvent(raw: unknown): Event {
  if (raw == null || typeof raw !== "object") {
    throw new TypeError(`event must be an object, got ${typeof raw}`);
  }
  const r = raw as Record<string, unknown>;
  const required = ["id", "source", "subject", "type", "time", "data"] as const;
  for (const key of required) {
    if (!(key in r)) {
      throw new TypeError(`event missing required field: ${key}`);
    }
  }
  const ev: Event = {
    specversion: typeof r.specversion === "string" ? r.specversion : "1.0",
    id: String(r.id),
    source: String(r.source),
    subject: String(r.subject),
    type: String(r.type),
    time: String(r.time),
    datacontenttype:
      typeof r.datacontenttype === "string"
        ? r.datacontenttype
        : "application/json",
    data: r.data,
  };
  if (typeof r.predecessorhash === "string") {
    ev.predecessorhash = r.predecessorhash;
  }
  if (typeof r.signature === "string") {
    ev.signature = r.signature;
  }
  if (Array.isArray(r.parents)) {
    ev.parents = r.parents.map(String);
  }
  if (typeof r.attestation === "string") {
    ev.attestation = r.attestation;
  }
  return ev;
}

/**
 * Produce the canonical signing bytes for `event`.
 *
 * Mirrors `ctxd_core::signing::canonical_bytes` exactly: a JSON
 * object with **sorted keys** containing every CloudEvents field
 * except `predecessorhash` and `signature`, plus `parents` (sorted
 * lexicographically; empty array if none) and `attestation`
 * (hex-encoded or `null`).
 *
 * The Rust side emits this via `serde_json::to_vec(BTreeMap<&str,
 * Value>)`: compact JSON (no spaces) with sorted keys. We construct
 * the same shape and rely on the helper below for sort-order
 * stability — `JSON.stringify` does NOT sort keys by default.
 */
export function canonicalBytes(event: Event): Uint8Array {
  const parents = [...(event.parents ?? [])].sort();
  const attestation = event.attestation ?? null;
  const payload: Record<string, unknown> = {
    attestation,
    data: event.data,
    datacontenttype: event.datacontenttype,
    id: event.id,
    parents,
    source: event.source,
    specversion: event.specversion,
    subject: event.subject,
    time: event.time,
    type: event.type,
  };
  const json = stableStringify(payload);
  return new TextEncoder().encode(json);
}

/**
 * Stable, sorted-key JSON serialization (compact form).
 *
 * Mirrors `serde_json` over `BTreeMap<&str, Value>` byte-for-byte:
 * sorted keys, no whitespace, no trailing newline. Pure recursive
 * walk; Maps and Sets are not handled (the SDK never emits them in a
 * canonical form).
 */
function stableStringify(value: unknown): string {
  if (value === null || value === undefined) return "null";
  if (typeof value === "number") {
    // Match serde_json: integers without trailing decimal, finite only.
    if (!Number.isFinite(value)) {
      throw new TypeError("non-finite number in canonical form");
    }
    return JSON.stringify(value);
  }
  if (typeof value === "string" || typeof value === "boolean") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return "[" + value.map(stableStringify).join(",") + "]";
  }
  if (typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const keys = Object.keys(obj).sort();
    const parts: string[] = [];
    for (const k of keys) {
      parts.push(JSON.stringify(k) + ":" + stableStringify(obj[k]));
    }
    return "{" + parts.join(",") + "}";
  }
  // bigint, symbol, function — not supported in canonical JSON.
  throw new TypeError(`unsupported value in canonical form: ${typeof value}`);
}
