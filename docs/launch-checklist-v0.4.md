# v0.4 launch checklist — embedded web dashboard

Process, not code. The dashboard is shipped (steps 0–10) but features that don't ship distribution updates ship invisibly. This is what to do the day v0.4 lands.

## Success metric (decide before tagging, check at T+14)

By 14 days post-launch, at least one of:

- A user posts a screenshot of their dashboard to Discord, Twitter, or HN unprompted.
- GitHub stars net +50 over the launch week.
- 3+ issues filed about dashboard UX (signal of real usage; bug noise is fine, idea noise is fine, "I tried this and X is weird" is great).

If none hit, the dashboard didn't move the needle and v0.5 priorities should reflect that. If multiple hit, treat the graph view (TODOS.md item, deferred from this plan) as the v0.5 headline.

## Pre-launch (T-1 day)

- [ ] **Tag `v0.4-rc.1`** on crates.io (or a `-rc.1` Git tag if not yet wired). Dogfood for 24–48 hours on the maintainer's daily-driver setup. Run `ctxd dashboard` against `~/.ctxd/ctxd.db` and use it for actual Claude Desktop sessions. Look for: stale rows in the recent-events panel, a misbehaving SSE reconnect, broken empty-state click, weird subject tree counts.
- [ ] **Generate `assets/img/dashboard.gif`** from `assets/vhs/dashboard.tape`:
  ```bash
  brew install vhs    # if not already
  vhs assets/vhs/dashboard.tape
  ```
- [ ] **Capture 4 static dashboard screenshots** for the README and `docs/dashboard.md`:
      overview (populated), subjects view, search results page, peers view (with at least one peer or the empty state). 1200×720 PNG. Commit under `assets/img/dashboard-{view}.png` and reference from `docs/dashboard.md`.
- [ ] **Verify the GIF renders on github.com.** GitHub processes GIFs differently from local viewers — push to a branch, open the README on the branch view, watch the GIF play through. If it loops too fast or color-shifts, re-record with adjusted `Set PlaybackSpeed` / theme.
- [ ] **Draft tweet thread** (3–5 tweets):
  1. Hook: the personal pain ("I run ctxd against Claude Desktop and couldn't see what it was writing. Now I can.")
  2. The GIF (`dashboard.gif`).
  3. The command (`brew install keeprlabs/tap/ctxd && ctxd dashboard`).
  4. The security one-liner (loopback-only, DNS-rebinding defenses, read-only).
  5. Repo link.
- [ ] **Optional: draft a 300–500 word blog post** on the keeprlabs blog or as a HN Show submission. Title hook: "I added a dashboard to ctxd to see what my AI agents were writing." Lead with the dogfooding story, not the architecture.
- [ ] **Notify the cofounder.** ctxd-code overlap — the dashboard is a generic substrate viewer; ctxd-code's session-replay UX is its own product. Coordinate any cross-promotion.

## Launch day

- [ ] **Tag `v0.4`** on the GitHub repo. The release workflow (per a2bd3cb) publishes crates to crates.io.
- [ ] **Update Homebrew tap** (`keeprlabs/homebrew-tap`). The release workflow may already do this; verify the new bottle / formula bumps `version 0.4.0` and the tarball SHA matches the GitHub release artifact.
- [ ] **Verify fresh-machine install**:
  ```bash
  # On a machine without ctxd installed (or in a clean Docker container):
  brew install keeprlabs/tap/ctxd
  ctxd dashboard
  # → browser opens at http://127.0.0.1:7777/, dashboard loads, empty
  #   state shows the hello-world button, click → event appears.
  ```
- [ ] **Post the tweet thread.**
- [ ] **Submit Show HN** if the blog post is ready. Submit on a weekday morning Pacific (best engagement window).
- [ ] **Post in any relevant Discord / Slack / community channels** the maintainer follows.

## Post-launch (T+7, T+14)

- [ ] **T+7**: triage any dashboard-related issues. Note any UX surprises that didn't show up in dogfooding.
- [ ] **T+14**: check the success metric (top of this file). Decide whether v2 work (graph view, time-travel slider, vector search UI, write actions) starts immediately or waits for further signal.
- [ ] **Move TODOS.md "VHS dashboard recording" item to done** once the GIF is shipped (it's marked P1 there).
- [ ] **Close the v0.4 milestone** on GitHub and roll any unfinished items forward to v0.5.

## Anti-checklist (things NOT to do at launch)

- Don't promise SSE-based dashboards as a "feature" if the daemon is bound to anything other than loopback. The current security model is loopback-only; remote dashboards need a separate auth model (TODOS.md, v3).
- Don't claim "real-time" in marketing copy without qualification — SSE updates land within 100ms in our local-loopback tests, but a slow consumer or a buffered proxy can stretch that. "Live tail" is honest, "real-time" invites latency complaints.
- Don't conflate `ctxd dashboard` (HTTP-only convenience launcher) with `ctxd serve` (full daemon with wire/MCP/federation). Documentation already keeps them distinct; marketing should too.
