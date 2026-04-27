/**
 * Browser entry point for `@ctxd/client`.
 *
 * Mirrors `index.ts` for Node — same names, same shapes — but pulls
 * in `client.browser.ts` and `wire.browser.ts` instead of their Node
 * counterparts. The browser bundle is HTTP-only: calling wire-protocol
 * methods (`write`, `query`, `subscribe`, `revoke`) throws `WireError`.
 *
 * See `README.md` for the v0.3 browser story (HTTP-only, no
 * subscriptions; per `docs/plans/sdks.md` decision #2).
 */
export { CtxdClient } from "./client.browser.js";
export type {
  CtxdClientOptions,
  GrantOptions,
  PeerRemoveOptions,
  QueryOptions,
  RevokeOptions,
  WriteOptions,
} from "./client.browser.js";

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

export { WireClient } from "./wire.browser.js";

export const VERSION = "0.3.0";
