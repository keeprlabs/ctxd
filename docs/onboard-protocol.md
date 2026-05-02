# `ctxd onboard --skill-mode` JSON protocol

This document is the contract between the `ctxd` binary and any
external front door — the in-tree Claude Code skill at
`skill/ctxd-memory/`, future IDE plugins, the `ctxd.dev/install` web
installer, anything else that wants to drive `ctxd onboard`
programmatically.

The skill team writes against this document. The binary team
implements against this document. Changes to either side that don't
match what is written here are bugs.

## Wire format

Newline-delimited JSON ("JSON Lines"): each message is one line on
stdout, terminated by `\n`. Logs and tracing remain on stderr — the
skill can capture stdout cleanly without filtering. UTF-8 throughout.

Every message is an object with a `kind` field that names the variant
and a `protocol` field that names the schema version. The remaining
fields depend on the variant.

```json
{"kind":"step","protocol":1,"step":"service-install","status":"started"}
{"kind":"step","protocol":1,"step":"service-install","status":"ok","detail":{"platform":"macos","plist_path":"/Users/me/Library/LaunchAgents/com.keeprlabs.ctxd.plist"}}
{"kind":"notice","protocol":1,"id":"gmail-oauth","message":"Visit the URL and enter the code.","action_url":"https://accounts.google.com/o/oauth2/device","action_code":"ABCD-EFGH","expires_at":"2026-05-01T22:14:00Z"}
{"kind":"done","protocol":1,"outcome":{"onboarded":true,"clients_configured":["claude-desktop","claude-code"],"adapters_enabled":["fs"],"doctor":{"total":9,"ok":9,"warnings":0,"failed":0}}}
```

## Versioning

`protocol` is a non-negative integer. **`PROTOCOL_VERSION = 1`** for
v0.4.

* Adding a new message variant or a new optional field is **not**
  breaking and does not require a version bump. Skills written
  against an older binary will see the new fields and ignore them.
* Renaming or removing fields, changing field semantics, or replacing
  a variant **is** breaking and requires bumping `protocol`.

When a skill sees a message whose `protocol` does not match the
version it was built against, it MUST refuse to interpret the message
and surface an "update required" error. Don't try to muddle through —
mismatched protocols silently misbehave in ways that look like ctxd
bugs.

## Message variants

### `step` — pipeline transition

The most common variant. Emitted as the orchestrator moves through
the [seven steps][steps] (plus `snapshot` and `doctor`).

```jsonc
{
  "kind": "step",
  "protocol": 1,
  "step": "service-install",       // see "Step names" below
  "status": "ok",                  // see "Statuses" below
  "detail": { "...": "..." }       // step-specific structured data
                                   // (omitted when null)
}
```

#### Step names

In the order they fire on a full run:

| Slug | Description |
|------|-------------|
| `snapshot` | Pre-flight scan + state snapshot for offboard restore. |
| `service-install` | Install user-scope service (launchd / systemd-user). |
| `service-start` | Start the service; wait for `/health`. |
| `configure-clients` | Write MCP entries for Claude Desktop, Claude Code, Codex. |
| `mint-capabilities` | Mint per-client tokens, persist as `0600` cap files. |
| `seed-subjects` | Write `/me/profile`, `/me/preferences`, `/me/about`. |
| `configure-adapters` | Walk OAuth / PAT flows for opt-in adapters. |
| `doctor` | Run diagnostic checks, report. |

#### Statuses

| Status | Meaning |
|--------|---------|
| `started` | Step is beginning. Emitted once per step. |
| `ok` | Step completed successfully. |
| `skipped` | User declined, step already complete, or `--skip-*` flag set. |
| `manual-pending` | Step partially completed. The user must do something out-of-band (see `detail.instructions`). The next `ctxd doctor` run promotes the status to `ok` once the user finishes. |
| `failed` | Step failed. The pipeline stops; an `error` message follows. |

#### Detail fields per step

`detail` is variant-by-step. The fields below are the contract; the
binary may add more (additive, non-breaking).

* `snapshot.ok.detail`:
  ```jsonc
  {
    "snapshot_path": "/Users/me/Library/Application Support/ctxd/onboard-snapshots/2026-05-01T22-04-13Z.json",
    "running_daemons": [{"pid": 66815, "binary": "/opt/homebrew/bin/ctxd"}],
    "existing_clients": ["claude-desktop"]
  }
  ```
* `service-install.ok.detail`:
  ```jsonc
  { "platform": "macos", "service_name": "com.keeprlabs.ctxd",
    "binary_path": "/opt/homebrew/bin/ctxd",
    "plist_path": "/Users/me/Library/LaunchAgents/com.keeprlabs.ctxd.plist" }
  ```
* `service-start.ok.detail`:
  ```jsonc
  { "http_url": "http://127.0.0.1:7777", "wire_url": "tcp://127.0.0.1:7778",
    "version": "0.4.0", "uptime_ms": 142 }
  ```
* `configure-clients.ok.detail`:
  ```jsonc
  {
    "clients": {
      "claude-desktop": "configured",
      "claude-code": "configured",
      "codex": "manual-pending"
    },
    "config_paths": ["/Users/me/Library/Application Support/Claude/claude_desktop_config.json"]
  }
  ```
* `mint-capabilities.ok.detail`:
  ```jsonc
  { "tokens_minted": 4, "stored_at": "/Users/me/Library/Application Support/ctxd/caps/" }
  ```
* `seed-subjects.ok.detail`:
  ```jsonc
  { "subjects_created": ["/me/profile", "/me/preferences", "/me/about"], "events_written": 3 }
  ```
* `configure-adapters.ok.detail`:
  ```jsonc
  { "adapters": { "gmail": "authenticated", "github": "skipped", "fs": "watching ~/Documents/notes" } }
  ```
* `doctor.ok.detail`:
  ```jsonc
  { "checks": [
      {"name": "daemon-running", "status": "ok"},
      {"name": "claude-desktop-config", "status": "ok"},
      {"name": "gmail-adapter", "status": "ok", "last_sync": "2026-05-01T22:13:00Z"}
  ]}
  ```

`*.skipped.detail`:
```jsonc
{ "reason": "human-readable explanation" }
```

`*.manual-pending.detail`:
```jsonc
{ "instructions": "Paste this into ~/.codex/config.toml under [mcp_servers.ctxd]:\n  command = \"/opt/homebrew/bin/ctxd\"\n  args = [\"serve\", \"--mcp-stdio\", \"--cap-file\", \"/Users/me/Library/Application Support/ctxd/caps/codex.bk\"]" }
```

### `notice` — out-of-band user-visible message

A user-visible message that does not expect a response. The skill
renders this directly; ctxd continues without waiting. Used for
OAuth device-flow prompts, ambient warnings, etc.

```jsonc
{
  "kind": "notice",
  "protocol": 1,
  "id": "gmail-oauth",            // stable identifier the skill can switch on
  "message": "Visit the URL and enter the code in your browser.",
  "action_url": "https://accounts.google.com/o/oauth2/device",
  "action_code": "ABCD-EFGH",     // omitted if not applicable
  "expires_at": "2026-05-01T22:14:00Z"  // RFC3339; omitted if not applicable
}
```

The skill is expected to map `id` to a localised UI presentation. The
contract today defines:

| `id` | Trigger |
|------|---------|
| `gmail-oauth` | Gmail OAuth device-flow code ready to display. |
| `github-pat-prompt` | Reminder that the user must paste a PAT (the binary cannot solicit input in v0.4 skill mode; the skill collects it and re-invokes ctxd with `--github-pat=<token>`). |
| `binary-mismatch` | The running ctxd is not at the canonical install path. Onboarding may produce surprising results. |

### `log` — diagnostic line

Best-effort debug/info/warn messages. Skills typically render `info`
and above; `debug` is for skill developers chasing problems.

```jsonc
{ "kind": "log", "protocol": 1, "level": "info", "message": "writing /Users/me/Library/LaunchAgents/com.keeprlabs.ctxd.plist" }
```

`level` is one of `debug`, `info`, `warn`, `error`.

### `done` — terminal success

Pipeline finished, with the summary attached. After this message the
process exits 0.

```jsonc
{
  "kind": "done",
  "protocol": 1,
  "outcome": {
    "onboarded": true,                                    // false if any step ended manual-pending or failed
    "clients_configured": ["claude-desktop", "claude-code"],
    "adapters_enabled": ["fs"],
    "doctor": { "total": 9, "ok": 9, "warnings": 0, "failed": 0 }
  }
}
```

### `error` — terminal failure

The pipeline aborted at `step` with `message`. After this message the
process exits non-zero. `remediation` is a one-line hint (often a
`ctxd` command) the skill can show the user.

```jsonc
{
  "kind": "error",
  "protocol": 1,
  "step": "service-start",
  "message": "port 7777 is already in use",
  "remediation": "Stop the existing daemon: kill $(cat ~/Library/Application\\ Support/ctxd/ctxd.pid)"
}
```

## Communication direction

In v0.4 the protocol is **strictly one-way**: ctxd writes JSON Lines,
the skill reads them. There is no stdin response channel.

User choices that need to influence the run are passed as CLI flags
on the `ctxd onboard` invocation:

| Flag | Effect |
|------|--------|
| `--skill-mode` | Selects this protocol (instead of human output). |
| `--headless` | Run with all defaults; never produce a `notice` that needs user attention. |
| `--dry-run` | Plan only — emit `step` messages but make no changes. |
| `--skip-service` | Foreground-only; do not install a service. |
| `--skip-adapters` | Skip the `configure-adapters` step entirely. |
| `--strict-scopes` | Mint narrower capability tokens. |
| `--gmail=<value>` | One of `interactive` (default — start OAuth and emit `notice`), `skip`, or a refresh-token literal. |
| `--github=<value>` | One of `skip` or a PAT literal. |
| `--fs=<paths>` | Comma-separated absolute paths to watch. Empty / omitted = skip. |
| `--at-login` | Configure the service to start at login (default off). |
| `--with-hooks` | Write Claude Code auto-invocation hooks (default on for the `claude-code` client). |
| `--only=<steps>` | Comma-separated step slugs; run only those. |

If a future version of the protocol needs a real bidirectional
channel (e.g. for prompts that can't be pre-answered), it will land
behind a version bump and a separate transport (probably a stdin
JSON-Lines response stream paired with an explicit pairing handshake).

## Stability guarantees

For the lifetime of `protocol: 1` we guarantee:

* Every existing variant continues to be emitted with the same
  semantics. New variants may be added.
* Every existing field continues to carry the documented type and
  meaning. New fields may be added (skills MUST tolerate unknown
  fields).
* Step slugs and statuses do not change. New steps may be added at
  the end of the pipeline; new statuses may be added.
* `expires_at` and other timestamp fields are RFC 3339 in UTC.
* Empty optional fields are omitted from serialisation rather than
  emitted as `null` — this lets the skill use truthy checks reliably.

[steps]: ./onboarding.md  "User-facing onboarding guide (Phase 4C)"
