/**
 * Test harness for the @ctxd/client integration suite.
 *
 * Mirrors the Rust + Python harnesses: walk up to find Cargo.lock,
 * resolve the debug binary, pick free ports, spawn ctxd with a
 * tempdir DB, wait for both /health and the wire-protocol port to
 * accept connections.
 *
 * The wire-port wait is the lesson from the Rust SDK PR #6 fix —
 * /health going green doesn't guarantee the wire listener has bound,
 * and a flaky "connection refused" on the first wire call will eat
 * an afternoon.
 */
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import * as net from "node:net";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

/** Walk up from this file until we find Cargo.lock. */
export function workspaceRoot(): string {
  let cursor = __dirname;
  for (let i = 0; i < 10; i++) {
    if (existsSync(path.join(cursor, "Cargo.lock"))) return cursor;
    cursor = path.dirname(cursor);
  }
  throw new Error("Cargo.lock not found walking up from tests/common.ts");
}

/** Absolute path to the debug ctxd binary. May not exist yet. */
export function findCtxdBinary(): string {
  return path.join(workspaceRoot(), "target", "debug", "ctxd");
}

/** Pick a free TCP port on 127.0.0.1. Caller races the bind, of course. */
export async function pickFreePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      if (typeof addr === "object" && addr !== null) {
        const port = addr.port;
        server.close(() => resolve(port));
      } else {
        server.close();
        reject(new Error("server.address() returned unexpected shape"));
      }
    });
  });
}

/** Sleep for `ms` milliseconds. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Poll /health until it answers 200 or the deadline elapses. */
async function waitForHealth(httpUrl: string, deadlineMs = 30_000): Promise<void> {
  const started = Date.now();
  let lastErr: unknown = null;
  while (Date.now() - started < deadlineMs) {
    try {
      const resp = await fetch(`${httpUrl}/health`);
      if (resp.status === 200) return;
      lastErr = new Error(`status ${resp.status}`);
    } catch (e) {
      lastErr = e;
    }
    await sleep(100);
  }
  throw new Error(
    `daemon /health did not become ready within ${deadlineMs}ms; last error: ${String(lastErr)}`,
  );
}

/** Poll the wire port until a TCP connect succeeds. */
async function waitForWire(addr: string, deadlineMs = 30_000): Promise<void> {
  const [host, portStr] = addr.split(":");
  const port = Number(portStr);
  const started = Date.now();
  let lastErr: unknown = null;
  while (Date.now() - started < deadlineMs) {
    try {
      await new Promise<void>((resolve, reject) => {
        const sock = net.createConnection({ host, port });
        sock.once("connect", () => {
          sock.end();
          resolve();
        });
        sock.once("error", (err) => {
          sock.destroy();
          reject(err);
        });
      });
      return;
    } catch (e) {
      lastErr = e;
    }
    await sleep(100);
  }
  throw new Error(
    `wire port ${addr} never accepted connections; last error: ${String(lastErr)}`,
  );
}

/** Handles for a spawned daemon. */
export interface SpawnedDaemon {
  httpUrl: string;
  wireAddr: string;
  proc: ChildProcess;
  cleanup: () => Promise<void>;
}

/** Spawn a fresh ctxd daemon on free ports + tempdir DB. */
export async function spawnDaemon(): Promise<SpawnedDaemon> {
  const ctxd = findCtxdBinary();
  if (!existsSync(ctxd)) {
    throw new Error(`ctxd binary not found at ${ctxd} — run cargo build first`);
  }

  const httpPort = await pickFreePort();
  let wirePort = await pickFreePort();
  while (wirePort === httpPort) {
    wirePort = await pickFreePort();
  }

  const httpAddr = `127.0.0.1:${httpPort}`;
  const wireAddr = `127.0.0.1:${wirePort}`;
  const httpUrl = `http://${httpAddr}`;

  const tempDir = mkdtempSync(path.join(tmpdir(), "ctxd-ts-test-"));
  const dbPath = path.join(tempDir, "ctxd.db");

  const proc = spawn(
    ctxd,
    [
      "--db",
      dbPath,
      "serve",
      "--bind",
      httpAddr,
      "--wire-bind",
      wireAddr,
    ],
    {
      stdio: ["ignore", "pipe", "pipe"],
    },
  );

  const stderrChunks: Buffer[] = [];
  proc.stderr?.on("data", (chunk: Buffer) => stderrChunks.push(chunk));

  try {
    await waitForHealth(httpUrl);
    await waitForWire(wireAddr);
  } catch (e) {
    // Surface the daemon's stderr so the failure is debuggable.
    const stderr = Buffer.concat(stderrChunks).toString("utf8");
    proc.kill("SIGTERM");
    rmSync(tempDir, { recursive: true, force: true });
    throw new Error(
      `daemon failed to come up: ${String(e)}\nstderr:\n${stderr}`,
    );
  }

  const cleanup = async (): Promise<void> => {
    if (!proc.killed) {
      proc.kill("SIGTERM");
      // Give the daemon up to 5s to exit cleanly; otherwise SIGKILL.
      const killed = await new Promise<boolean>((resolve) => {
        const t = setTimeout(() => resolve(false), 5_000);
        proc.once("exit", () => {
          clearTimeout(t);
          resolve(true);
        });
      });
      if (!killed) proc.kill("SIGKILL");
    }
    rmSync(tempDir, { recursive: true, force: true });
  };

  return { httpUrl, wireAddr, proc, cleanup };
}
