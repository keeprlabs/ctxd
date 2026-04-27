/**
 * Capability operations.
 *
 * Mirrors `ctxd_cap::Operation` and the OpenAPI `operations[]` enum.
 * The wire serialization matches the daemon's snake_case names exactly.
 *
 * Modeled as a string-valued const enum object (rather than `enum`) so
 * the values are inlined at usage sites and the bundle stays tiny.
 */
export const Operation = {
  /** Read events under a subject. */
  Read: "read",
  /** Write (append) events. */
  Write: "write",
  /** List subject paths. */
  Subjects: "subjects",
  /** FTS / vector search. */
  Search: "search",
  /** Admin operations (mint tokens, manage peers). */
  Admin: "admin",
} as const;

/** A single capability operation as emitted on the wire. */
export type Operation = (typeof Operation)[keyof typeof Operation];

/** Type guard: is `value` a known wire-format operation string? */
export function isOperation(value: unknown): value is Operation {
  return (
    typeof value === "string" &&
    Object.values(Operation).includes(value as Operation)
  );
}
