/**
 * HTTP admin client for the ctxd daemon.
 *
 * Hand-written types matching `docs/api/openapi.yaml`. We deliberately
 * avoid OpenAPI codegen for v0.3 — the surface is small, the shapes
 * are stable, and a hand-rolled client is easier to read in an
 * incident.
 *
 * Auth model: every constructor accepts an optional bearer token.
 * When set, it is attached as `Authorization: Bearer <token>` on
 * every request. Endpoints documented as open (`/health`,
 * `/v1/grant`, `/v1/stats`, `/v1/approvals`) tolerate the header
 * anyway, so we send it unconditionally when configured.
 *
 * Transport: relies on the runtime's global `fetch` — Node 20+ ships
 * undici under the hood; modern browsers ship native fetch. Same code
 * runs in both.
 *
 * Logging: this module never logs the bearer token, capability bytes,
 * or signature material. Status codes and body excerpts are fair
 * game; the secret material is not.
 */
import { httpStatusError, UnexpectedWireResponseError } from "./errors.js";
import { type Operation } from "./operation.js";

/** `GET /health` response body. */
export interface HealthInfo {
  status: string;
  version: string;
}

/** `GET /v1/stats` response body. Future versions may add fields. */
export interface StatsInfo {
  subject_count: number;
  [k: string]: unknown;
}

/** `GET /v1/peers` response item. */
export interface PeerInfo {
  peer_id: string;
  url: string;
  public_key: string;
  subject_patterns: string[];
  added_at: string;
  last_seen_at?: string | null;
}

/** Per-call HTTP timeout. Long enough to forgive a slow loopback under contention,
 *  short enough that a misconfigured URL fails loudly rather than hanging. */
export const DEFAULT_TIMEOUT_MS = 10_000;

/** Construction options for the HTTP admin client. */
export interface HttpAdminClientOptions {
  /** Bearer token attached to every request. */
  token?: string;
  /** Per-call HTTP timeout in milliseconds. Default: {@link DEFAULT_TIMEOUT_MS}. */
  timeoutMs?: number;
  /**
   * Override the global `fetch` (e.g. for Bun / older runtimes /
   * test doubles). Defaults to the global `fetch`.
   */
  fetch?: typeof fetch;
}

/** Async HTTP admin client. */
export class HttpAdminClient {
  private readonly base: string;
  private token: string | undefined;
  private readonly timeoutMs: number;
  private readonly fetchImpl: typeof fetch;

  constructor(baseUrl: string, options: HttpAdminClientOptions = {}) {
    this.base = baseUrl.replace(/\/+$/, "");
    this.token = options.token;
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    const f = options.fetch ?? globalThis.fetch;
    if (typeof f !== "function") {
      throw new Error(
        "no global fetch found; pass `fetch` via HttpAdminClientOptions on this runtime",
      );
    }
    // Re-bind so `this` doesn't get lost on detached calls (matters
    // for some polyfills + browsers).
    this.fetchImpl = f.bind(globalThis);
  }

  /** Base URL the client is pointed at (with any trailing slash stripped). */
  get baseUrl(): string {
    return this.base;
  }

  /** Bearer token attached to every request, or `undefined`. */
  get bearerToken(): string | undefined {
    return this.token;
  }

  /**
   * Attach a capability token to all admin calls. Replaces any
   * previously attached token.
   */
  withToken(token: string): this {
    this.token = token;
    return this;
  }

  /** `GET /health` — daemon liveness + version probe. */
  async health(): Promise<HealthInfo> {
    const json = (await this.getJson("/health")) as Record<string, unknown>;
    return {
      status: String(json.status),
      version: String(json.version),
    };
  }

  /** `GET /v1/stats` — basic store statistics. */
  async stats(): Promise<StatsInfo> {
    const json = (await this.getJson("/v1/stats")) as Record<string, unknown>;
    const sc = json.subject_count;
    if (typeof sc !== "number") {
      throw new UnexpectedWireResponseError(
        `stats response missing numeric subject_count: ${JSON.stringify(json)}`,
      );
    }
    return { ...json, subject_count: sc };
  }

  /**
   * `POST /v1/grant` — mint a capability token.
   *
   * Returns the base64-encoded biscuit token. The optional `expiresAt`
   * is converted to `expires_in_secs` server-side; we clamp at 1 so
   * passing a past timestamp surfaces a clear 400 from the server
   * rather than an "expires_in_secs: 0" rejection.
   */
  async grant(
    subject: string,
    operations: ReadonlyArray<Operation | string>,
    expiresAt?: Date,
  ): Promise<string> {
    const ops = operations.map((o) => String(o));
    const body: Record<string, unknown> = {
      subject,
      operations: ops,
    };
    if (expiresAt instanceof Date) {
      const deltaSec = Math.floor((expiresAt.getTime() - Date.now()) / 1000);
      body.expires_in_secs = Math.max(1, deltaSec);
    }
    const resp = (await this.postJson("/v1/grant", body)) as Record<
      string,
      unknown
    >;
    const token = resp.token;
    if (typeof token !== "string") {
      throw new UnexpectedWireResponseError(
        `grant response missing string \`token\` field: ${JSON.stringify(resp)}`,
      );
    }
    return token;
  }

  /** `GET /v1/peers` — list federation peers (admin). */
  async peers(): Promise<PeerInfo[]> {
    const json = (await this.getJson("/v1/peers")) as Record<string, unknown>;
    const list = Array.isArray(json.peers) ? json.peers : [];
    return list.map((p: unknown) => parsePeer(p));
  }

  /** `DELETE /v1/peers/{peerId}` — remove a federation peer (admin). */
  async peerRemove(peerId: string): Promise<void> {
    await this.delete(`/v1/peers/${encodeURIComponent(peerId)}`);
  }

  // ----- Internals -----

  private headers(): Record<string, string> {
    const h: Record<string, string> = {
      Accept: "application/json",
    };
    if (this.token) {
      h.Authorization = `Bearer ${this.token}`;
    }
    return h;
  }

  private async getJson(path: string): Promise<unknown> {
    const url = `${this.base}${path}`;
    const resp = await this.fetchWithTimeout(url, {
      method: "GET",
      headers: this.headers(),
    });
    return decodeJson(resp);
  }

  private async postJson(
    path: string,
    body: Record<string, unknown>,
  ): Promise<unknown> {
    const url = `${this.base}${path}`;
    const resp = await this.fetchWithTimeout(url, {
      method: "POST",
      headers: {
        ...this.headers(),
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });
    return decodeJson(resp);
  }

  private async delete(path: string): Promise<void> {
    const url = `${this.base}${path}`;
    const resp = await this.fetchWithTimeout(url, {
      method: "DELETE",
      headers: this.headers(),
    });
    if (resp.status < 200 || resp.status >= 300) {
      const text = await safeText(resp);
      throw httpStatusError(resp.status, text);
    }
  }

  private async fetchWithTimeout(
    url: string,
    init: RequestInit,
  ): Promise<Response> {
    const controller = new AbortController();
    const t = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      return await this.fetchImpl(url, { ...init, signal: controller.signal });
    } finally {
      clearTimeout(t);
    }
  }
}

async function decodeJson(resp: Response): Promise<unknown> {
  if (resp.status >= 200 && resp.status < 300) {
    if (resp.status === 204) return null;
    const text = await resp.text();
    if (text.length === 0) return null;
    try {
      return JSON.parse(text);
    } catch (e) {
      throw new UnexpectedWireResponseError(
        `expected JSON body, got ${(e as Error).message}: ${text.slice(0, 200)}`,
      );
    }
  }
  const text = await safeText(resp);
  throw httpStatusError(resp.status, text);
}

async function safeText(resp: Response): Promise<string> {
  try {
    return await resp.text();
  } catch {
    return "";
  }
}

function parsePeer(p: unknown): PeerInfo {
  if (!p || typeof p !== "object") {
    throw new UnexpectedWireResponseError(
      `peer entry not an object: ${JSON.stringify(p)}`,
    );
  }
  const r = p as Record<string, unknown>;
  return {
    peer_id: String(r.peer_id ?? ""),
    url: String(r.url ?? ""),
    public_key: String(r.public_key ?? ""),
    subject_patterns: Array.isArray(r.subject_patterns)
      ? r.subject_patterns.map(String)
      : [],
    added_at: String(r.added_at ?? ""),
    last_seen_at:
      typeof r.last_seen_at === "string" ? r.last_seen_at : null,
  };
}
