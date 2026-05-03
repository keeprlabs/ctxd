# VHS tape for assets/img/terminal.gif (v0.4 redesign).
#
# Pitches the v0.4 onboarding flow: one command sets up persistent
# shared memory across every MCP-aware AI tool on the machine, the
# doctor confirms it, and a baseline /me/** is already populated
# so the next AI conversation starts with non-empty context.
#
# Regenerate with:
#
#   cargo build -p ctxd-cli
#   PATH=./target/debug:$PATH vhs assets/vhs/terminal.tape
#
# Requires: vhs (brew install vhs) + a built ctxd binary on PATH.
#
# Why the env-var sandboxing: ctxd onboard normally writes caps and
# skills.toml to the user's real Application Support / XDG path.
# CTXD_DATA_DIR + CTXD_CONFIG_DIR (added in #24) point ctxd at a
# tempdir so the recording is self-contained and reproducible.

Output assets/img/terminal.gif

# Monochrome theme matched to site/landing.css `.terminal`.
Set Theme '{ "name": "ctxd", "background": "#07090a", "foreground": "#e8e8e8", "cursor": "#ffffff", "selection": "#ffffff33", "black": "#0a0a0a", "red": "#e8e8e8", "green": "#e8e8e8", "yellow": "#cfcfcf", "blue": "#cfcfcf", "magenta": "#e8e8e8", "cyan": "#e8e8e8", "white": "#ffffff", "brightBlack": "#5e5e5e", "brightRed": "#ffffff", "brightGreen": "#ffffff", "brightYellow": "#ffffff", "brightBlue": "#ffffff", "brightMagenta": "#ffffff", "brightCyan": "#ffffff", "brightWhite": "#ffffff" }'
Set FontSize 14
Set Width 1200
Set Height 720
Set Padding 24
Set Framerate 30
Set PlaybackSpeed 1.0
Set Shell "zsh"
Set TypingSpeed 30ms

# Hidden setup. Sandbox the demo to /tmp and override the cap /
# config / data dirs so we don't trample the user's real install.
# Start a foreground-style daemon in the background on non-default
# ports so doctor's daemon-running check passes — without this, the
# closing doctor would surface the "no daemon running" failure mode
# and confuse the marketing read.
Hide
Type "cd /tmp && rm -rf ctxd-demo && mkdir ctxd-demo && cd ctxd-demo"
Enter
Sleep 200ms
Type "export CTXD_DATA_DIR=$PWD CTXD_CONFIG_DIR=$PWD PS1='$ '"
Enter
Sleep 200ms
Type "ctxd serve --db ./ctxd.db --bind 127.0.0.1:18888 --wire-bind 127.0.0.1:18889 >/dev/null 2>&1 &"
Enter
Sleep 1500ms
Type "clear"
Enter
Show

# The pitch as a comment.
Type "# every AI on your machine, one memory."
Enter
Sleep 800ms
Type "# brew install keeprlabs/tap/ctxd && ctxd onboard"
Enter
Sleep 1200ms
Type "clear"
Enter
Sleep 200ms

# Run onboard. --skip-service skips launchd plist install (we have
# a foreground daemon running for the demo); --skip-adapters keeps
# the recording clean (no OAuth flow). The user's real run is just
# `ctxd onboard`.
Type "ctxd onboard --skip-service --skip-adapters --bind 127.0.0.1:18888"
Enter
Sleep 4500ms

# Doctor. With the foreground daemon running, daemon-running passes.
# service-installed shows ! since we skipped install — that's an
# accurate signal the user can see.
Type "ctxd doctor"
Enter
Sleep 3500ms

# Show the seeded /me/about — the welcome event a fresh AI would
# read on its first query.
Type "ctxd read --subject /me/about"
Enter
Sleep 2500ms

# Cleanup background daemon so the recording ends on a clean prompt.
Hide
Type "kill %1 2>/dev/null; wait 2>/dev/null"
Enter
Sleep 500ms
Show
Sleep 600ms
