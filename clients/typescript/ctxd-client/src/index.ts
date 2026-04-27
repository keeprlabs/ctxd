/**
 * Public entry point for `@ctxd/client` (Node).
 *
 * Re-exports every type and function callers should reach for. Prefer
 * named imports — there is no default export.
 *
 * For the browser entry, the same names are re-exported but the
 * wire-protocol methods on `CtxdClient` throw at call time. See
 * README.md for the browser story.
 */
export { CtxdClient } from "./client.js";
export type {
  CtxdClientOptions,
  GrantOptions,
  PeerRemoveOptions,
  QueryOptions,
  RevokeOptions,
  WriteOptions,
} from "./client.js";

export {
  AuthError,
  CtxdError,
  HttpError,
  NotFoundError,
  SigningError,
  UnexpectedWireResponseError,
  WireError,
  WireNotConfiguredError,
} from "./errors.js";

export {
  type Event,
  canonicalBytes,
  eventToWire,
  parseEvent,
} from "./events.js";

export {
  type HealthInfo,
  type HttpAdminClientOptions,
  HttpAdminClient,
  type PeerInfo,
  type StatsInfo,
} from "./http.js";

export { Operation, isOperation } from "./operation.js";

export { verifySignature } from "./signing.js";

export { WireClient } from "./wire.node.js";

export const VERSION = "0.3.0";
