# VHS tape for assets/img/terminal.gif (v0.4 redesign).
#
# Pitches the v0.4 onboard flow: every step lands clean, then the
# user immediately sees the seeded /me/** the next AI conversation
# will read.
#
# Regenerate with:
#
#   cargo build -p ctxd-cli
#   PATH=./target/debug:$PATH vhs assets/vhs/terminal.tape
#
# Requires: vhs (brew install vhs) + a built ctxd binary on PATH.
#
# Sandboxing: CTXD_DATA_DIR + CTXD_CONFIG_DIR (added in #24) point
# ctxd at a tempdir so the recording is reproducible and doesn't
# touch the recorder's real Application Support / XDG dir.
#
# Cleanliness: --only filters to the steps that don't depend on a
# real launchd plist install. The omitted steps (service-install,
# service-start, configure-adapters, doctor) are valid in production
# but would surface "skip" or "fail" markers tied to the recording
# environment, which would distract from the actual pitch.

Output assets/img/terminal.gif

Set Theme '{ "name": "ctxd", "background": "#07090a", "foreground": "#e8e8e8", "cursor": "#ffffff", "selection": "#ffffff33", "black": "#0a0a0a", "red": "#e8e8e8", "green": "#e8e8e8", "yellow": "#cfcfcf", "blue": "#cfcfcf", "magenta": "#e8e8e8", "cyan": "#e8e8e8", "white": "#ffffff", "brightBlack": "#5e5e5e", "brightRed": "#ffffff", "brightGreen": "#ffffff", "brightYellow": "#ffffff", "brightBlue": "#ffffff", "brightMagenta": "#ffffff", "brightCyan": "#ffffff", "brightWhite": "#ffffff" }'
Set FontSize 14
Set Width 1200
Set Height 720
Set Padding 24
Set Framerate 30
Set PlaybackSpeed 1.0
Set Shell "zsh"
Set TypingSpeed 30ms

# Hidden setup: tempdir + env-var sandbox + custom prompt.
# VHS spawns an interactive zsh which sources ~/.zshrc and resets
# PATH, so the parent's PATH=./target/debug:$PATH env doesn't
# propagate. We capture the repo root before cd'ing and export the
# target/debug dir into the recorded shell's PATH explicitly.
Hide
Type "export CTXD_BIN_DIR=$PWD/target/debug && export PATH=$CTXD_BIN_DIR:$PATH"
Enter
Sleep 200ms
Type "cd /tmp && rm -rf ctxd-demo && mkdir ctxd-demo && cd ctxd-demo"
Enter
Sleep 200ms
Type "export CTXD_DATA_DIR=$PWD CTXD_CONFIG_DIR=$PWD PS1='$ '"
Enter
Sleep 200ms
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

# Run onboard. --only restricts to the steps that succeed without
# actually installing the launchd plist (which the recording host
# can't make permanent). In production users just type `ctxd onboard`.
Type "ctxd onboard --only=snapshot,configure-clients,mint-capabilities,seed-subjects --skip-service --skip-adapters"
Enter
Sleep 4000ms

# Show the seeded /me/about — the welcome event a fresh AI would
# read on its first query. Proof the substrate isn't empty.
Type "ctxd read --subject /me/about"
Enter
Sleep 3000ms
