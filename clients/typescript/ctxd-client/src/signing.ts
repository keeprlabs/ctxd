/**
 * Ed25519 signature verification.
 *
 * Re-implements the daemon-side canonical-bytes routine from
 * `ctxd_core::signing` so SDK consumers can verify an event's
 * signature without reaching for an extra crypto library. Pinned to
 * the daemon side via the `docs/api/conformance/signatures/*.json`
 * fixtures — if either implementation drifts, conformance breaks.
 *
 * Notes on `@noble/ed25519` v2:
 *
 *   v2 dropped its bundled SHA-512 implementation to keep the audited
 *   surface tiny. Callers must wire a SHA-512 hook before the first
 *   verify call. We do that once at module load using
 *   `@noble/hashes/sha512`. The pattern is officially documented in
 *   the noble-ed25519 README; we hide it from SDK consumers so they
 *   don't re-discover it.
 *
 *   Verify is async because the underlying primitives are async-only
 *   in noble v2. `verifySignature` therefore returns `Promise<boolean>`.
 */
import * as ed from "@noble/ed25519";
import { sha512 } from "@noble/hashes/sha512";

import { canonicalBytes, type Event } from "./events.js";
import { SigningError } from "./errors.js";

// Wire the SHA-512 hook noble v2 requires. This MUST run before the
// first call to `ed.verify` / `ed.signAsync` / etc. — otherwise noble
// throws a confusing `etc.sha512Sync` error. We set both the sync and
// async hooks so consumers who pull either path get a working setup.
//
// `etc.sha512Sync` is consulted by sync entrypoints; the async paths
// fall back to it when `sha512Async` is unset. We provide both
// explicitly so future noble revisions don't surprise us.
ed.etc.sha512Sync = (...m: Uint8Array[]): Uint8Array => sha512(concat(m));
ed.etc.sha512Async = async (...m: Uint8Array[]): Promise<Uint8Array> =>
  sha512(concat(m));

function concat(chunks: Uint8Array[]): Uint8Array {
  let total = 0;
  for (const c of chunks) total += c.length;
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.length;
  }
  return out;
}

/**
 * Verify an event's Ed25519 signature against a hex-encoded public key.
 *
 * - `pubkeyHex` is a 64-character hex string (32 bytes).
 * - The event's own `signature` field is read; if it is missing, this
 *   resolves to `false` rather than rejecting — an unsigned event is
 *   indistinguishable from a tampered one for callers asking "is this
 *   signed by `pubkeyHex`?".
 *
 * Throws `SigningError` only for hard input failures: malformed hex
 * or wrong-length pubkey. Cryptographic verify failures resolve to
 * `false` (matching the Rust + Python SDKs).
 */
export async function verifySignature(
  event: Event,
  pubkeyHex: string,
): Promise<boolean> {
  const pubkeyBytes = decodePubkey(pubkeyHex);

  const sigHex = event.signature;
  if (sigHex == null) {
    // Unsigned event: "is this signed by this key?" -> No.
    return false;
  }

  let sigBytes: Uint8Array;
  try {
    sigBytes = hexToBytes(sigHex.trim());
  } catch (e) {
    throw new SigningError(`invalid signature hex: ${(e as Error).message}`);
  }
  if (sigBytes.length !== 64) {
    // Wrong-length signature -> not this signature. Match the Rust /
    // Python SDKs' behavior of returning false rather than raising.
    return false;
  }

  const canonical = canonicalBytes(event);

  try {
    return await ed.verifyAsync(sigBytes, canonical, pubkeyBytes);
  } catch {
    // noble v2 throws on malformed signature/pubkey shape; we treat
    // any throw as "not a valid signature for this key/event pair".
    return false;
  }
}

function decodePubkey(pubkeyHex: string): Uint8Array {
  let bytes: Uint8Array;
  try {
    bytes = hexToBytes(pubkeyHex.trim());
  } catch (e) {
    throw new SigningError(`invalid pubkey hex: ${(e as Error).message}`);
  }
  if (bytes.length !== 32) {
    throw new SigningError("pubkey must be 32 bytes (64 hex chars)");
  }
  return bytes;
}

/** Decode a hex string into bytes. Throws on odd length or invalid chars. */
function hexToBytes(hex: string): Uint8Array {
  if (hex.length % 2 !== 0) {
    throw new Error("hex string has odd length");
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    const byte = parseInt(hex.substring(i * 2, i * 2 + 2), 16);
    if (Number.isNaN(byte)) {
      throw new Error(`invalid hex char near offset ${i * 2}`);
    }
    out[i] = byte;
  }
  return out;
}
