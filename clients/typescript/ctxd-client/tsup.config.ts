import { defineConfig } from "tsup";

/**
 * Build matrix:
 *
 * - `dist/index.js`         — Node ESM (default; pulls in `wire.node.ts`).
 * - `dist/index.cjs`        — Node CommonJS.
 * - `dist/index.browser.js` — Browser ESM (re-exports `wire.browser.ts`).
 * - `dist/index.d.ts`       — Shared type declarations.
 *
 * The browser bundle MUST NOT contain any `node:*` imports — verified
 * by a smoke test in CI. We achieve this by routing the wire entry
 * via a separate file (`wire.browser.ts` vs `wire.node.ts`).
 */
export default defineConfig([
  // Node entry (ESM + CJS). Bundles `wire.node.ts`.
  {
    entry: { index: "src/index.ts" },
    format: ["esm", "cjs"],
    dts: { entry: "src/index.ts" },
    sourcemap: true,
    clean: true,
    target: "node20",
    platform: "node",
    splitting: false,
    treeshake: true,
    outDir: "dist",
  },
  // Browser entry (ESM only). Bundles `wire.browser.ts`.
  {
    entry: { "index.browser": "src/index.browser.ts" },
    format: ["esm"],
    dts: false,
    sourcemap: true,
    clean: false,
    target: "es2022",
    platform: "browser",
    splitting: false,
    treeshake: true,
    outDir: "dist",
  },
]);
