#!/bin/sh
# Lyra installer for macOS: user directory only, no sudo, no Node.
#   curl -fsSL https://raw.githubusercontent.com/ImHangLi/lyra/main/scripts/install.sh | sh
# Environment:
#   LYRA_VERSION       release version to install (default: latest release)
#   LYRA_INSTALL_DIR   where to put the binary (default: ~/.lyra/bin)
#   LYRA_INSTALL_FROM  local directory with release archives (offline/testing)
#   LYRA_NO_MODIFY_PATH  set to leave shell profiles unchanged (the PATH line is printed instead)
# The installer verifies the SHA-256 checksum, refuses to replace a `lyra` it did not install,
# and never removes macOS quarantine attributes or bypasses Gatekeeper.
set -eu

repo="ImHangLi/lyra"
dir="${LYRA_INSTALL_DIR:-$HOME/.lyra/bin}"
marker="$dir/.lyra-installed"

fail() { echo "lyra install: $*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || fail "only macOS is supported"
case "$(uname -m)" in
  arm64) target="aarch64-apple-darwin" ;;
  x86_64) target="x86_64-apple-darwin" ;;
  *) fail "unsupported architecture $(uname -m)" ;;
esac

version="${LYRA_VERSION:-}"
version="${version#v}"
if [ -z "$version" ] && [ -z "${LYRA_INSTALL_FROM:-}" ]; then
  version="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)"
  [ -n "$version" ] || fail "cannot find the latest release; set LYRA_VERSION"
fi
if [ -z "$version" ]; then
  archive="$(ls "$LYRA_INSTALL_FROM"/lyra-*-"$target".tar.gz 2>/dev/null | tail -1)"
  [ -n "$archive" ] || fail "no archive for $target in $LYRA_INSTALL_FROM"
  name="$(basename "$archive" .tar.gz)"
else
  name="lyra-$version-$target"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
if [ -n "${LYRA_INSTALL_FROM:-}" ]; then
  cp "$LYRA_INSTALL_FROM/$name.tar.gz" "$LYRA_INSTALL_FROM/$name.tar.gz.sha256" "$tmp/" || fail "archive not found"
else
  base="https://github.com/$repo/releases/download/v$version"
  curl -fsSL "$base/$name.tar.gz" -o "$tmp/$name.tar.gz" || fail "download failed: lyra $version has no $target build (see https://github.com/$repo/releases)"
  curl -fsSL "$base/$name.tar.gz.sha256" -o "$tmp/$name.tar.gz.sha256" || fail "checksum download failed"
fi
(cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null) || fail "checksum mismatch; nothing was installed"
tar -C "$tmp" -xzf "$tmp/$name.tar.gz"

if [ -e "$dir/lyra" ] && [ ! -f "$marker" ]; then
  fail "$dir/lyra exists and was not installed by this script; remove it or set LYRA_INSTALL_DIR"
fi
mkdir -p "$dir"
cp "$tmp/$name/lyra" "$dir/lyra.new"
chmod 0755 "$dir/lyra.new"
mv "$dir/lyra.new" "$dir/lyra"
echo "$name" > "$marker"

other="$(command -v lyra 2>/dev/null || true)"
echo "Installed $("$dir/lyra" --version) to $dir/lyra"
if [ -n "$other" ] && [ "$other" != "$dir/lyra" ]; then
  echo "Note: another lyra is first on PATH: $other"
fi
# Put the install directory on PATH for new shells, once, unless LYRA_NO_MODIFY_PATH is set.
case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    case "$(basename "${SHELL:-}")" in
      zsh) rc="${ZDOTDIR:-$HOME}/.zshrc"; line="export PATH=\"$dir:\$PATH\"" ;;
      bash) rc="$HOME/.bash_profile"; line="export PATH=\"$dir:\$PATH\"" ;;
      fish) rc="$HOME/.config/fish/conf.d/lyra.fish"; line="fish_add_path \"$dir\"" ;;
      *) rc="" ;;
    esac
    # The path is written into a shell profile, so it must not contain characters a shell
    # would interpret there.
    case "$dir" in
      *[\"\$\`\\]* | *"
"*) unsafe=1 ;;
      *) unsafe="" ;;
    esac
    if [ -n "${LYRA_NO_MODIFY_PATH:-}" ] || [ -z "$rc" ] || [ -n "$unsafe" ]; then
      echo "Add it to PATH:  export PATH=\"$dir:\$PATH\""
    else
      if ! grep -qsF "$line" "$rc"; then
        mkdir -p "$(dirname "$rc")"
        printf '\n# Added by the Lyra installer\n%s\n' "$line" >> "$rc"
        echo "Added $dir to PATH in $rc."
      fi
      echo "Open a new terminal, or run now:  export PATH=\"$dir:\$PATH\""
    fi
    ;;
esac
echo "Next: in your project, run 'lyra setup'. It prepares your coding agent and prints the one command to run."
echo "Uninstall: rm \"$dir/lyra\" \"$marker\", and remove the Lyra line from your shell profile (project .lyra files and workspace data are kept)."
