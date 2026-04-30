# VHS tape for assets/img/terminal.gif
#
# Runs the same write -> read -> grant -> serve flow the landing page
# animates, against a real ctxd binary. Regenerate with:
#
#   vhs assets/vhs/terminal.tape
#
# Requires: vhs (brew install vhs) and ctxd on PATH.
#
# Note on quoting: VHS' Type command does not honor backslash-escaped
# quotes inside its string. Shell commands that need both single and
# double quotes (e.g. --data '{"k":"v"}') are emitted by chaining
# multiple Type calls — VHS types them on the same shell line.

Output assets/img/terminal.gif

# Custom monochrome theme matched to the landing page (site/landing.css `.terminal`).
Set Theme '{ "name": "ctxd", "background": "#07090a", "foreground": "#e8e8e8", "cursor": "#ffffff", "selection": "#ffffff33", "black": "#0a0a0a", "red": "#e8e8e8", "green": "#e8e8e8", "yellow": "#cfcfcf", "blue": "#cfcfcf", "magenta": "#e8e8e8", "cyan": "#e8e8e8", "white": "#ffffff", "brightBlack": "#5e5e5e", "brightRed": "#ffffff", "brightGreen": "#ffffff", "brightYellow": "#ffffff", "brightBlue": "#ffffff", "brightMagenta": "#ffffff", "brightCyan": "#ffffff", "brightWhite": "#ffffff" }'
Set FontSize 14
Set Width 1200
Set Height 720
Set Padding 24
Set Framerate 30
Set PlaybackSpeed 1.0
Set Shell "zsh"
Set TypingSpeed 35ms

# Use a fresh sandbox db so the recording is reproducible.
Hide
Type "cd /tmp && rm -rf ctxd-demo && mkdir ctxd-demo && cd ctxd-demo"
Enter
Sleep 200ms
Type "clear"
Enter
Show

# write 1
Type "ctxd write --subject /work/acme/notes/standup --type ctx.note --data "
Type "'"
Type `{"content":"Ship auth Friday"}`
Type "'"
Enter
Sleep 1500ms

# write 2
Type "ctxd write --subject /work/acme/customers/cust-42 --type ctx.crm --data "
Type "'"
Type `{"plan":"enterprise"}`
Type "'"
Enter
Sleep 1500ms

# read recursive
Type "ctxd read --subject /work/acme --recursive"
Enter
Sleep 2200ms

# grant
Type "ctxd grant --subject "
Type `"/work/acme/**"`
Type " --operations "
Type `"read,subjects"`
Enter
Sleep 2000ms

# serve, then Ctrl+C so the loop returns to a fresh prompt
Type "ctxd serve"
Enter
Sleep 3500ms
Ctrl+C
Sleep 600ms
