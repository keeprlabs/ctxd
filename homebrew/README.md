# Homebrew formula for ctxd

This directory holds the Homebrew formula template for `ctxd`. The
release workflow renders it with the new version + per-platform
sha256s and pushes the result to
[`keeprlabs/homebrew-tap`](https://github.com/keeprlabs/homebrew-tap)
as `Formula/ctxd.rb`.

## Install

```bash
brew install keeprlabs/tap/ctxd
```

That installs the `ctxd` binary to `/opt/homebrew/bin/ctxd` (Apple
Silicon) or `/usr/local/bin/ctxd` (Intel Mac, Linuxbrew).

## Update

```bash
brew upgrade ctxd
```

## Uninstall

```bash
brew uninstall ctxd
```

## Why a formula and not a cask?

`ctxd` is a single CLI binary, not a `.app` bundle, so it ships as a
formula. Casks are for GUI applications.

## Release-time mechanics

`homebrew/ctxd.rb.tmpl` is the template. The release workflow does:

1. Builds tarballs for the four supported targets (macOS arm64 +
   x86_64, Linux x86_64 + aarch64).
2. Uploads each tarball plus its `.sha256` sibling to the GitHub
   release for the tag.
3. Substitutes `__VERSION__` and `__SHA_*__` placeholders into the
   template.
4. Clones `keeprlabs/homebrew-tap` over SSH using the `TAP_DEPLOY_KEY`
   secret, copies the rendered file to `Formula/ctxd.rb`, commits,
   and pushes.

## Setup (one-time, already done)

For posterity:

1. The `keeprlabs/homebrew-tap` repo has a deploy key with write
   access named `ctxd-tap-deploy`.
2. The matching private key is stored in this repo's secrets as
   `TAP_DEPLOY_KEY`.
3. The release workflow's `update-homebrew` job uses that key to
   push.

If `TAP_DEPLOY_KEY` is missing, the job logs a warning and exits
cleanly — releases still publish, the tap just doesn't update.
