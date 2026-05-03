# Running the Gmail + GitHub adapters in the background

This page is the practical "I want my mail and my pull requests in ctxd, running 24/7 on my laptop" recipe. It covers macOS (launchd) and Linux (systemd-user), and ends with verification + troubleshooting.

For what each adapter ingests and how to authenticate, see the per-adapter pages first:

- [docs/adapters/gmail.md](gmail.md)
- [docs/adapters/github.md](github.md)

## Status in v0.4.1

The adapter binaries are **complete and tested**, but `ctxd serve` doesn't yet spawn them in-process — `crates/ctxd-cli/src/onboard/adapter_runtime.rs` reads `[gmail]` / `[github]` sections from `skills.toml` and logs *"spawn deferred"*. The auto-spawn lands in a follow-on release. Until then:

- The Homebrew bottle and release tarball ship `ctxd` only.
- You build the adapter binaries from source and run them as **separate** background services pointing at the same SQLite DB the daemon uses.
- The `[gmail]` / `[github]` sections in `skills.toml` are reserved — leave them out for now.

The fs adapter is the exception: it *is* spawned in-process by `ctxd serve` from `skills.toml`. Use `ctxd onboard --fs ~/Documents/notes` for it.

## 1. Build the adapter binaries

From a clone of the repo:

```bash
git clone https://github.com/keeprlabs/ctxd && cd ctxd
cargo build --release \
  -p ctxd-adapter-github \
  -p ctxd-adapter-gmail

# Install alongside the brew-managed ctxd binary so launchd plists
# don't need an absolute path baked to your home directory.
sudo install -m 0755 \
  target/release/ctxd-adapter-github \
  target/release/ctxd-adapter-gmail \
  /opt/homebrew/bin/        # Linux: /usr/local/bin
```

Verify:

```bash
ctxd-adapter-github --version
ctxd-adapter-gmail  --version
```

## 2. Choose where the adapters write

Both adapters take `--db <path>`. Point them at **the same SQLite file `ctxd serve` opens** so events show up in the dashboard, MCP tools, and CLI:

| OS      | Default ctxd DB path |
| ---     | --- |
| macOS   | `~/Library/Application Support/ctxd/ctxd.db` |
| Linux   | `$XDG_DATA_HOME/ctxd/ctxd.db` (typically `~/.local/share/ctxd/ctxd.db`) |
| Windows | `%APPDATA%\ctxd\data\ctxd.db` |

`ctxd doctor` prints the resolved path under `db-path`.

SQLite WAL mode handles three concurrent writers (daemon + two adapters) without coordination. If you see `database is locked`, increase the adapter polling intervals — short intervals on a personal mailbox aren't useful anyway.

## 3. GitHub adapter — PAT + launchd

GitHub authenticates with a Personal Access Token. Fine-grained PAT recommended; see [docs/adapters/github.md §1](github.md#1-personal-access-tokens) for the exact scopes.

```bash
# Sanity-check the binary against your token before daemonizing.
# Let it run for one poll cycle, watch the log, then Ctrl-C.
GITHUB_TOKEN=ghp_xxx ctxd-adapter-github run \
  --db "$HOME/Library/Application Support/ctxd/ctxd.db" \
  --user \
  --poll-interval 5m

# Confirm events landed:
ctxd subjects --prefix /work/github --recursive
```

Once the foreground run looks healthy, install the launchd plist:

```bash
launchctl bootout gui/$UID/dev.ctxd.adapter.github 2>/dev/null  # idempotent
mkdir -p ~/Library/LaunchAgents
cat > ~/Library/LaunchAgents/dev.ctxd.adapter.github.plist <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>Label</key><string>dev.ctxd.adapter.github</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/homebrew/bin/ctxd-adapter-github</string>
    <string>run</string>
    <string>--db</string>
    <string>$HOME/Library/Application Support/ctxd/ctxd.db</string>
    <string>--user</string>
    <string>--poll-interval</string><string>5m</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>GITHUB_TOKEN</key><string>ghp_REPLACE_ME</string>
  </dict>
  <key>KeepAlive</key>
  <dict><key>SuccessfulExit</key><false/></dict>
  <key>RunAtLoad</key><true/>
  <key>StandardErrorPath</key><string>/tmp/ctxd-adapter-github.log</string>
  <key>StandardOutPath</key><string>/tmp/ctxd-adapter-github.log</string>
</dict></plist>
PLIST
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/dev.ctxd.adapter.github.plist
```

Replace `--user` with one or more `--repo owner/name` flags if you'd rather pin a fixed list. Replace `ghp_REPLACE_ME` with your token. The plist runs as your login user, so your `$HOME` resolves the same way `ctxd onboard` does.

`KeepAlive { SuccessfulExit = false }` restarts the adapter if it dies unexpectedly but lets you take it down with `launchctl bootout` cleanly.

## 4. Gmail adapter — OAuth device flow + launchd

The Gmail adapter uses Google's OAuth2 device flow. Create a Desktop OAuth client in Google Cloud (see [docs/adapters/gmail.md](gmail.md) for the exact steps), then authorize once:

```bash
export GOOGLE_CLIENT_ID=...apps.googleusercontent.com
export GOOGLE_CLIENT_SECRET=...
ctxd-adapter-gmail auth
# prints a verification URL + user code; finish in a browser
```

That writes an encrypted refresh token under `~/Library/Application Support/ctxd-adapter-gmail/` (macOS) or `$XDG_STATE_HOME/ctxd-adapter-gmail/` (Linux). `gmail.token.enc` is AES-256-GCM-encrypted under `gmail.key` (mode `0600`).

Then daemonize:

```bash
launchctl bootout gui/$UID/dev.ctxd.adapter.gmail 2>/dev/null
cat > ~/Library/LaunchAgents/dev.ctxd.adapter.gmail.plist <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>Label</key><string>dev.ctxd.adapter.gmail</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/homebrew/bin/ctxd-adapter-gmail</string>
    <string>run</string>
    <string>--db</string>
    <string>$HOME/Library/Application Support/ctxd/ctxd.db</string>
    <string>--poll-interval</string><string>2m</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>GOOGLE_CLIENT_ID</key><string>...apps.googleusercontent.com</string>
    <key>GOOGLE_CLIENT_SECRET</key><string>...</string>
  </dict>
  <key>KeepAlive</key>
  <dict><key>SuccessfulExit</key><false/></dict>
  <key>RunAtLoad</key><true/>
  <key>StandardErrorPath</key><string>/tmp/ctxd-adapter-gmail.log</string>
  <key>StandardOutPath</key><string>/tmp/ctxd-adapter-gmail.log</string>
</dict></plist>
PLIST
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/dev.ctxd.adapter.gmail.plist
```

The same `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` you used for `auth` must be in the env for `run` — the adapter uses them to refresh access tokens.

## 5. Linux — systemd-user units

Equivalent to the launchd plists above. Drop the units in `~/.config/systemd/user/`, then `systemctl --user daemon-reload && systemctl --user enable --now ctxd-adapter-{github,gmail}.service`.

```ini
# ~/.config/systemd/user/ctxd-adapter-github.service
[Unit]
Description=ctxd GitHub adapter
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=GITHUB_TOKEN=ghp_REPLACE_ME
ExecStart=/usr/local/bin/ctxd-adapter-github run \
  --db %h/.local/share/ctxd/ctxd.db \
  --user \
  --poll-interval 5m
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

```ini
# ~/.config/systemd/user/ctxd-adapter-gmail.service
[Unit]
Description=ctxd Gmail adapter
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=GOOGLE_CLIENT_ID=...apps.googleusercontent.com
Environment=GOOGLE_CLIENT_SECRET=...
ExecStart=/usr/local/bin/ctxd-adapter-gmail run \
  --db %h/.local/share/ctxd/ctxd.db \
  --poll-interval 2m
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

`loginctl enable-linger $USER` once if you want the user manager (and thus the adapters) to keep running when you're logged out.

## 6. Verify

```bash
# launchd
launchctl list | grep ctxd
tail -f /tmp/ctxd-adapter-github.log /tmp/ctxd-adapter-gmail.log

# systemd-user
systemctl --user status ctxd-adapter-github ctxd-adapter-gmail
journalctl --user -u ctxd-adapter-github -f

# Adapter-side state (cursors, last poll, rate-limit)
ctxd-adapter-github status
ctxd-adapter-gmail  status

# Events landing in ctxd
ctxd subjects --prefix /work/github   --recursive
ctxd subjects --prefix /work/email/gmail --recursive
ctxd watch /work/github/** --limit 5
```

The dashboard at `http://127.0.0.1:7777/` will surface the same events under their subjects — useful for confirming the daemon and adapters are pointing at the same DB.

## 7. Updates

When you `brew upgrade ctxd`, the adapter binaries don't move with it — they're independent. To upgrade them, rebuild and reinstall:

```bash
cd path/to/ctxd && git pull && \
  cargo build --release -p ctxd-adapter-github -p ctxd-adapter-gmail && \
  sudo install -m 0755 target/release/ctxd-adapter-{github,gmail} /opt/homebrew/bin/

# Restart so the new binary is in use.
launchctl kickstart -k gui/$UID/dev.ctxd.adapter.github
launchctl kickstart -k gui/$UID/dev.ctxd.adapter.gmail
# Linux:
# systemctl --user restart ctxd-adapter-github ctxd-adapter-gmail
```

## 8. Troubleshooting

**`(code: 14) unable to open database file` in adapter logs.** The `--db` path doesn't exist yet — start the daemon first (`ctxd serve` or the `dev.ctxd.daemon` launchd job from `ctxd onboard`) so it creates the file.

**`database is locked` under heavy poll load.** Lengthen `--poll-interval` (`5m` → `15m` for github; `2m` → `5m` for gmail). On a personal mailbox the polling tax dwarfs anything you'd gain from tighter intervals.

**Gmail: `historyId expired` / falls back to full sync.** Expected after a long downtime — Gmail retains history for ~7 days. The adapter does a fresh `messages.list` walk and resumes from the new `historyId`. No action needed.

**Gmail: refresh-token revoked.** If you revoke access in your Google account settings, `run` exits with `oauth: refresh failed`. Re-run `ctxd-adapter-gmail auth` to issue a new refresh token.

**GitHub: `403` on private repos.** The PAT scope is wrong. Fine-grained PATs need *Repository → Read* on each repo you list (or *All repositories* for `--user` mode); see [docs/adapters/github.md §1](github.md#1-personal-access-tokens).

**Adapter writes events but the daemon doesn't see them.** Check `--db` paths match between `ctxd serve` and the adapter. `ctxd doctor` prints the daemon's path; `launchctl print gui/$UID/dev.ctxd.adapter.github` prints the adapter's argv.
