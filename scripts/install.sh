#!/usr/bin/env sh
# ctxd installer.
#
# Usage:
#   curl -fsSL https://github.com/keeprlabs/ctxd/releases/latest/download/install.sh | sh
#   curl -fsSL https://github.com/keeprlabs/ctxd/releases/download/v0.3.0/install.sh | sh
#
# What it does:
#   1. Detects your OS + architecture.
#   2. Downloads the matching release tarball from GitHub.
#   3. Verifies the published sha256.
#   4. Extracts the `ctxd` binary into a directory on your $PATH
#      (preferring an existing directory you can write to, falling
#      back to ~/.local/bin and asking you to add it to PATH).
#
# Override the install location with CTXD_INSTALL_DIR, e.g.:
#   curl -fsSL .../install.sh | CTXD_INSTALL_DIR=/usr/local/bin sh
#
# This script is rendered with a pinned version baked in at release
# time; the placeholder below is replaced by the release workflow.
# A copy without the pinned version is also published as the "latest"
# install.sh — that variant resolves the latest tag at run-time.

set -eu

REPO="keeprlabs/ctxd"
# `PINNED_VERSION` is rewritten by the release workflow so a per-tag
# install.sh ships pinned to that exact version. The "latest" copy in
# the repo keeps the UNPINNED sentinel and resolves the version from
# the GitHub /releases/latest redirect at run time.
PINNED_VERSION="UNPINNED"

err() { printf 'ctxd-install: %s\n' "$*" >&2; exit 1; }
info() { printf 'ctxd-install: %s\n' "$*"; }

# ── version resolution ─────────────────────────────────────────────────

resolve_version() {
  if [ "$PINNED_VERSION" != "UNPINNED" ]; then
    printf '%s' "$PINNED_VERSION"
    return
  fi
  # Latest tag from the GitHub redirect — no auth, no jq.
  if command -v curl >/dev/null 2>&1; then
    curl -fsSLI -o /dev/null -w '%{url_effective}' \
      "https://github.com/${REPO}/releases/latest" \
      | sed -E 's|.*/tag/v?([^/]+)$|\1|'
  elif command -v wget >/dev/null 2>&1; then
    wget --max-redirect=10 --server-response -q -O /dev/null \
      "https://github.com/${REPO}/releases/latest" 2>&1 \
      | awk '/Location:/ {url=$2} END {sub(/.*\/tag\/v?/,"",url); print url}'
  else
    err "need curl or wget to resolve latest version"
  fi
}

# ── platform detection ─────────────────────────────────────────────────

detect_target() {
  uname_s="$(uname -s)"
  uname_m="$(uname -m)"
  case "$uname_s" in
    Linux) os="unknown-linux-gnu" ;;
    Darwin) os="apple-darwin" ;;
    *) err "unsupported OS: $uname_s (only Linux + macOS for now)" ;;
  esac
  case "$uname_m" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *) err "unsupported architecture: $uname_m" ;;
  esac
  printf '%s-%s' "$arch" "$os"
}

# ── install location ───────────────────────────────────────────────────

# Pick the first directory we can actually write to. Honour explicit
# CTXD_INSTALL_DIR when set. We deliberately avoid sudo: an installer
# that silently escalates is a bad neighbour.
pick_install_dir() {
  if [ -n "${CTXD_INSTALL_DIR:-}" ]; then
    printf '%s' "$CTXD_INSTALL_DIR"
    return
  fi
  for d in "$HOME/.local/bin" "$HOME/.cargo/bin" "/usr/local/bin"; do
    if [ -d "$d" ] && [ -w "$d" ]; then
      printf '%s' "$d"
      return
    fi
  done
  # Fall back to creating ~/.local/bin.
  mkdir -p "$HOME/.local/bin"
  printf '%s' "$HOME/.local/bin"
}

# ── download + verify ──────────────────────────────────────────────────

fetch() {
  url="$1"
  out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto '=https' --tlsv1.2 -o "$out" "$url"
  else
    wget --https-only -qO "$out" "$url"
  fi
}

verify_sha() {
  archive="$1"
  expected="$2"
  if command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$archive" | awk '{print $1}')"
  elif command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$archive" | awk '{print $1}')"
  else
    err "neither shasum nor sha256sum found — cannot verify download"
  fi
  if [ "$actual" != "$expected" ]; then
    err "sha256 mismatch: expected $expected, got $actual"
  fi
}

# ── main ───────────────────────────────────────────────────────────────

main() {
  version="$(resolve_version)"
  [ -n "$version" ] || err "could not resolve version"
  target="$(detect_target)"
  install_dir="$(pick_install_dir)"

  info "version:     v${version}"
  info "target:      ${target}"
  info "install_dir: ${install_dir}"

  base="https://github.com/${REPO}/releases/download/v${version}"
  archive_name="ctxd-${version}-${target}.tar.gz"
  archive_url="${base}/${archive_name}"
  sha_url="${base}/${archive_name}.sha256"

  tmp="$(mktemp -d 2>/dev/null || mktemp -d -t ctxd-install)"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  info "downloading ${archive_url}"
  fetch "$archive_url" "$tmp/$archive_name"
  fetch "$sha_url" "$tmp/$archive_name.sha256"

  expected_sha="$(awk '{print $1}' "$tmp/$archive_name.sha256")"
  verify_sha "$tmp/$archive_name" "$expected_sha"
  info "checksum verified"

  tar -xzf "$tmp/$archive_name" -C "$tmp"
  binary_path="$tmp/ctxd-${version}-${target}/ctxd"
  [ -f "$binary_path" ] || err "binary not found in archive at expected path"

  install -m 0755 "$binary_path" "$install_dir/ctxd"
  info "installed: $install_dir/ctxd"

  case ":$PATH:" in
    *":$install_dir:"*) ;;
    *)
      info ""
      info "note: $install_dir is not on your \$PATH."
      info "      add this to your shell profile to fix:"
      info "        export PATH=\"$install_dir:\$PATH\""
      ;;
  esac

  info ""
  info "verify with:  ctxd --version"
  info "get started:  ctxd serve"
}

main "$@"
