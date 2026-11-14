#!/bin/sh
# A final version references its siblings by release, never by a moving branch.
# Sibling repositories (yasuomaidana/dendrite-*, embedag) are used as a release tag, a crates.io
# or Go semver version, or the sibling's main branch.
#
# Fails when
#   - a Cargo.toml dependency uses git = ".." without tag = ".." (branch = "main" is the only
#     branch allowed), or pins rev = "..", or a branch other than main;
#   - a Cargo.toml dependency is a path outside the repository (a sibling checkout);
#   - go.mod requires a github.com/yasuomaidana/* module at a pseudo-version, or replaces any
#     module with a local path or a yasuomaidana fork;
#   - a workflow checks out a yasuomaidana/* repository at a ref that is neither main nor a
#     v<number> tag (the literals of `${{ vars.X || 'main' }}` count);
#   - a Markdown file links to a sibling's blob/develop or tree/develop.
# A moving ref is fine while a branch is being worked on; this guard runs on every pull request
# and push, and the release branch (main) must pass it.
#
# usage: scripts/check-release-refs.sh [root]    (default: the repository root)
root=${1:-$(cd "$(dirname "$0")/.." && pwd)}
cd "$root" || exit 2
fail=0
report() { printf 'release-refs: %s\n' "$1" >&2; fail=1; }
flag() { printf '%s\n' "$1" | while IFS= read -r l; do printf 'release-refs: %s\n' "$l" >&2; done; fail=1; }
list() { # $1 = find name pattern
  find . \( -name target -o -name .git -o -name node_modules -o -name .ci -o -path ./scripts/tests \) -prune -o \
    -type f -name "$1" -print | sed 's#^\./##' | sort
}

# --- Cargo manifests ---------------------------------------------------------------------
for m in $(list Cargo.toml); do
  out=$(sed 's/#.*//' "$m" | awk -v m="$m" '
    function check(name, spec) {
      if (spec ~ /(^|[ ,{])git[ ]*=/) {
        if (spec ~ /(^|[ ,{])rev[ ]*=/) print m ": " name " pins rev (use tag = \"v..\")"
        else if (spec ~ /(^|[ ,{])branch[ ]*=/ && spec !~ /branch[ ]*=[ ]*"main"/) print m ": " name " follows a branch other than main"
        else if (spec !~ /(^|[ ,{])tag[ ]*=/ && spec !~ /branch[ ]*=[ ]*"main"/) print m ": " name " is a git dependency without tag = \"v..\""
      }
      if (match(spec, /path[ ]*=[ ]*"[^"]*"/)) {
        p = substr(spec, RSTART, RLENGTH); sub(/^path[ ]*=[ ]*"/, "", p); sub(/"$/, "", p)
        ups = 0; q = p; while (q ~ /^\.\.\//) { ups++; sub(/^\.\.\//, "", q) }
        depth = gsub(/\//, "/", m)
        if (ups > depth || p ~ /^\//) print m ": " name " is a path outside the repository (" p ")"
      }
    }
    /^\[/ { dep = ($0 ~ /dependencies/); tbl = ""
            if (dep && match($0, /dependencies\.[A-Za-z0-9_-]+\]/)) { tbl = substr($0, RSTART + 13, RLENGTH - 14) }
            next }
    dep && tbl != "" { check(tbl, $0); next }
    dep && /^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=[[:space:]]*\{/ {
      n = $0; sub(/^[[:space:]]*/, "", n); sub(/[[:space:]]*=.*/, "", n); check(n, $0) }
    dep && /^[[:space:]]*[A-Za-z0-9_-]+\.(git|path|rev|branch)[[:space:]]*=/ {
      n = $0; sub(/^[[:space:]]*/, "", n); check(n, " " $0) }
  ')
  [ -n "$out" ] && flag "$out"
done

# --- go.mod --------------------------------------------------------------------------------
for g in $(list go.mod); do
  out=$(grep -nE 'github\.com/yasuomaidana/[^ ]+[[:space:]]+v[0-9][^ ]*-[0-9]{14}-[0-9a-f]{12}' "$g" |
    sed "s#^\([0-9]*\):.*#$g:\1: pseudo-version of a sibling module (require a release tag)#")
  [ -n "$out" ] && flag "$out"
  out=$(grep -nE '=>[[:space:]]+(\.|/|github\.com/yasuomaidana/)' "$g" |
    sed "s#^\([0-9]*\):.*#$g:\1: replace directive points at a local path or a fork#")
  [ -n "$out" ] && flag "$out"
done

# --- workflows -----------------------------------------------------------------------------
for w in $(find .github/workflows -type f \( -name '*.yml' -o -name '*.yaml' \) 2>/dev/null | sort); do
  out=$(awk -v w="$w" '
    /^[[:space:]]*-[[:space:]]/ { repo = 0 }
    /repository:[[:space:]]*yasuomaidana\// { repo = 1; next }
    repo && /^[[:space:]]*ref:/ {
      v = $0; sub(/^[[:space:]]*ref:[[:space:]]*/, "", v)
      bad = 0; found = 0; rest = v
      while (match(rest, /\x27[^\x27]*\x27/)) {
        lit = substr(rest, RSTART + 1, RLENGTH - 2); rest = substr(rest, RSTART + RLENGTH); found = 1
        if (lit != "main" && lit !~ /^v[0-9]/) bad = 1 }
      if (!found) { lit = v; gsub(/["]/, "", lit); if (lit != "main" && lit !~ /^v[0-9]/) bad = 1 }
      if (bad) print w ": sibling checkout at ref " v " (use main or a v<number> tag)"
      repo = 0 }
  ' "$w")
  [ -n "$out" ] && flag "$out"
done

# --- Markdown links ------------------------------------------------------------------------
for d in $(list '*.md'); do
  if grep -qE 'github\.com/yasuomaidana/[^ )]*/(blob|tree)/develop' "$d"; then
    report "$d: links to a sibling's develop branch (use main or a tag)"; fail=1
  fi
done

[ "$fail" -eq 0 ] && echo "release-refs: ok (siblings are referenced by tag, version or main)"
exit $fail
