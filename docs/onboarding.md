# `ctxd onboard` — one-command setup

This is the user-facing guide for `ctxd onboard`. The protocol the
binary speaks to skill front-ends is documented separately at
[`docs/onboard-protocol.md`](./onboard-protocol.md).

## What onboard does

`ctxd onboard` is a one-time setup that turns a fresh ctxd install
into a running, MCP-connected, opinion-having context substrate. In
under two minutes, after one command, every MCP-aware AI tool on
your machine — Claude Desktop, Claude Code, Codex — shares the same
local memory.

The eight steps, in order:

1. **`snapshot`** — capture the current state of any client config
   files we'd modify, so `offboard` can restore them.
2. **`service-install`** — install ctxd as a user-scope service
   (launchd plist on macOS, systemd-user unit on Linux).
3. **`service-start`** — start the service and wait for `/health`.
4. **`configure-clients`** — write `mcpServers.ctxd` entries into
   Claude Desktop / Claude Code; write a paste-ready snippet for
   Codex.
5. **`mint-capabilities`** — mint per-client capability tokens
   scoped to `/me/**`, persist as `0600` cap files. Tokens never
   appear in process arguments or app config JSON.
6. **`seed-subjects`** — populate `/me/profile` (hostname, platform,
   git identity), `/me/preferences` (placeholder), `/me/about`
   (welcome) so a fresh AI conversation starts with non-empty
   context.
7. **`configure-adapters`** — opt-in: walk OAuth flows for Gmail,
   prompt for a GitHub PAT, prompt for filesystem watch paths.
   Default off (`--skip-adapters`).
8. **`doctor`** — run the diagnostic checks and report.

Run `ctxd doctor` anytime to re-verify. Each check carries a
remediation hint when it fails.

## Quickstart

```bash
brew install keeprlabs/tap/ctxd
ctxd onboard
```

That's it. To verify:

```bash
ctxd doctor
```

You should see a green checklist. Any failed check has a
copy-pasteable fix.

## Mode flags

`ctxd onboard` accepts the following flags. They compose freely.

| Flag                | Effect                                                                                                |
|---------------------|-------------------------------------------------------------------------------------------------------|
| `--skill-mode`      | JSON Lines on stdout (the [protocol](./onboard-protocol.md)). Implies `--headless`.                   |
| `--headless`        | No interactive prompts; defaults everywhere. Safe for automation / CI.                                |
| `--dry-run`         | Plan only — emit step messages but make no changes.                                                   |
| `--skip-service`    | Don't install the service. Useful if you'd rather run `ctxd serve` in a terminal tab.                 |
| `--skip-adapters`   | Skip the configure-adapters step entirely.                                                            |
| `--at-login`        | Configure the service to start at user login. Off by default — opt in when you want autostart.       |
| `--strict-scopes`   | Mint narrower capability tokens (read + search only on `/me/**`). Write must be granted later.       |
| `--with-hooks`      | Install Claude Code hooks (SessionStart / UserPromptSubmit / PreCompact / Stop). Default on.         |
| `--only=step1,step2`| Run only the named steps. Comma-separated step slugs (see protocol doc for the inventory).           |
| `--bind 127.0.0.1:7777` | HTTP admin bind. Defaults to `127.0.0.1:7777` (the launchd plist points here too).               |
| `--wire-bind 127.0.0.1:7778` | Wire-protocol bind.                                                                          |

## Reverse the setup

`ctxd offboard` is the explicit reverse. Idempotent.

```bash
ctxd offboard           # restore client configs + stop service.
                        # SQLite DB and adapter tokens stay.

ctxd offboard --purge   # also delete the SQLite DB and HNSW index
                        # sidecars. Cap files and adapter tokens
                        # remain — delete those manually if needed.

ctxd offboard --skip-service  # restore client configs but keep
                              # launchd / systemd service config.
```

How offboard restores: phase 3A captures a JSON snapshot of every
client config file we touched before onboard mutated it. `offboard`
reads the most recent snapshot from
`<data_dir>/onboard-snapshots/` and writes each file back to its
pre-onboard contents (or removes files onboard created where none
existed).

## Where things live

After onboard, your machine has:

* `~/Library/Application Support/ctxd/ctxd.db` (macOS) — the SQLite
  event log.
* `~/Library/Application Support/ctxd/ctxd.db.pid` — the daemon's
  pidfile (created on serve, removed on shutdown).
* `~/Library/Application Support/ctxd/caps/<client>.bk` (mode 0600)
  — per-client capability files.
* `~/Library/Application Support/ctxd/onboard-snapshots/<ts>.json`
  — pre-onboard config snapshots for `offboard` restore.
* `~/Library/LaunchAgents/dev.ctxd.daemon.plist` (macOS) — the
  service unit.
* `~/Library/Logs/ctxd/{stdout,stderr}.log` (macOS) — the daemon's
  log files.
* `~/.claude/settings.json` — Claude Code config with
  `mcpServers.ctxd` plus optional hooks.
* `~/Library/Application Support/Claude/claude_desktop_config.json`
  — Claude Desktop config with `mcpServers.ctxd`.

On Linux, paths shift to XDG conventions
(`$XDG_DATA_HOME/ctxd/...` and
`~/.config/systemd/user/ctxd.service`).

## Manual client config

If you can't / don't want to use `ctxd onboard --skip-service`
configures clients without installing the service, but you can also
hand-edit the client config files directly. The MCP entry shape
that onboard would write:

### Claude Desktop

`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "ctxd": {
      "command": "/opt/homebrew/bin/ctxd",
      "args": [
        "serve", "--mcp-stdio",
        "--cap-file", "/Users/me/Library/Application Support/ctxd/caps/claude-desktop.bk",
        "--db", "/Users/me/Library/Application Support/ctxd/ctxd.db"
      ]
    }
  }
}
```

### Claude Code

`~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "ctxd": {
      "command": "/opt/homebrew/bin/ctxd",
      "args": [
        "serve", "--mcp-stdio",
        "--cap-file", "/Users/me/Library/Application Support/ctxd/caps/claude-code.bk",
        "--db", "/Users/me/Library/Application Support/ctxd/ctxd.db"
      ]
    }
  }
}
```

### Codex

Codex's MCP config story is still evolving in v0.4. `ctxd onboard`
writes a paste-ready TOML snippet at
`<config_dir>/ctxd/codex.snippet.toml`; copy its contents into your
Codex config.

## Troubleshooting

`ctxd doctor` is the canonical entry point. Each failed check
includes a remediation string.

```bash
ctxd doctor
ctxd doctor --json   # for scripts / CI
```

Common issues:

* **`daemon-running: failed`** — service unit installed but daemon
  isn't running. Try `ctxd onboard --only service-start`.
* **`port 7777 already in use`** — another ctxd is already running.
  Likely a brew binary or a `cargo run` you forgot. The error
  message includes the running daemon's PID, version, and admin
  URL; stop it (`kill $pid`) and retry.
* **`caps-valid: failed`** — the cap files don't verify against the
  current DB's root key. This usually means you ran onboard against
  one DB and doctor against another. Re-run onboard against the
  correct `--db`.
* **`codex-config: warn`** — Codex requires a manual paste. The
  snippet file's path is in the check's detail.

## Protocol contract

The skill at `skill/ctxd-memory/SKILL.md` shells to
`ctxd onboard --skill-mode` and parses the output as JSON Lines.
The contract is documented in
[`docs/onboard-protocol.md`](./onboard-protocol.md). Bumps to
`PROTOCOL_VERSION` are breaking changes; additive fields are not.

## Windows

Not yet supported. v0.4 ships macOS + Linux. For Windows, run `ctxd
serve` in a terminal tab and configure clients by hand using the
snippets above.
