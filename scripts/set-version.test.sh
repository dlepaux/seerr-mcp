#!/usr/bin/env bash
# Tests for scripts/set-version.sh, the toolchain-free replacement for `cargo set-version` in .releaserc.
#
# Run from the repo root: bash scripts/set-version.test.sh
# The first case needs cargo, because the proof that the bump is correct is cargo itself accepting the
# lockfile under --locked (which refuses a toml-only bump, exit 101). It reads HEAD, so it needs a commit.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/set-version.sh"
[ -x "$SCRIPT" ] || { echo "FAIL: $SCRIPT missing or not executable"; exit 1; }

pass=0; fail=0
ok() { echo "  ok: $*"; pass=$((pass+1)); }
no() { echo "  FAIL: $*"; fail=$((fail+1)); }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

echo "TEST 1: the real manifest — exactly the two version lines change, and cargo accepts the lock under --locked"
mkdir "$TMP/real" && git -C "$ROOT" archive HEAD | tar -x -C "$TMP/real"
cp "$TMP/real/Cargo.toml" "$TMP/toml.before"; cp "$TMP/real/Cargo.lock" "$TMP/lock.before"
( cd "$TMP/real" && "$SCRIPT" 9.8.7 ) && ok "exit 0" || no "exit $?"
changed=$( { diff "$TMP/toml.before" "$TMP/real/Cargo.toml"; diff "$TMP/lock.before" "$TMP/real/Cargo.lock"; } | grep -c '^>' )
[ "$changed" = 2 ] && ok "exactly two lines changed" || no "$changed lines changed"
grep -qx 'version = "9.8.7"' "$TMP/real/Cargo.toml" && ok "Cargo.toml [package] version" || no "Cargo.toml"
grep -A1 -x 'name = "seerr-mcp"' "$TMP/real/Cargo.lock" | grep -qx 'version = "9.8.7"' && ok "Cargo.lock entry" || no "Cargo.lock"
if ( cd "$TMP/real" && cargo metadata --locked --format-version 1 >/dev/null 2>"$TMP/meta.err" ); then
  ok "cargo metadata --locked accepts the bumped lock"
else
  no "cargo rejected the lock: $(tail -2 "$TMP/meta.err")"
fi

echo "TEST 2: only the [package] version moves — a dependency's own version line does not"
mkdir "$TMP/dep" && cd "$TMP/dep"
printf '[package]\nname = "seerr-mcp"\nversion = "1.0.0"\n\n[dependencies.foo]\nversion = "1.0.0"\n' > Cargo.toml
printf '[[package]]\nname = "foo"\nversion = "1.0.0"\n\n[[package]]\nname = "seerr-mcp"\nversion = "1.0.0"\n' > Cargo.lock
"$SCRIPT" 2.0.0 && ok "exit 0" || no "exit $?"
[ "$(grep -c 'version = "2.0.0"' Cargo.toml)" = 1 ] && [ "$(grep -c 'version = "1.0.0"' Cargo.toml)" = 1 ] && ok "dependency version untouched in Cargo.toml" || no "$(cat Cargo.toml)"
[ "$(sed -n 3p Cargo.lock)" = 'version = "1.0.0"' ] && [ "$(sed -n 7p Cargo.lock)" = 'version = "2.0.0"' ] && ok "only seerr-mcp's lock entry moved" || no "$(cat Cargo.lock)"

echo "TEST 3: a version that is not semver is refused, and nothing is written"
cp Cargo.toml t.before; cp Cargo.lock l.before
"$SCRIPT" "2.0.0; rm -rf /" >/dev/null 2>&1 && no "accepted a non-semver" || ok "refused"
cmp -s Cargo.toml t.before && cmp -s Cargo.lock l.before && ok "files unchanged" || no "a file changed"

echo "TEST 4: a lock without the package fails loudly and leaves BOTH files unchanged"
printf '[[package]]\nname = "foo"\nversion = "1.0.0"\n' > Cargo.lock; cp Cargo.lock l.before
"$SCRIPT" 3.0.0 >/dev/null 2>&1 && no "exit 0 with no lock entry" || ok "non-zero exit"
cmp -s Cargo.toml t.before && ok "Cargo.toml not half-written" || no "Cargo.toml changed"
cmp -s Cargo.lock l.before && ok "Cargo.lock unchanged" || no "Cargo.lock changed"

echo
echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
