/**
 * Node-only wire-protocol client over `node:net` + `@msgpack/msgpack`.
 *
 * One TCP connection per `WireClient`. The daemon puts a connection
 * into streaming-receive mode after a `Sub`, so subscriptions take
 * exclusive ownership of a connection — `subscribe()` opens a fresh
 * TCP connection per call.
 *
 * Framing: 4-byte big-endian length prefix, then a single MessagePack
 * value. Max frame size is 16 MiB; oversize frames are rejected
 * before allocation.
 *
 * Concurrency: methods that send a request and read a response are
 * mutually exclusive — do not interleave from multiple async tasks
 * against the same `WireClient`. The internal request lock serializes
 * writes; consumers that want concurrency should hold one client per
 * task.
 */
import * as net from "node:net";

import {
  UnexpectedWireResponseError,
  WireError,
} from "./errors.js";
import { type Event, parseEvent } from "./events.js";
import {
  type WireRequest,
  decodeResponse,
  encodeRequest,
  expectOk,
  frameHeader,
  makeRequest,
  MAX_FRAME_BYTES,
  readFrameLength,
} from "./wire-codec.js";

/** A single TCP connection to the daemon's wire-protocol port. */
export class WireClient {
  private readonly socket: net.Socket;
  private readonly addr: string;
  private buffer: Uint8Array = new Uint8Array(0);
  private readers: Array<{
    resolve: (frame: Uint8Array | null) => void;
    reject: (err: Error) => void;
  }> = [];
  private requestLock: Promise<void> = Promise.resolve();
  private closed = false;
  private readError: Error | null = null;

  private constructor(socket: net.Socket, addr: string) {
    this.socket = socket;
    this.addr = addr;
    socket.on("data", (chunk) => this.onData(chunk));
    socket.on("error", (err) => this.onError(err));
    socket.on("close", () => this.onClose());
  }

  /** Open a fresh TCP connection to `host:port`. */
  static async connect(addr: string): Promise<WireClient> {
    const [host, portStr] = splitAddr(addr);
    const port = Number.parseInt(portStr, 10);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new WireError(`invalid wire port: ${portStr}`);
    }
    return new Promise<WireClient>((resolve, reject) => {
      const socket = net.createConnection({ host, port });
      const onError = (err: Error): void => {
        socket.removeListener("connect", onConnect);
        reject(new WireError(`failed to connect wire ${addr}: ${err.message}`));
      };
      const onConnect = (): void => {
        socket.removeListener("error", onError);
        // Disable Nagle so small request/response cycles don't
        // accumulate the 40ms ack delay penalty.
        socket.setNoDelay(true);
        resolve(new WireClient(socket, addr));
      };
      socket.once("error", onError);
      socket.once("connect", onConnect);
    });
  }

  /** Address this connection was opened against. */
  get address(): string {
    return this.addr;
  }

  /** Close the underlying TCP connection. Idempotent. */
  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    await new Promise<void>((resolve) => {
      this.socket.end(() => resolve());
      // Belt-and-braces: if `end` doesn't fire the callback (e.g.
      // already-closed socket), destroy + resolve next tick.
      setTimeout(() => {
        this.socket.destroy();
        resolve();
      }, 100);
    });
  }

  /** Send `Ping`, expect `Pong`. */
  async ping(): Promise<void> {
    const resp = await this.request("Ping");
    if (resp !== "Pong") {
      throw new UnexpectedWireResponseError(
        `expected Pong, got ${JSON.stringify(resp)}`,
      );
    }
  }

  /** Send `Pub`, return the new event's UUIDv7 id. */
  async publish(
    subject: string,
    eventType: string,
    data: unknown,
  ): Promise<string> {
    const req = makeRequest("Pub", {
      subject,
      event_type: eventType,
      data,
    });
    const resp = await this.request(req);
    const ok = unwrapOk(resp);
    if (
      !ok ||
      typeof ok !== "object" ||
      Array.isArray(ok) ||
      typeof (ok as Record<string, unknown>).id !== "string"
    ) {
      throw new UnexpectedWireResponseError(
        `Pub response missing string \`id\` field: ${JSON.stringify(ok)}`,
      );
    }
    return (ok as { id: string }).id;
  }

  /** Send `Query`, return the parsed Event list. */
  async query(subjectPattern: string, view: string): Promise<Event[]> {
    if (view === "kv") {
      throw new UnexpectedWireResponseError(
        "kv view returns a value, not a list of events; use the wire APIs directly",
      );
    }
    const req = makeRequest("Query", {
      subject_pattern: subjectPattern,
      view,
    });
    const resp = await this.request(req);
    const ok = unwrapOk(resp);
    if (!Array.isArray(ok)) {
      throw new UnexpectedWireResponseError(
        `Query response data is not a list: ${JSON.stringify(ok)}`,
      );
    }
    return ok.map((e) => parseEvent(e));
  }

  /** Send `Revoke`. Throws on `Error` response. */
  async revoke(capId: string): Promise<void> {
    const req = makeRequest("Revoke", { cap_id: capId });
    const resp = await this.request(req);
    unwrapOk(resp);
  }

  /**
   * Subscribe to events matching `subjectPattern`. Opens a *fresh* TCP
   * connection (a `Sub` puts the daemon-side socket into
   * streaming-receive mode and we can't reuse it for further
   * requests). Returns an async iterator; iterate with `for await`.
   */
  async *subscribe(subjectPattern: string): AsyncGenerator<Event, void, void> {
    const sub = await WireClient.connect(this.addr);
    try {
      const req = makeRequest("Sub", { subject_pattern: subjectPattern });
      await sub.writeFrame(encodeRequest(req));
      while (true) {
        const body = await sub.readFrame();
        if (body === null) return;
        const resp = decodeResponse(body);
        if (
          resp &&
          typeof resp === "object" &&
          !Array.isArray(resp) &&
          "Event" in (resp as Record<string, unknown>)
        ) {
          const inner = (resp as Record<string, unknown>).Event;
          if (Array.isArray(inner) && inner.length === 1) {
            yield parseEvent(inner[0]);
          } else if (inner && typeof inner === "object") {
            // Tolerate map-form for codecs that emit named-fields.
            const obj = inner as Record<string, unknown>;
            yield parseEvent(obj.event ?? obj);
          } else {
            throw new UnexpectedWireResponseError(
              `unexpected Event shape: ${JSON.stringify(resp)}`,
            );
          }
        } else if (resp === "EndOfStream") {
          return;
        } else if (
          resp &&
          typeof resp === "object" &&
          !Array.isArray(resp) &&
          "Error" in (resp as Record<string, unknown>)
        ) {
          const inner = (resp as Record<string, unknown>).Error;
          const msg =
            Array.isArray(inner) && inner.length === 1
              ? String(inner[0])
              : JSON.stringify(inner);
          throw new UnexpectedWireResponseError(msg);
        } else {
          throw new UnexpectedWireResponseError(
            `expected Event/EndOfStream, got ${JSON.stringify(resp)}`,
          );
        }
      }
    } finally {
      await sub.close();
    }
  }

  // ----- Internals -----

  private async request(req: WireRequest): Promise<unknown> {
    // Serialize on the lock so concurrent callers against the same
    // WireClient don't interleave a request and a response.
    const release = await this.acquireLock();
    try {
      await this.writeFrame(encodeRequest(req));
      const body = await this.readFrame();
      if (body === null) {
        throw new WireError("connection closed before response");
      }
      return decodeResponse(body);
    } finally {
      release();
    }
  }

  private async acquireLock(): Promise<() => void> {
    const prev = this.requestLock;
    let release!: () => void;
    this.requestLock = new Promise<void>((resolve) => {
      release = resolve;
    });
    await prev;
    return release;
  }

  private async writeFrame(payload: Uint8Array): Promise<void> {
    if (payload.length > MAX_FRAME_BYTES) {
      throw new WireError(`frame too large to send: ${payload.length} bytes`);
    }
    const header = frameHeader(payload.length);
    await new Promise<void>((resolve, reject) => {
      this.socket.write(header, (err) => {
        if (err) {
          reject(new WireError(`failed to write frame header: ${err.message}`));
          return;
        }
        this.socket.write(payload, (err2) => {
          if (err2) {
            reject(new WireError(`failed to write frame body: ${err2.message}`));
            return;
          }
          resolve();
        });
      });
    });
  }

  private readFrame(): Promise<Uint8Array | null> {
    // Try to satisfy from the buffer first; if not enough, queue a
    // pending reader that `onData` will resolve.
    return new Promise<Uint8Array | null>((resolve, reject) => {
      if (this.readError) {
        reject(this.readError);
        return;
      }
      this.readers.push({ resolve, reject });
      this.tryDeliver();
    });
  }

  private tryDeliver(): void {
    while (this.readers.length > 0) {
      if (this.buffer.length < 4) {
        // Not enough for header. If the socket has closed and we have
        // *no* bytes at all, deliver a clean EOF; otherwise wait for
        // more data (or a close notification).
        if (this.closed && this.buffer.length === 0) {
          const reader = this.readers.shift();
          if (reader) reader.resolve(null);
          continue;
        }
        return;
      }
      const length = readFrameLength(this.buffer.subarray(0, 4));
      if (length > MAX_FRAME_BYTES) {
        const err = new WireError(`frame too large: ${length} bytes`);
        this.readError = err;
        const reader = this.readers.shift();
        if (reader) reader.reject(err);
        return;
      }
      if (this.buffer.length < 4 + length) {
        // Header present but body still arriving.
        if (this.closed) {
          const err = new WireError("truncated frame body");
          this.readError = err;
          const reader = this.readers.shift();
          if (reader) reader.reject(err);
          continue;
        }
        return;
      }
      const body = this.buffer.subarray(4, 4 + length);
      // Copy out so the consumer owns its bytes.
      const owned = new Uint8Array(body);
      this.buffer = this.buffer.subarray(4 + length);
      const reader = this.readers.shift();
      if (reader) reader.resolve(owned);
    }
  }

  private onData(chunk: Buffer): void {
    // Concat chunk -> buffer.
    const next = new Uint8Array(this.buffer.length + chunk.length);
    next.set(this.buffer, 0);
    next.set(chunk, this.buffer.length);
    this.buffer = next;
    this.tryDeliver();
  }

  private onError(err: Error): void {
    const wireErr = new WireError(`wire socket error: ${err.message}`);
    this.readError = wireErr;
    while (this.readers.length > 0) {
      const reader = this.readers.shift();
      if (reader) reader.reject(wireErr);
    }
  }

  private onClose(): void {
    this.closed = true;
    this.tryDeliver();
  }
}

function splitAddr(addr: string): [string, string] {
  const idx = addr.lastIndexOf(":");
  if (idx <= 0 || idx === addr.length - 1) {
    throw new WireError(`invalid wire address: ${addr}`);
  }
  return [addr.substring(0, idx), addr.substring(idx + 1)];
}

function unwrapOk(resp: unknown): unknown {
  try {
    return expectOk(resp);
  } catch (err) {
    throw new UnexpectedWireResponseError((err as Error).message);
  }
}
