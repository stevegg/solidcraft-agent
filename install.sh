#!/usr/bin/env bash
# Install solidcraft-agent (and optionally its skill for OpenCode).
#
#   curl -fsSL https://raw.githubusercontent.com/stevegg/solidcraft-agent/main/install.sh | bash
#
# Downloads the prebuilt binary for this machine from the latest GitHub Release, checks its SHA-256
# and puts it in ~/.local/bin. When there is no binary for this platform (or with
# SOLIDCRAFT_AGENT_FROM_SOURCE=1) it builds from source with the Rust toolchain SolidCraft pins.
#
# Options (after `bash -s --` when piped from curl):
#   --opencode    also install the skill for OpenCode (~/.config/opencode/skills/solidcraft-agent)
#   --skill-only  install only the skill(s), not the binary
#
# Environment:
#   SOLIDCRAFT_AGENT_VERSION      a release tag such as v0.1.0 (default: the latest release)
#   SOLIDCRAFT_AGENT_INSTALL_DIR  where the binary goes (default: ~/.local/bin)
#   SOLIDCRAFT_AGENT_FROM_SOURCE  1 to skip the download and build from source
set -euo pipefail

REPO="stevegg/solidcraft-agent"
VERSION="${SOLIDCRAFT_AGENT_VERSION:-latest}"
INSTALL_DIR="${SOLIDCRAFT_AGENT_INSTALL_DIR:-$HOME/.local/bin}"
TOOLCHAIN="1.99.1"   # keep in step with rust-toolchain.toml

OPENCODE=0
SKILL_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --opencode) OPENCODE=1 ;;
    --skill-only) SKILL_ONLY=1 ;;
    -h|--help) sed -n '2,20p' "$0" 2>/dev/null || true; exit 0 ;;
    *) printf 'solidcraft-agent: unknown option %s\n' "$arg" >&2; exit 2 ;;
  esac
done

say() { printf '\033[1msolidcraft-agent:\033[0m %s\n' "$*"; }
die() { say "error: $*" >&2; exit 1; }

target() {
  local os arch
  os="$(uname -s)"; arch="$(uname -m)"
  case "$arch" in
    arm64|aarch64) arch=aarch64 ;;
    x86_64|amd64) arch=x86_64 ;;
    *) return 1 ;;
  esac
  case "$os" in
    Darwin) echo "$arch-apple-darwin" ;;
    Linux) echo "$arch-unknown-linux-gnu" ;;
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

install_binary() {
  local t="$1" base tmp name
  if [ "$VERSION" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi
  name="solidcraft-agent-$t.tar.gz"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  say "downloading $name ($VERSION release)"
  curl -fsSL "$base/$name" -o "$tmp/$name" || return 1
  if curl -fsSL "$base/$name.sha256" -o "$tmp/$name.sha256"; then
    [ "$(sha256 "$tmp/$name")" = "$(cut -d' ' -f1 < "$tmp/$name.sha256")" ] || die "checksum mismatch for $name"
  else
    say "warning: no checksum file published for $name; skipping verification"
  fi
  tar -xzf "$tmp/$name" -C "$tmp"
  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$tmp/solidcraft-agent" "$INSTALL_DIR/solidcraft-agent"
}

install_from_source() {
  if ! command -v rustup >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/rustup" ]; then
      export PATH="$HOME/.cargo/bin:$PATH"
    else
      say "installing rustup (the Rust toolchain installer)"
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
      export PATH="$HOME/.cargo/bin:$PATH"
    fi
  fi
  say "installing Rust $TOOLCHAIN (SolidCraft's pinned toolchain)"
  rustup toolchain install "$TOOLCHAIN" --profile minimal
  local sel=(--branch main)
  [ "$VERSION" != latest ] && sel=(--tag "$VERSION")
  say "building from source; the first build compiles SolidCraft's engine and takes several minutes"
  cargo "+$TOOLCHAIN" install --locked --force --git "https://github.com/$REPO" "${sel[@]}" solidcraft-agent
  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$HOME/.cargo/bin/solidcraft-agent" "$INSTALL_DIR/solidcraft-agent"
}

install_opencode_skill() {
  local ref=main dir="${XDG_CONFIG_HOME:-$HOME/.config}/opencode/skills/solidcraft-agent"
  [ "$VERSION" != latest ] && ref="$VERSION"
  mkdir -p "$dir"
  curl -fsSL "https://raw.githubusercontent.com/$REPO/$ref/plugin/skills/solidcraft-agent/SKILL.md" -o "$dir/SKILL.md.tmp" \
    || die "could not download the skill"
  mv "$dir/SKILL.md.tmp" "$dir/SKILL.md"
  say "installed the OpenCode skill at $dir/SKILL.md"
}

if [ "$OPENCODE" = 1 ]; then
  install_opencode_skill
fi
if [ "$SKILL_ONLY" = 1 ]; then
  [ "$OPENCODE" = 1 ] || say "nothing to do: --skill-only needs --opencode (Claude Code installs the skill as a plugin, see the README)"
  exit 0
fi

if [ "${SOLIDCRAFT_AGENT_FROM_SOURCE:-0}" = 1 ]; then
  install_from_source
elif t="$(target)" && install_binary "$t"; then
  :
else
  say "no prebuilt binary for $(uname -s) $(uname -m); building from source instead"
  install_from_source
fi

BIN="$INSTALL_DIR/solidcraft-agent"
[ -x "$BIN" ] || die "install failed: $BIN not found"
say "installed $BIN"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "add $INSTALL_DIR to your PATH, e.g.: echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
esac
say "done. Open SolidCraft (or run 'solidcraft-agent launch'), then 'solidcraft-agent status'."
