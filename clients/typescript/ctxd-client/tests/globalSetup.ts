/**
 * Vitest globalSetup — build the workspace ctxd binary once per run.
 *
 * Integration tests spawn a real ctxd daemon. Building the binary on
 * each test would dominate runtime; building it once up-front lets
 * the suite stay fast.
 *
 * If `cargo` is not on PATH, this throws with an actionable message.
 * If the build fails, the test run fails before any test executes,
 * which is exactly what we want.
 */
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";

import { findCtxdBinary, workspaceRoot } from "./common.js";

export default async function globalSetup(): Promise<() => void> {
  const ctxd = findCtxdBinary();
  if (existsSync(ctxd)) {
    return () => {
      /* nothing to clean up */
    };
  }

  await new Promise<void>((resolve, reject) => {
    const proc = spawn("cargo", ["build", "--quiet", "--bin", "ctxd"], {
      cwd: workspaceRoot(),
      stdio: ["ignore", "inherit", "inherit"],
    });
    proc.on("error", (err) => {
      reject(
        new Error(
          `cargo build failed to start (is cargo on PATH?): ${err.message}`,
        ),
      );
    });
    proc.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`cargo build --bin ctxd exited with code ${code}`));
    });
  });

  if (!existsSync(ctxd)) {
    throw new Error(
      `cargo build succeeded but ${ctxd} is missing — workspace layout drift?`,
    );
  }

  return () => {
    /* nothing to clean up */
  };
}
