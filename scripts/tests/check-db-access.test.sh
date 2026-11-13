#!/bin/sh
# Runs scripts/check-db-access.sh against the fixture trees: each case names the expected
# result and a fragment of the message.
here=$(cd "$(dirname "$0")" && pwd)
script=$here/../check-db-access.sh
bad=0
# Fixtures carry a .fixture suffix so cargo, the quality gates and editors do not treat them as
# real crates; each case is copied to a temporary tree with the suffix removed.
tmp=$(mktemp -d) || exit 2
trap 'rm -rf "$tmp"' EXIT
materialize() { # name -> $tmp/name
  cp -R "$here/fixtures/$1" "$tmp/$1"
  find "$tmp/$1" -name '*.fixture' | while IFS= read -r f; do mv "$f" "${f%.fixture}"; done
}
expect() { # name, want (0|1), fragment
  materialize "$1"
  out=$(sh "$script" "$tmp/$1" 2>&1); got=$?
  if [ "$got" -ne "$2" ]; then echo "FAIL $1: exit $got, want $2"; echo "$out"; bad=1
  elif [ -n "$3" ] && ! printf %s "$out" | grep -qF "$3"; then echo "FAIL $1: missing '$3'"; echo "$out"; bad=1
  else echo "ok   $1"; fi
}
expect pass 0 "ok (SQL goes through Diesel)"
expect fail-manifest 1 "dependency 'sqlx'"
expect fail-driver 1 "driver 'tokio-postgres' without diesel"
expect fail-source 1 "uses a database crate other than Diesel"
expect fail-rawsql 1 "hand-written SQL outside a module named raw"
expect allowed 0 ""
expect no-reason 1 "dependency 'sqlx'"
exit $bad
