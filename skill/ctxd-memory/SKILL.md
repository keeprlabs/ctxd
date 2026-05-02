---
name: ctxd-memory
description: One-time setup for persistent, cross-tool memory across Claude Desktop, Claude Code, and Codex. Powered by the local-first ctxd substrate.
license: Apache-2.0
homepage: https://github.com/keeprlabs/ctxd
version: 0.1.0
---

# ctxd-memory

You are walking the user through a **one-time setup** for ctxd —
a local-first context substrate that gives every AI tool on their
machine a single shared memory. After this setup, anything they
tell Claude Desktop is visible to Claude Code, Codex, and any other
MCP-aware AI; their memory is owned by them, stored on their
machine, never sent to a third party they didn't authorize.

This skill does **no orchestration logic** itself. It shells to
`ctxd onboard --skill-mode` and narrates the JSON-Lines protocol the
binary emits. The protocol contract is documented in
[`docs/onboard-protocol.md`](https://github.com/keeprlabs/ctxd/blob/main/docs/onboard-protocol.md).

## What you'll do, in order

1. **Detect ctxd.** Confirm the user has the `ctxd` binary on their
   PATH. If they don't, offer to install via Homebrew (macOS) or the
   official install script (Linux).
2. **Confirm consent.** Tell the user *exactly* what's about to
   change on their machine. Get explicit "yes."
3. **Run `ctxd onboard --skill-mode`.** Parse JSON-Lines from
   stdout, narrate each step.
4. **Surface adapter prompts.** When the binary emits a `notice`
   message about an OAuth code or a config snippet, render it
   prominently and remind the user to come back when done.
5. **Show the doctor summary.** When the binary emits the terminal
   `done` message, render the doctor checklist as a green/red
   summary plus any remediation strings.
6. **Demo the value.** Walk the user through writing one fact via
   any connected AI, then show them it landed in the substrate by
   running `ctxd query`.

The whole flow should take **two minutes from first invocation to
first stored memory**. If you find yourself running long, either
something is broken (escalate via `ctxd doctor`) or the user is
stuck on an OAuth flow (offer to defer adapters with `--skip-adapters`).

---

## Step 1 — detect ctxd

Run:

```bash
which ctxd
```

* **If it returns a path** → continue. Tell the user the path you
  found ("ctxd at /opt/homebrew/bin/ctxd, version: $(ctxd --version)")
  and check the version is `0.4.0` or later. Older versions don't
  have the `onboard` command and need to be upgraded first.
* **If it errors** → offer to install:
  * macOS: `brew install keeprlabs/tap/ctxd`
  * Linux: `curl -fsSL https://ctxd.dev/install.sh | sh`
  * Windows: not supported in v0.4. Apologise and link
    https://github.com/keeprlabs/ctxd/issues for a feature request.

After install, confirm with `ctxd --version` and continue.

## Step 2 — confirm consent

Tell the user, in plain language, what's about to happen:

> I'm about to run `ctxd onboard`. This will:
>
> - Install ctxd as a background service that auto-starts when you
>   log in (you can opt out with --skip-service).
> - Configure Claude Desktop and Claude Code to use ctxd over MCP.
>   Existing entries in those config files are preserved.
> - Write a paste-ready snippet for Codex (Codex's MCP config is
>   still moving; it requires a manual paste).
> - Mint capability tokens scoped to `/me/**` (one per app), stored
>   as `0600` files. No tokens go in process arguments or app config
>   JSON.
> - Seed three baseline events (`/me/profile`, `/me/preferences`,
>   `/me/about`) so a fresh AI conversation has something to read.
> - Capture a snapshot of your existing config so `ctxd offboard`
>   can fully reverse this setup.
>
> Want to proceed?

Wait for an affirmative. If the user says "what about adapters?" —
tell them adapters (Gmail, GitHub, filesystem) are opt-in and will
be prompted for separately if they enable them. Default is
`--skip-adapters` for the first run.

If the user wants to see what would happen without any mutations,
run `ctxd onboard --dry-run --skill-mode` first and narrate the
plan.

## Step 3 — run onboard, narrate progress

Run:

```bash
ctxd onboard --skill-mode --skip-adapters
```

(If the user opted into adapters, omit `--skip-adapters`.)

The binary emits JSON Lines on stdout. Read each line, parse JSON,
switch on `kind`:

* `step` → render with the step's slug and status. Friendly mapping:
  * `snapshot` → "Saving your current config so we can undo this..."
  * `service-install` → "Installing ctxd as a launchd / systemd service..."
  * `service-start` → "Starting the daemon..."
  * `configure-clients` → "Wiring up Claude Desktop and Claude Code..."
  * `mint-capabilities` → "Minting per-app access tokens..."
  * `seed-subjects` → "Writing baseline memory under /me/**..."
  * `configure-adapters` → "Setting up adapters..."
  * `doctor` → "Checking everything works..."
* `notice` → render prominently. If `action_url` and `action_code`
  are present, this is an OAuth device-flow prompt: tell the user
  exactly what to do ("Open this URL, enter this code, come back
  when you're authorized").
* `log` → optional. Show `info` and above; hide `debug` unless the
  user asked for verbose mode.
* `done` → terminal. The pipeline finished. Render the
  `outcome.doctor` summary and either celebrate (all green) or
  explain what's wrong.
* `error` → terminal failure. Render `message` + `remediation`.
  Offer to re-run after the user fixes whatever was wrong.

Always check `protocol`. If it's not `1`, tell the user the binary
and skill are out of sync — offer to upgrade ctxd or the skill.

## Step 4 — surface adapter prompts (if applicable)

If the user opted into adapters, you'll see one or more `notice`
messages with `id` matching `gmail-oauth`, `github-pat-prompt`, etc.
Each is unique:

* `gmail-oauth` → user must visit `action_url` in their browser,
  enter `action_code`, and approve the scopes ctxd asks for. The
  binary polls for completion automatically. Tell the user "I'm
  waiting for you to finish the OAuth flow — come back when done."
* `github-pat-prompt` → ctxd cannot solicit input directly in v0.4
  skill mode. You need to ask the user for their fine-grained PAT
  ("with read access to repos and issues") and re-invoke ctxd with
  `--github=<token>`. Defer this if they don't have a PAT ready.
* `binary-mismatch` → warn the user that their running ctxd is at a
  non-canonical install path. This isn't fatal but it can confuse
  later operations. Recommend `brew upgrade ctxd` or pointing the
  service at the canonical binary.

## Step 5 — render the doctor summary

When you see `kind: done`:

```
✓ daemon-running    pid 12345, http://127.0.0.1:7777, version 0.4.0
✓ storage-healthy   /Users/me/Library/Application Support/ctxd/ctxd.db
✓ events-present    3 events in the log
✓ service-installed launchd, /Users/me/Library/LaunchAgents/dev.ctxd.daemon.plist
✓ claude-desktop-config /Users/me/Library/Application Support/Claude/claude_desktop_config.json
✓ claude-code-config /Users/me/.claude/settings.json
! codex-config       Codex requires a manual paste — see /Users/me/.../codex.snippet.toml
✓ caps-valid         6/6 cap files verified
↷ adapters           skipped (no adapters enabled)

8/9 ok, 1 warning, 0 failed
```

If anything is `failed`, surface the `remediation` string verbatim
and offer to re-run the relevant `--only` flow.

## Step 6 — demo the value

The whole point of all that setup. Tell the user:

> Setup's done. To prove it worked:
>
> 1. Open Claude Desktop and say: **"Remember that I prefer
>    TypeScript for new projects."**
> 2. Come back here and press enter.

After they press enter, run:

```bash
ctxd query 'FROM e IN events WHERE e.subject LIKE "/me/preferences/%" ORDER BY e.time DESC LIMIT 3'
```

Show the user the resulting events. They should see:

```json
[
  {
    "id": "...",
    "subject": "/me/preferences",
    "type": "ctx.note",
    "data": { "text": "user prefers TypeScript for new projects" },
    "source": "claude-desktop://...",
    "time": "..."
  },
  {
    "id": "...",
    "subject": "/me/preferences",
    "type": "ctx.note",
    "data": { "text": "No preferences captured yet. Tell your AI..." },
    "source": "ctxd://onboard"
  }
]
```

Then tell them: "Open Claude Code and ask 'what do I prefer for new
projects?'. It'll know — same memory, different agent."

If `ctxd query` returns only the seed event (no fresh entry), tell
the user one of:

* Claude Desktop hasn't reloaded its MCP config yet — quit and
  reopen it.
* Claude Desktop didn't actually call `ctx_write` — try a more
  explicit prompt like "Use the ctxd tool to remember that I prefer
  TypeScript."
* The cap file is missing — `ctxd doctor` will tell you which.

## Step 7 — wrap up

Tell the user the three things they care about:

1. **Where the data lives.** `~/Library/Application Support/ctxd/`
   on macOS. They can browse it, back it up, copy it to a new
   machine. Their memory, their disk.
2. **How to reverse this.** `ctxd offboard` to stop the service
   and restore their config. `ctxd offboard --purge` to also
   delete the SQLite DB. Both are idempotent.
3. **Where to see what's happening live.** `ctxd dashboard`
   opens a browser at `http://127.0.0.1:7777` showing recent
   events, subjects, and a live tail.

Done.

---

## What this skill does NOT do

- Does **not** mint capability tokens. Always shells to ctxd.
- Does **not** edit config files directly. Always shells to ctxd.
- Does **not** handle PII / OAuth tokens in its own context.
  Tokens live in `0600` files ctxd manages.
- Does **not** run as a long-lived agent. One-shot setup wizard.

## Troubleshooting from inside the skill

If the user reports that something feels broken at any point, run
`ctxd doctor --json` and surface the failed checks with their
remediation strings. The doctor's check inventory is stable, so you
can map specific failure shapes to specific user-friendly
explanations:

* `daemon-running: failed` → "The daemon isn't running. Try
  `ctxd onboard --only service-start`, or run `ctxd serve` in a
  foreground terminal to see the boot log."
* `service-installed: failed` → "The launchd plist / systemd unit
  is missing. Re-run `ctxd onboard --only service-install`."
* `caps-valid: failed` → "The cap files don't verify against the
  current root key. Re-run `ctxd onboard --only mint-capabilities`."
* `codex-config: warn` → "Codex needs a one-time manual paste. The
  snippet is at the path in the detail field — copy it into
  `~/.codex/config.toml`."

If `ctxd doctor` itself fails to run, ctxd is broken at a level the
skill can't fix. Apologise, point the user at
https://github.com/keeprlabs/ctxd/issues, and capture
`ctxd --version`, the `doctor --json` output (if any), and the
platform / shell.
