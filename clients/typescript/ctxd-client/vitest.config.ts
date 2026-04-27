import { defineConfig } from "vitest/config";

/**
 * Vitest configuration for the @ctxd/client SDK.
 *
 * - `globalSetup` builds the workspace `ctxd` binary once per test
 *   run so integration tests can spawn a real daemon without each
 *   test paying the cargo build cost.
 * - The pool is single-threaded for the integration suite; loopback
 *   port races between concurrent daemon spawns are real and
 *   miserable to debug.
 * - Daemon startup can take a few seconds on cold caches, so the
 *   default `testTimeout` is bumped to 30 s.
 */
export default defineConfig({
  test: {
    globalSetup: ["./tests/globalSetup.ts"],
    testTimeout: 30_000,
    hookTimeout: 30_000,
    pool: "forks",
    poolOptions: {
      forks: {
        singleFork: true,
      },
    },
    include: ["tests/**/*.test.ts"],
    reporters: ["default"],
  },
});
