# VHS tape for assets/img/dashboard.gif (v0.4 redesign).
#
# Pitches the dashboard's headline: a daemon you can *see into*.
# Seeds varied subjects, queries the same admin endpoints the
# browser UI calls, then runs `ctxd watch` to catch a fresh write
# in real time.
#
# The browser UI itself is captured separately as static PNGs in
# assets/img/dashboard-{view}.png (Chrome headless screenshots).
# This gif covers the API surface those views are built on.
#
# Regenerate with:
#
#   cargo build -p ctxd-cli
#   PATH=./target/debug:$PATH vhs assets/vhs/dashboard.tape

Output assets/img/dashboard.gif

Set Theme '{ "name": "ctxd", "background": "#07090a", "foreground": "#e8e8e8", "cursor": "#ffffff", "selection": "#ffffff33", "black": "#0a0a0a", "red": "#e8e8e8", "green": "#e8e8e8", "yellow": "#cfcfcf", "blue": "#cfcfcf", "magenta": "#e8e8e8", "cyan": "#e8e8e8", "white": "#ffffff", "brightBlack": "#5e5e5e", "brightRed": "#ffffff", "brightGreen": "#ffffff", "brightYellow": "#ffffff", "brightBlue": "#ffffff", "brightMagenta": "#ffffff", "brightCyan": "#ffffff", "brightWhite": "#ffffff" }'
Set FontSize 14
Set Width 1200
Set Height 720
Set Padding 24
Set Framerate 30
Set PlaybackSpeed 1.0
Set Shell "zsh"
Set TypingSpeed 30ms

# Hidden setup: sandbox via env vars, start a foreground daemon on
# non-default ports so it doesn't collide with anything the
# recorder happens to have running on :7777.
Hide
Type "cd /tmp && rm -rf ctxd-dash-demo && mkdir ctxd-dash-demo && cd ctxd-dash-demo"
Enter
Sleep 200ms
Type "export CTXD_DATA_DIR=$PWD CTXD_CONFIG_DIR=$PWD PS1='$ '"
Enter
Sleep 200ms
Type "ctxd serve --db ./ctxd.db --bind 127.0.0.1:18888 --wire-bind 127.0.0.1:18889 >/dev/null 2>&1 &"
Enter
Sleep 1500ms
# Seed the /me/** baseline (without the noisy onboard pipeline output).
Type "ctxd --db ./ctxd.db onboard --only=seed-subjects --skip-service --skip-adapters >/dev/null 2>&1"
Enter
Sleep 1500ms
# Three more events covering the subject hierarchy.
Type "ctxd --db ./ctxd.db write --subject /work/notes/standup --type ctx.note --data "
Type "'"
Type `{"content":"Ship auth Friday"}`
Type "'"
Type " >/dev/null"
Enter
Sleep 200ms
Type "ctxd --db ./ctxd.db write --subject /work/customers/cust-42 --type ctx.crm --data "
Type "'"
Type `{"plan":"enterprise"}`
Type "'"
Type " >/dev/null"
Enter
Sleep 200ms
Type "ctxd --db ./ctxd.db write --subject /coding/sessions/01 --type session.start --data "
Type "'"
Type `{"editor":"claude-code"}`
Type "'"
Type " >/dev/null"
Enter
Sleep 200ms
Type "clear"
Enter
Show

# Pitch comment.
Type "# the dashboard: see what your AI is writing, in real time."
Enter
Sleep 800ms
Type "clear"
Enter
Sleep 200ms

# 1. Stats — proof the daemon is alive and has events.
Type "curl -s -H 'Host: 127.0.0.1:18888' http://127.0.0.1:18888/v1/stats | python3 -m json.tool"
Enter
Sleep 2200ms

# 2. Subject tree — the navigable hierarchy.
Type "curl -s -H 'Host: 127.0.0.1:18888' 'http://127.0.0.1:18888/v1/subjects/tree?prefix=/' | python3 -m json.tool | head -25"
Enter
Sleep 2800ms

# 3. The headline: live tail. Start `ctxd watch` in the background,
# then fire a write. The matching event appears on stdout.
Type "ctxd --db ./ctxd.db watch --url http://127.0.0.1:18888 --timeout-s 8 --limit 1 &"
Enter
Sleep 800ms
Type "ctxd --db ./ctxd.db write --subject /me/preferences --type ctx.note --data "
Type "'"
Type `{"text":"prefer TypeScript"}`
Type "'"
Type " >/dev/null"
Enter
Sleep 1500ms
Type "wait"
Enter
Sleep 1500ms

# Cleanup: kill background daemon + watcher.
Hide
Type "kill %1 %2 2>/dev/null; wait 2>/dev/null"
Enter
Sleep 500ms
Show
Sleep 500ms
