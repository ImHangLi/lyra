#!/usr/bin/env bash
# Builds native macOS release archives and SHA-256 checksums into dist/.
# Usage: scripts/release/package.sh [TARGET...]   (default: aarch64-apple-darwin x86_64-apple-darwin)
# Uses the shared heavy-check lock when it exists, so only one large build runs at a time.
set -euo pipefail
cd "$(dirname "$0")/../.."
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
targets=("$@")
[[ ${#targets[@]} -eq 0 ]] && targets=(aarch64-apple-darwin x86_64-apple-darwin)
lock=(); [[ -f "$HOME/.codex/scripts/with-heavy-check-lock.py" ]] && lock=(python3 "$HOME/.codex/scripts/with-heavy-check-lock.py" --)
mkdir -p dist
for t in "${targets[@]}"; do
  rustup target add "$t" >/dev/null
  "${lock[@]}" cargo build --release --locked --target "$t" -p lyra-cli
  name="lyra-${version}-${t}"
  stage="$(mktemp -d)/$name"
  mkdir -p "$stage"
  cp "target/$t/release/lyra" "$stage/lyra"
  cp README.md "$stage/"
  tar -C "$(dirname "$stage")" -czf "dist/$name.tar.gz" "$name"
  (cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
  echo "dist/$name.tar.gz ($(file -b "$stage/lyra" | cut -d, -f1-2))"
done
