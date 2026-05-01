# VHS tape for assets/img/dashboard.gif
#
# Demonstrates `ctxd dashboard` end-to-end: launch the embedded UI,
# seed an event, query each view via curl. The actual browser UI is
# captured separately as static screenshots in docs/dashboard.md.
#
# Regenerate with:
#
#   vhs assets/vhs/dashboard.tape
#
# Requires: vhs (brew install vhs) and ctxd on PATH.

Output assets/img/dashboard.gif

# Match the terminal-tape monochrome theme so the two GIFs visually
# rhyme on the README.
Set Theme '{ "name": "ctxd", "background": "#07090a", "foreground": "#e8e8e8", "cursor": "#ffffff", "selection": "#ffffff33", "black": "#0a0a0a", "red": "#e8e8e8", "green": "#e8e8e8", "yellow": "#cfcfcf", "blue": "#cfcfcf", "magenta": "#e8e8e8", "cyan": "#e8e8e8", "white": "#ffffff", "brightBlack": "#5e5e5e", "brightRed": "#ffffff", "brightGreen": "#ffffff", "brightYellow": "#ffffff", "brightBlue": "#ffffff", "brightMagenta": "#ffffff", "brightCyan": "#ffffff", "brightWhite": "#ffffff" }'
Set FontSize 14
Set Width 1200
Set Height 720
Set Padding 24
Set Framerate 30
Set PlaybackSpeed 1.0
Set Shell "zsh"
Set TypingSpeed 35ms

# Sandbox dir + db so the recording is reproducible.
Hide
Type "cd /tmp && rm -rf ctxd-dash-demo && mkdir ctxd-dash-demo && cd ctxd-dash-demo"
Enter
Sleep 200ms
Type "clear"
Enter
Show

# Launch the dashboard in the background. --no-open keeps the
# recording terminal-only; the actual browser UI is shown in static
# screenshots elsewhere in docs.
Type "ctxd --db ctxd.db dashboard --no-open &"
Enter
Sleep 1500ms
Type "clear"
Enter
Sleep 200ms

# Seed two events through the loopback hello-world endpoint.
Type "curl -s -X POST http://127.0.0.1:7777/v1/dashboard/hello-world | head -c 80"
Enter
Sleep 1500ms
Type "echo"
Enter

Type "curl -s http://127.0.0.1:7777/v1/stats | python3 -m json.tool"
Enter
Sleep 2000ms

# Show the cursor-paginated events list.
Type "curl -s 'http://127.0.0.1:7777/v1/events?limit=2' | python3 -m json.tool"
Enter
Sleep 2500ms

# Search.
Type "curl -s 'http://127.0.0.1:7777/v1/search?q=hello' | python3 -c "
Type "'"
Type 'import sys,json; d=json.load(sys.stdin); print(d["took_ms"],"ms,",len(d["results"]),"hits")'
Type "'"
Enter
Sleep 1500ms

# Subject tree.
Type "curl -s 'http://127.0.0.1:7777/v1/subjects/tree?prefix=/' | python3 -m json.tool"
Enter
Sleep 2500ms

# Tear down.
Hide
Type "kill %1 2>/dev/null; wait 2>/dev/null"
Enter
Sleep 600ms
