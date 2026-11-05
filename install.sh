#!/bin/sh
# Installs reel and its agent skill: curl -fsSL https://raw.githubusercontent.com/alexandrebodin/reel/main/install.sh | sh
set -eu

REPO="alexandrebodin/reel"
RELEASES="${REEL_RELEASES:-https://github.com/$REPO/releases/latest/download}"
ROOT="$HOME/.reel"
BIN="$ROOT/bin"

fail() {
  printf 'reel: %s\n' "$1" >&2
  exit 1
}

case "$(uname -s) $(uname -m)" in
  "Darwin arm64") target="aarch64-apple-darwin" ;;
  "Darwin x86_64") target="x86_64-apple-darwin" ;;
  "Linux x86_64") target="x86_64-unknown-linux-musl" ;;
  *) fail "there is no build for $(uname -s) $(uname -m). Build it from https://github.com/$REPO with cargo install --path ." ;;
esac

asset="reel-$target.tar.gz"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# gh reaches a private repo, which a plain download cannot. Reading from /dev/null keeps it off the piped script.
curl -fsSL "$RELEASES/$asset" -o "$work/$asset" 2>/dev/null ||
  { command -v gh >/dev/null 2>&1 && gh release download --repo "$REPO" --pattern "$asset" --output "$work/$asset" --clobber </dev/null 2>/dev/null; } ||
  fail "could not download $asset from $RELEASES. If the repo is private, install gh, run gh auth login, then run this again."

tar -xzf "$work/$asset" -C "$work" || fail "could not unpack $asset"

mkdir -p "$BIN" "$ROOT/skills"
mv "$work/reel" "$BIN/reel"
rm -rf "$ROOT/skills/reel"
mv "$work/skills/reel" "$ROOT/skills/reel"
echo "Installed reel to $BIN/reel"

found_agent=""
for agent in "$HOME/.claude" "$HOME/.agents"; do
  [ -d "$agent" ] || continue
  found_agent="yes"
  link="$agent/skills/reel"
  if [ ! -e "$link" ] && [ ! -L "$link" ]; then
    mkdir -p "$agent/skills"
    ln -s "$ROOT/skills/reel" "$link"
    echo "Linked the skill at $link"
  elif [ "$(readlink "$link")" != "$ROOT/skills/reel" ]; then
    echo "Left $link as it is. The installed skill is at $ROOT/skills/reel"
  fi
done
[ -n "$found_agent" ] || echo "Point your agent at the skill in $ROOT/skills/reel"

command -v ffmpeg >/dev/null 2>&1 || echo "reel needs ffmpeg to make videos. Install it from https://ffmpeg.org"
command -v agent-browser >/dev/null 2>&1 || echo "reel needs agent-browser to record. Install it with: npm install -g agent-browser && agent-browser install"

case ":$PATH:" in
  *":$BIN:"*) ;;
  *)
    line="export PATH=\"$BIN:\$PATH\""
    case "${SHELL:-}" in
      */zsh) rc="$HOME/.zshrc" ;;
      */bash) rc="$HOME/.bashrc" ;;
      *) rc="" ;;
    esac
    if [ -n "$rc" ]; then
      grep -qsF "$BIN" "$rc" || printf '\n# reel\n%s\n' "$line" >> "$rc"
      echo "Added $BIN to your PATH in $rc"
    fi
    echo "Open a new terminal, or run: $line"
    ;;
esac
