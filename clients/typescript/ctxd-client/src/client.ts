/**
 * High-level `CtxdClient` facade.
 *
 * Wraps the lower-level `HttpAdminClient` and a lazily-instantiated
 * wire connection so a typical "construct -> write -> query" flow
 * looks like any modern SDK:
 *
 * ```ts
 * const client = new CtxdClient({
 *   httpUrl: "http://127.0.0.1:7777",
 *   wireAddr: "127.0.0.1:7778",
 * });
 * const eid = await client.write({
 *   subject: "/work/note",
 *   eventType: "ctx.note",
 *   data: { text: "hi" },
 * });
 * for await (const event of client.subscribe("/work/**")) {
 *   ...
 * }
 * await client.close();
 * ```
 *
 * `wireAddr` is optional — without it, only HTTP admin methods are
 * available (`health`, `stats`, `grant`, `peers`, `peerRemove`).
 * Calling `write` / `query` / `subscribe` / `revoke` without a wire
 * address throws `WireNotConfiguredError`.
 *
 * Browser builds: imports `WireClient` from the browser stub. Wire
 * methods throw at call time. HTTP methods work as on Node.
 */
import { WireNotConfiguredError } from "./errors.js";
import { type Event } from "./events.js";
import {
  type HealthInfo,
  type HttpAdminClientOptions,
  HttpAdminClient,
  type PeerInfo,
  type StatsInfo,
} from "./http.js";
import { type Operation } from "./operation.js";
import { verifySignature as verifySignatureFn } from "./signing.js";
// `wire.node.ts` for Node, `wire.browser.ts` for browser — wired by
// tsup's build matrix + the `exports` map in package.json.
import { WireClient } from "./wire.node.js";

/** Construction options for {@link CtxdClient}. */
export interface CtxdClientOptions {
  /** Base HTTP admin URL, e.g. `"http://127.0.0.1:7777"`. */
  httpUrl: string;
  /** Wire-protocol address `host:port`, e.g. `"127.0.0.1:7778"`. */
  wireAddr?: string;
  /** Bearer token attached to every HTTP request. */
  token?: string;
  /** Per-call HTTP timeout in milliseconds. */
  timeoutMs?: number;
  /** Override the global `fetch`. See {@link HttpAdminClientOptions.fetch}. */
  fetch?: typeof fetch;
}

/** Inputs to {@link CtxdClient.write}. */
export interface WriteOptions {
  subject: string;
  eventType: string;
  data: unknown;
}

/** Inputs to {@link CtxdClient.query}. */
export interface QueryOptions {
  subjectPattern: string;
  view?: string;
}

/** Inputs to {@link CtxdClient.grant}. */
export interface GrantOptions {
  subject: string;
  operations: ReadonlyArray<Operation | string>;
  expiresAt?: Date;
}

/** Inputs to {@link CtxdClient.revoke}. */
export interface RevokeOptions {
  tokenId: string;
}

/** Inputs to {@link CtxdClient.peerRemove}. */
export interface PeerRemoveOptions {
  peerId: string;
}

/**
 * High-level ctxd client. Holds an HTTP admin client (always present)
 * and an optional wire connection (lazy — opened on first wire-method
 * call when `wireAddr` was provided).
 */
export class CtxdClient {
  private readonly http: HttpAdminClient;
  private wire: WireClient | null = null;
  private wireConnect: Promise<WireClient> | null = null;
  private readonly wireAddr: string | undefined;

  constructor(options: CtxdClientOptions) {
    const httpOptions: HttpAdminClientOptions = {};
    if (options.token !== undefined) httpOptions.token = options.token;
    if (options.timeoutMs !== undefined) httpOptions.timeoutMs = options.timeoutMs;
    if (options.fetch !== undefined) httpOptions.fetch = options.fetch;
    this.http = new HttpAdminClient(options.httpUrl, httpOptions);
    this.wireAddr = options.wireAddr;
  }

  /** Base HTTP admin URL (with any trailing slash stripped). */
  get httpUrl(): string {
    return this.http.baseUrl;
  }

  /** Wire address this client was configured with, if any. */
  get wireAddress(): string | undefined {
    return this.wireAddr;
  }

  /** Attach a capability token to all admin calls. */
  withToken(token: string): this {
    this.http.withToken(token);
    return this;
  }

  /** Release the HTTP pool and the wire connection (if open). */
  async close(): Promise<void> {
    if (this.wire !== null) {
      await this.wire.close();
      this.wire = null;
    }
    this.wireConnect = null;
  }

  // ----- HTTP admin endpoints -----

  /** `GET /health`. */
  async health(): Promise<HealthInfo> {
    return this.http.health();
  }

  /** `GET /v1/stats`. */
  async stats(): Promise<StatsInfo> {
    return this.http.stats();
  }

  /** `POST /v1/grant` — mint a capability token. */
  async grant(opts: GrantOptions): Promise<string> {
    return this.http.grant(opts.subject, opts.operations, opts.expiresAt);
  }

  /** `GET /v1/peers` — list federation peers (admin). */
  async peers(): Promise<PeerInfo[]> {
    return this.http.peers();
  }

  /** `DELETE /v1/peers/{peerId}` — remove a federation peer (admin). */
  async peerRemove(opts: PeerRemoveOptions): Promise<void> {
    return this.http.peerRemove(opts.peerId);
  }

  // ----- Wire-protocol verbs -----

  /** Append an event under a subject. Returns the new UUIDv7 id. */
  async write(opts: WriteOptions): Promise<string> {
    const wire = await this.requireWire();
    return wire.publish(opts.subject, opts.eventType, opts.data);
  }

  /** Query a materialized view. `view` defaults to `"log"`. */
  async query(opts: QueryOptions): Promise<Event[]> {
    const wire = await this.requireWire();
    return wire.query(opts.subjectPattern, opts.view ?? "log");
  }

  /**
   * Subscribe to events matching `subjectPattern`. Opens a *fresh*
   * TCP connection (the daemon puts its side into streaming-receive
   * mode after a `Sub`, so the connection can't be reused for further
   * requests). Iterate with `for await`:
   *
   * ```ts
   * for await (const event of client.subscribe("/work/**")) {
   *   ...
   * }
   * ```
   */
  async *subscribe(
    subjectPattern: string,
  ): AsyncGenerator<Event, void, void> {
    const wire = await this.requireWire();
    yield* wire.subscribe(subjectPattern);
  }

  /** Revoke a capability token by id (wire `Revoke` verb). */
  async revoke(opts: RevokeOptions): Promise<void> {
    const wire = await this.requireWire();
    await wire.revoke(opts.tokenId);
  }

  // ----- Pure helpers -----

  /** Verify an event's Ed25519 signature against a hex-encoded pubkey. */
  static verifySignature(event: Event, pubkeyHex: string): Promise<boolean> {
    return verifySignatureFn(event, pubkeyHex);
  }

  // ----- Internals -----

  private async requireWire(): Promise<WireClient> {
    if (this.wire !== null) return this.wire;
    if (this.wireAddr === undefined) {
      throw new WireNotConfiguredError();
    }
    // Coalesce concurrent connect attempts. If two callers hit a
    // wire method at the same time on a fresh client, they should
    // share a single TCP connection — not race two against each other.
    if (this.wireConnect === null) {
      const addr = this.wireAddr;
      this.wireConnect = WireClient.connect(addr).then((c) => {
        this.wire = c;
        return c;
      });
    }
    return this.wireConnect;
  }
}
