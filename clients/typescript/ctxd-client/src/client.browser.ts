/**
 * Browser variant of {@link CtxdClient}.
 *
 * Identical to `client.ts` except it imports `WireClient` from the
 * browser stub (`wire.browser.ts`) instead of the Node implementation
 * (`wire.node.ts`). The Node + browser bundles are produced by
 * separate tsup builds, each picking up the right entry — that's how
 * we keep `node:net` out of the browser bundle without a runtime
 * shim.
 *
 * Wire-protocol methods (`write`, `query`, `subscribe`, `revoke`)
 * throw `WireError` from the browser bundle. HTTP admin methods
 * (`health`, `stats`, `grant`, `peers`, `peerRemove`) work normally.
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
import { WireClient } from "./wire.browser.js";

/** Construction options for {@link CtxdClient} (browser bundle). */
export interface CtxdClientOptions {
  httpUrl: string;
  wireAddr?: string;
  token?: string;
  timeoutMs?: number;
  fetch?: typeof fetch;
}

export interface WriteOptions {
  subject: string;
  eventType: string;
  data: unknown;
}

export interface QueryOptions {
  subjectPattern: string;
  view?: string;
}

export interface GrantOptions {
  subject: string;
  operations: ReadonlyArray<Operation | string>;
  expiresAt?: Date;
}

export interface RevokeOptions {
  tokenId: string;
}

export interface PeerRemoveOptions {
  peerId: string;
}

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

  get httpUrl(): string {
    return this.http.baseUrl;
  }

  get wireAddress(): string | undefined {
    return this.wireAddr;
  }

  withToken(token: string): this {
    this.http.withToken(token);
    return this;
  }

  async close(): Promise<void> {
    if (this.wire !== null) {
      await this.wire.close();
      this.wire = null;
    }
    this.wireConnect = null;
  }

  async health(): Promise<HealthInfo> {
    return this.http.health();
  }

  async stats(): Promise<StatsInfo> {
    return this.http.stats();
  }

  async grant(opts: GrantOptions): Promise<string> {
    return this.http.grant(opts.subject, opts.operations, opts.expiresAt);
  }

  async peers(): Promise<PeerInfo[]> {
    return this.http.peers();
  }

  async peerRemove(opts: PeerRemoveOptions): Promise<void> {
    return this.http.peerRemove(opts.peerId);
  }

  async write(opts: WriteOptions): Promise<string> {
    const wire = await this.requireWire();
    return wire.publish(opts.subject, opts.eventType, opts.data);
  }

  async query(opts: QueryOptions): Promise<Event[]> {
    const wire = await this.requireWire();
    return wire.query(opts.subjectPattern, opts.view ?? "log");
  }

  async *subscribe(
    subjectPattern: string,
  ): AsyncGenerator<Event, void, void> {
    const wire = await this.requireWire();
    yield* wire.subscribe(subjectPattern);
  }

  async revoke(opts: RevokeOptions): Promise<void> {
    const wire = await this.requireWire();
    await wire.revoke(opts.tokenId);
  }

  static verifySignature(event: Event, pubkeyHex: string): Promise<boolean> {
    return verifySignatureFn(event, pubkeyHex);
  }

  private async requireWire(): Promise<WireClient> {
    if (this.wire !== null) return this.wire;
    if (this.wireAddr === undefined) {
      throw new WireNotConfiguredError();
    }
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
