/**
 * Browser stub for the wire-protocol client.
 *
 * The wire protocol speaks raw TCP + MessagePack — there's no path to
 * that from a browser sandbox. Per `docs/plans/sdks.md` decision #2
 * (default option a), v0.3 ships HTTP-only on the browser. Calling
 * any wire-protocol method from the browser bundle throws a typed
 * `WireError` so the failure mode is loud and unambiguous.
 *
 * The public API shape mirrors `wire.node.ts` so dual-mode consumers
 * (e.g. an isomorphic config layer that imports `WireClient` for
 * type-only purposes) can reference the type without breaking the
 * browser bundle.
 */
import { WireError } from "./errors.js";
import type { Event } from "./events.js";

export class WireClient {
  private constructor() {
    // Constructor itself never runs successfully in the browser; the
    // static `connect` factory throws before reaching here.
    throw new WireError(
      "wire protocol requires Node.js; @ctxd/client browser bundle is HTTP-only",
    );
  }

  static async connect(_addr: string): Promise<WireClient> {
    throw new WireError(
      "wire protocol requires Node.js; @ctxd/client browser bundle is HTTP-only",
    );
  }

  get address(): string {
    throw new WireError("wire protocol unavailable in browser");
  }

  async close(): Promise<void> {
    /* no-op — never opened */
  }

  async ping(): Promise<void> {
    throw new WireError("wire protocol unavailable in browser");
  }

  async publish(
    _subject: string,
    _eventType: string,
    _data: unknown,
  ): Promise<string> {
    throw new WireError("wire protocol unavailable in browser");
  }

  async query(_subjectPattern: string, _view: string): Promise<Event[]> {
    throw new WireError("wire protocol unavailable in browser");
  }

  async revoke(_capId: string): Promise<void> {
    throw new WireError("wire protocol unavailable in browser");
  }

  async *subscribe(
    _subjectPattern: string,
  ): AsyncGenerator<Event, void, void> {
    throw new WireError(
      "wire protocol unavailable in browser; subscriptions require Node.js",
    );
  }
}
