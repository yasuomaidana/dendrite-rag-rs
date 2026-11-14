#!/bin/sh
# Runs scripts/check-release-refs.sh against the fixture trees: each case names the expected
# result and a fragment of the message.
here=$(cd "$(dirname "$0")" && pwd)
script=$here/../check-release-refs.sh
bad=0
# Fixtures carry a .fixture suffix so cargo, go and editors do not treat them as real manifests.
tmp=$(mktemp -d) || exit 2
trap 'rm -rf "$tmp"' EXIT
materialize() { # name -> $tmp/name
  cp -R "$here/release-refs-fixtures/$1" "$tmp/$1"
  find "$tmp/$1" -name '*.fixture' | while IFS= read -r f; do mv "$f" "${f%.fixture}"; done
}
expect() { # name, want (0|1), fragment
  materialize "$1"
  out=$(sh "$script" "$tmp/$1" 2>&1); got=$?
  if [ "$got" -ne "$2" ]; then echo "FAIL $1: exit $got, want $2"; echo "$out"; bad=1
  elif [ -n "$3" ] && ! printf %s "$out" | grep -qF "$3"; then echo "FAIL $1: missing '$3'"; echo "$out"; bad=1
  else echo "ok   $1"; fi
}
expect pass 0 "ok (siblings are referenced by tag"
expect fail-git-notag 1 "git dependency without tag"
expect fail-rev 1 "pins rev"
expect fail-branch 1 "branch other than main"
expect fail-path 1 "path outside the repository"
expect fail-pseudo 1 "pseudo-version"
expect fail-replace 1 "replace directive"
expect fail-ref 1 "sibling checkout at ref"
expect fail-doc 1 "links to a sibling's develop"
exit $bad
