# ctxd-memory

A Claude Code skill that walks the user through a one-time setup
for **persistent, cross-tool AI memory** powered by
[ctxd](https://github.com/keeprlabs/ctxd).

After running this skill, anything the user tells **Claude Desktop**,
**Claude Code**, or **Codex** lands in a single shared context graph
on their local machine. Their memory is owned by them, never sent to
a third party they didn't authorize, and survives across tools and
sessions.

## What the skill does

Runs `ctxd onboard --skill-mode` and narrates the JSON-Lines
protocol. The binary handles all real orchestration — installing the
service, configuring MCP entries, minting capability tokens, seeding
baseline events, capturing a snapshot for offboard. The skill is the
friendly front door.

See [`SKILL.md`](./SKILL.md) for the full conversational flow.

## Install

This skill ships in the [ctxd repo](https://github.com/keeprlabs/ctxd).
Install:

```bash
# From the ctxd repo, copy the skill to Claude Code's skills dir:
cp -R skill/ctxd-memory ~/.claude/skills/

# Or via Anthropic's skill marketplace (when available):
claude-code skills add keeprlabs/ctxd-memory
```

After installation, invoke it from Claude Code:

```
/ctxd-memory
```

## Requirements

* `ctxd` v0.4.0 or later on `$PATH`. The skill will offer to
  install via Homebrew (macOS) or the official install script
  (Linux) if missing.
* macOS or Linux. Windows support lands in v0.5.

## Reverse this setup

```bash
ctxd offboard           # restore client configs + stop service
ctxd offboard --purge   # also delete the SQLite DB
```

## License

Apache-2.0. Same as the ctxd substrate.
