/**
 * Unified error hierarchy for the ctxd TypeScript SDK.
 *
 * The SDK glues HTTP, the wire protocol, and Ed25519 signature
 * verification into a single `CtxdError` hierarchy so callers can
 * raise/catch one base class without juggling library-specific
 * exceptions. Mirrors the Rust + Python SDKs' error shapes.
 *
 * Every subclass sets `name` explicitly so `instanceof` checks work
 * across realms (e.g. when a transpiler downlevels `class extends
 * Error`).
 */

/** Base class for all errors raised by the ctxd SDK. */
export class CtxdError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CtxdError";
    // Restore prototype chain for ES5 targets and cross-realm.
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/**
 * HTTP request returned a non-success status with a plain-text body.
 *
 * Carries both the status code and the response body so callers
 * retain full diagnostic context without losing the HTTP shape.
 */
export class HttpError extends CtxdError {
  readonly status: number;
  readonly body: string;

  constructor(status: number, body: string) {
    super(`http ${status}: ${body}`);
    this.name = "HttpError";
    this.status = status;
    this.body = body;
    Object.setPrototypeOf(this, HttpError.prototype);
  }
}

/** Authorization rejected by the server (HTTP 401 / 403). */
export class AuthError extends CtxdError {
  readonly body: string;

  constructor(body: string) {
    super(`authorization rejected: ${body}`);
    this.name = "AuthError";
    this.body = body;
    Object.setPrototypeOf(this, AuthError.prototype);
  }
}

/** Server returned 404 for the requested resource. */
export class NotFoundError extends CtxdError {
  readonly body: string;

  constructor(body: string) {
    super(`not found: ${body}`);
    this.name = "NotFoundError";
    this.body = body;
    Object.setPrototypeOf(this, NotFoundError.prototype);
  }
}

/** Wire-protocol IO or codec failure. */
export class WireError extends CtxdError {
  constructor(message: string) {
    super(message);
    this.name = "WireError";
    Object.setPrototypeOf(this, WireError.prototype);
  }
}

/**
 * The SDK was used in a way that requires a wire connection but only
 * HTTP is configured. Callers fix this by passing `wireAddr` to the
 * `CtxdClient` constructor (or in browser code, by using the HTTP-only
 * subset of the API).
 */
export class WireNotConfiguredError extends CtxdError {
  constructor() {
    super(
      "wire client not configured: pass `wireAddr` to the CtxdClient constructor first",
    );
    this.name = "WireNotConfiguredError";
    Object.setPrototypeOf(this, WireNotConfiguredError.prototype);
  }
}

/** The server's wire-protocol response did not match what the SDK expected. */
export class UnexpectedWireResponseError extends CtxdError {
  constructor(message: string) {
    super(`unexpected wire response: ${message}`);
    this.name = "UnexpectedWireResponseError";
    Object.setPrototypeOf(this, UnexpectedWireResponseError.prototype);
  }
}

/**
 * Ed25519 signature verification input failure.
 *
 * Surfaces malformed input (bad pubkey hex, wrong-length pubkey).
 * The SDK does NOT surface the underlying crypto library's verify
 * failures verbatim — those errors deliberately don't disclose which
 * check failed (side-channel hardening), and we preserve that.
 */
export class SigningError extends CtxdError {
  constructor(message: string) {
    super(message);
    this.name = "SigningError";
    Object.setPrototypeOf(this, SigningError.prototype);
  }
}

/**
 * Map an HTTP status code to the right `CtxdError` subclass.
 *
 * 401 / 403 -> AuthError, 404 -> NotFoundError, everything else
 * -> HttpError carrying the raw status + body.
 */
export function httpStatusError(status: number, body: string): CtxdError {
  if (status === 401 || status === 403) return new AuthError(body);
  if (status === 404) return new NotFoundError(body);
  return new HttpError(status, body);
}
