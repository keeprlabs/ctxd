# TODOS

Deferred work captured during planning and review. Each item has enough context that anyone (or any agent) can pick it up cold.

Format per the gstack convention: **What / Why / Pros / Cons / Context / Effort / Priority / Depends on**.

Effort scales: human team → CC+gstack. Priority: P1 (next up), P2 (soon), P3 (someday), P4 (blocked or speculative).

---

## Dashboard v2 — Subjects-as-graph view

**What:** Add a "related subjects" panel on the subject-detail view, surfacing subjects that share entities with the current one. Render as a small node-link diagram (or a flat list with relationship counts in v2.0; full graph in v2.1).

**Why:** ctxd's real differentiator vs flat event stores is the addressable hierarchy + entity/relationship graph. The dashboard v1 shows the hierarchy and ignores the graph, which underwhelms power users. The graph is the v2 headline visual.

**Pros:** differentiated visual, surfaces the substrate's superpower, drives stickiness for power users.
**Cons:** larger lift than v1 cherry-picks, requires UI design work (graph rendering, layout), risks over-investing before adoption is proven.

**Context:** ctxd-store-sqlite already exposes `EntityRow` and `RelationshipRow` from `EntityQuery`. New endpoint: `/v1/subjects/:path/related?depth=N`. Frontend renders as a side panel on the subject-detail view. Consider whether a force-directed layout (d3) is worth the JS toolchain cost or if a flat ranked list is enough for v2.0.

**Effort:** human L → CC M.
**Priority:** P2 (target v2 release, ~4-8 weeks post-dashboard launch).
**Depends on:** dashboard v1 ships and is dogfooded. v2 design review.

---

## Dashboard v2 — Time-travel slider via `read_at`

**What:** UI slider on the events view that lets the user scrub backward in time and see the substrate's state at that moment. Backed by the existing `EventStore::read_at(subject, as_of)` method.

**Why:** ctxd is event-sourced. Without a temporal UI, the substrate's "show me the state at any past instant" capability is invisible. This is one of the cheapest ways to communicate "this is more than a kv-store."

**Pros:** unique capability, easy to demo ("rewind your AI's memory"), reuses existing store method.
**Cons:** UX is non-trivial (how does the user pick "2 hours ago" without a calendar widget?); limited utility once an event log is large.

**Context:** `EventStore::read_at` exists and is tested. Endpoint: `/v1/events?as_of=<ISO8601>`. UI: slider component on the subjects view, live-updates events as the slider moves. Throttle requests aggressively.

**Effort:** human M → CC S.
**Priority:** P3.
**Depends on:** dashboard v1.

---

## Dashboard v2 — Vector + hybrid search UI

**What:** Search view exposes vector and hybrid (vector + FTS) search, not just FTS. Show distance scores, similar-events panel, semantic browse.

**Why:** v1 search is FTS-only. ctxd already supports vector and hybrid search via MCP (`ctx_search`); v1 dashboard hides this entirely.

**Pros:** showcases the AI-substrate angle directly, useful for fuzzy queries where FTS fails.
**Cons:** requires embedder configured at the daemon level (not all installs), adds complexity to the search UI (mode toggle, score interpretation).

**Context:** `EventStore::vector_search` and the hybrid search composer already exist. New endpoint: `/v1/search?mode=vector|fts|hybrid`. UI: mode tabs above search results.

**Effort:** human L → CC M.
**Priority:** P3.
**Depends on:** dashboard v1; embedder telemetry to know when to show vector mode (greyed-out if no embedder).

---

## Dashboard v2 — Federation health metrics

**What:** Peers tab adds replication lag, last successful sync time, error rate per peer. Surfaces federation health as a first-class signal.

**Why:** Federation is ctxd's most operationally-complex feature. Without health metrics, debugging a peer issue means reading logs.

**Pros:** unblocks federation as a daily-driver feature, foundation for alerting later.
**Cons:** needs telemetry plumbing on the wire side (not just dashboard work). Bigger lift than it looks.

**Context:** `peer_cursors` table exists with `last_event_id` and `last_event_time`. Health derives from comparing those to `now()` and to the local log's tip.

**Effort:** human L → CC M.
**Priority:** P3.
**Depends on:** wire-side telemetry (per-peer success/error counters), which is its own project.

---

## Dashboard v3 — Capability-token UI (mint, revoke, scope viewer)

**What:** UI for minting tokens, revoking them, and viewing the granted scope of an existing token.

**Why:** Today this is `ctxd grant ...` only. Operators want a UI when granting tokens to multiple agents/services.

**Pros:** completes the operator console story; reduces docs surface ("how do I mint a read-only token?" becomes a button).
**Cons:** requires remote auth model first (this is a privileged action — can't be loopback-bypassed in production).

**Context:** `cap-engine` exposes mint/verify. `/v1/grant` already exists (cap-token-gated). UI: forms + scope picker + token display modal.

**Effort:** human XL → CC L.
**Priority:** P4 (blocked).
**Depends on:** remote auth model design (separate plan), TLS termination decisions.

---

## Dashboard v3 — Write actions beyond hello-world

**What:** Allow event creation, subject deletion, peer approve/deny, etc. from the UI.

**Why:** Once the dashboard is the operator console, operators expect to do operator things from it.

**Pros:** completes the control-plane story.
**Cons:** every write surface is a new threat model. Each write action needs auth, audit trail, undo semantics, and review. Significant ongoing work.

**Context:** Each action gets its own endpoint, its own threat model, its own UI. Don't ship as one PR; ship per-action.

**Effort:** human XL → CC L (per action, then aggregated).
**Priority:** P4 (blocked).
**Depends on:** capability-token UI (above), audit log infrastructure.

---

## Dashboard v3 — Light theme

**What:** Add a light theme via CSS variable swap. Maintain accessibility (contrast ratios) in both themes.

**Why:** Some users genuinely prefer light themes; some tools (presentation, screenshots in docs) work better in light.

**Pros:** trivial once tokens are stable; broadens audience marginally.
**Cons:** doubles the screenshot/QA matrix. Must verify a11y in both modes.

**Context:** `style.css` uses CSS variables. Adding a `[data-theme="light"]` block plus a toggle in the header takes ~50 LOC.

**Effort:** human S → CC S.
**Priority:** P4 (low signal until users ask for it).
**Depends on:** dashboard v1 ships and tokens stabilize.

---

## VHS dashboard recording (v1 launch dependency)

**What:** Record a `dashboard.tape` mirroring the existing `assets/vhs/install.tape` style. Tour: `ctxd dashboard` → browser opens → overview → subjects → search → close.

**Why:** The README's headline visual today is the install GIF. The dashboard's launch needs its own GIF as the new headline.

**Pros:** hard requirement for the launch (Step 10 of dashboard v1 plan).
**Cons:** none — this is a requirement, not an option.

**Context:** Already in the dashboard v1 plan as Step 10. Listed here as a TODO so it doesn't get lost if the plan is split.

**Effort:** human S → CC S (mostly time spent fine-tuning the tape).
**Priority:** P1.
**Depends on:** dashboard v1 implementation complete.

---

## Mobile-first dashboard design

**What:** Make the dashboard work cleanly on phones — collapsing nav, stacked tables, larger touch targets.

**Why:** Some users will pull up the dashboard on their phone to check "is my AI still writing things." Not the primary use case but real.

**Pros:** broader audience; nice-to-have for ops folks on the go.
**Cons:** non-trivial CSS work; competes with v2 features for time.

**Context:** v1 targets desktop ≥ 1024px; tablet works; mobile is bonus per the v1 plan. v2 makes mobile a real target.

**Effort:** human M → CC S.
**Priority:** P3.
**Depends on:** dashboard v1; user signal that mobile matters.
