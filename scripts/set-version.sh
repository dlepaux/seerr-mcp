#!/usr/bin/env bash
# set-version.sh <version> — set this crate's version in Cargo.toml and Cargo.lock, with no Rust toolchain.
#
# semantic-release's prepare step used `cargo set-version`, the ONLY reason the release job needed a Rust
# toolchain and cargo-edit. This edits exactly the two lines that command
# edits: the `[package]` version and this crate's `[[package]]` entry in the lock. CI proves the result is
# one cargo accepts under --locked (scripts/set-version.test.sh).
#
# Both files are written only after both edits succeed, so a failure never leaves a half-bumped tree.
set -euo pipefail

PKG=seerr-mcp
v="${1:?usage: set-version.sh <semver>}"
[[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || { echo "set-version: not a semver: $v" >&2; exit 1; }

toml_tmp="$(mktemp)"; lock_tmp="$(mktemp)"
trap 'rm -f "$toml_tmp" "$lock_tmp"' EXIT

# The first `version = "…"` line inside [package], and no other table's.
awk -v v="$v" '
  /^\[/ { in_pkg = ($0 == "[package]") }
  in_pkg && !done && /^version = "/ { $0 = "version = \"" v "\""; done = 1 }
  { print }
  END { if (!done) exit 3 }' Cargo.toml > "$toml_tmp" || { echo "set-version: no [package] version in Cargo.toml" >&2; exit 1; }

# The version line of this crate's own [[package]] entry.
awk -v v="$v" -v name="name = \"$PKG\"" '
  /^\[\[package\]\]$/ { mine = 0 }
  $0 == name { mine = 1 }
  mine && !done && /^version = "/ { $0 = "version = \"" v "\""; done = 1 }
  { print }
  END { if (!done) exit 3 }' Cargo.lock > "$lock_tmp" || { echo "set-version: no $PKG entry in Cargo.lock" >&2; exit 1; }

cat "$toml_tmp" > Cargo.toml
cat "$lock_tmp" > Cargo.lock
echo "set-version: $PKG $v (Cargo.toml, Cargo.lock)"
