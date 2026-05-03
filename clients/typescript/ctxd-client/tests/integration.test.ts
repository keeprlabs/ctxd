/**
 * End-to-end integration tests for @ctxd/client.
 *
 * Covers the same surface as the Rust + Python SDKs: write, query,
 * subscribe, grant, revoke, peers, peerRemove, stats, health, and
 * verifySignature. Each test spawns a fresh daemon for isolation —
 * tests are sub-second after the warm-up cost of building the binary.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  CtxdClient,
  NotFoundError,
  Operation,
  UnexpectedWireResponseError,
  WireNotConfiguredError,
} from "../src/index.js";
import { type SpawnedDaemon, spawnDaemon } from "./common.js";

describe("CtxdClient integration", () => {
  let daemon: SpawnedDaemon;

  beforeAll(async () => {
    daemon = await spawnDaemon();
  }, 60_000);

  afterAll(async () => {
    await daemon.cleanup();
  });

  it("health() returns status=ok and a supported daemon version", async () => {
    const client = new CtxdClient({ httpUrl: daemon.httpUrl });
    try {
      const info = await client.health();
      expect(info.status).toBe("ok");
      // SDK supports 0.3.x and 0.4.x daemons — wire format is pinned,
      // minor versions track new features that don't break the SDK.
      const supported =
        info.version.startsWith("0.3.") || info.version.startsWith("0.4.");
      expect(supported).toBe(true);
    } finally {
      await client.close();
    }
  });

  it("write() then query() returns the just-written event", async () => {
    const client = new CtxdClient({
      httpUrl: daemon.httpUrl,
      wireAddr: daemon.wireAddr,
    });
    try {
      const eid = await client.write({
        subject: "/sdk-test/log/one",
        eventType: "ctx.note",
        data: { content: "hello sdk" },
      });
      expect(eid).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-/);

      const events = await client.query({
        subjectPattern: "/sdk-test/log/one",
      });
      const found = events.find((e) => e.id === eid);
      expect(found).toBeTruthy();
      expect(found?.type).toBe("ctx.note");
    } finally {
      await client.close();
    }
  });

  it("subscribe() yields a published event", async () => {
    const client = new CtxdClient({
      httpUrl: daemon.httpUrl,
      wireAddr: daemon.wireAddr,
    });
    const publisher = new CtxdClient({
      httpUrl: daemon.httpUrl,
      wireAddr: daemon.wireAddr,
    });
    try {
      // Open the subscription FIRST so the daemon registers the
      // broadcast receiver before we publish.
      const stream = client.subscribe("/sdk-test/sub/**");

      // Schedule a delayed publish on a separate connection. 50ms is
      // generous on every CI we've seen but leaves race-margin for
      // slow runners.
      const publishPromise = (async (): Promise<string> => {
        await new Promise((r) => setTimeout(r, 50));
        return publisher.write({
          subject: "/sdk-test/sub/event",
          eventType: "ctx.note",
          data: { msg: "via sub" },
        });
      })();

      // Race the first event against a 5s timeout so a daemon hang
      // fails the test rather than wedging the runner.
      const next = await Promise.race([
        stream.next(),
        new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error("subscribe timeout (5s)")), 5_000),
        ),
      ]);

      expect(next.done).toBe(false);
      const writtenId = await publishPromise;
      if (!next.done) {
        expect(next.value.id).toBe(writtenId);
        expect(next.value.type).toBe("ctx.note");
      }

      // Drain the generator so it closes its TCP connection cleanly.
      await stream.return(undefined);
    } finally {
      await publisher.close();
      await client.close();
    }
  });

  it("write() without wireAddr throws WireNotConfiguredError", async () => {
    const client = new CtxdClient({ httpUrl: daemon.httpUrl });
    try {
      await expect(
        client.write({ subject: "/x", eventType: "demo", data: {} }),
      ).rejects.toBeInstanceOf(WireNotConfiguredError);
    } finally {
      await client.close();
    }
  });

  it("grant() returns a base64-shaped biscuit token", async () => {
    const client = new CtxdClient({
      httpUrl: daemon.httpUrl,
      wireAddr: daemon.wireAddr,
    });
    try {
      const token = await client.grant({
        subject: "/sdk-test/grant/**",
        operations: [Operation.Read, Operation.Subjects],
      });
      expect(token.length).toBeGreaterThan(32);
      // Biscuit tokens use base64 (URL-safe alphabet).
      expect(token).toMatch(/^[A-Za-z0-9_\-=]+$/);
    } finally {
      await client.close();
    }
  });

  it("revoke() surfaces the daemon's stub error", async () => {
    // The daemon's REVOKE handler is a stub today; SDK surfaces the
    // error via UnexpectedWireResponseError. Same behavior as Rust
    // and Python.
    const client = new CtxdClient({
      httpUrl: daemon.httpUrl,
      wireAddr: daemon.wireAddr,
    });
    try {
      await expect(
        client.revoke({ tokenId: "any-id" }),
      ).rejects.toBeInstanceOf(UnexpectedWireResponseError);
    } finally {
      await client.close();
    }
  });

  it("peers() starts empty and peerRemove() of an unknown id 404s", async () => {
    const client = new CtxdClient({
      httpUrl: daemon.httpUrl,
      wireAddr: daemon.wireAddr,
    });
    try {
      // /v1/peers requires admin; mint an admin token and re-attach.
      const adminToken = await client.grant({
        subject: "/",
        operations: [Operation.Admin],
      });

      const admin = new CtxdClient({
        httpUrl: daemon.httpUrl,
        token: adminToken,
      });
      try {
        const peers = await admin.peers();
        expect(peers).toEqual([]);

        await expect(
          admin.peerRemove({ peerId: "does-not-exist" }),
        ).rejects.toBeInstanceOf(NotFoundError);
      } finally {
        await admin.close();
      }
    } finally {
      await client.close();
    }
  });

  it("stats() returns subject_count >= 1 after a write", async () => {
    const client = new CtxdClient({
      httpUrl: daemon.httpUrl,
      wireAddr: daemon.wireAddr,
    });
    try {
      await client.write({
        subject: "/sdk-test/stats/one",
        eventType: "ctx.note",
        data: { k: "v" },
      });
      const stats = await client.stats();
      expect(stats.subject_count).toBeGreaterThanOrEqual(1);
    } finally {
      await client.close();
    }
  });

  it("verifySignature() exists on CtxdClient as a static method", () => {
    expect(typeof CtxdClient.verifySignature).toBe("function");
  });
});
