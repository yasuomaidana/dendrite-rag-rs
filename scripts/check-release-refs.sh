#!/bin/sh
# Release reference policy: a final version refers to its siblings by release tag, or by their main branch.
#
# Fails when
#   * a Cargo.toml git dependency has no tag = "vX.Y.Z" and is not on branch = "main";
#   * a git dependency names a rev, or a branch other than main;
#   * a workflow checks out or installs from a develop ref (`ref:`, `--branch`, a *_REF variable defaulting to it);
#   * a Markdown or workflow file links to a develop branch (blob/develop, tree/develop, raw/develop).
#
# Usage: scripts/check-release-refs.sh [root]   (default: the current directory)

root=${1:-.}
cd "$root" || { echo "check-release-refs: cannot enter $root" >&2; exit 2; }
fail=0
tmp=$(mktemp)
trap 'rm -f "$tmp" "$tmp.list"' EXIT

report() { # file message
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        echo "::error file=$1::$2"
    fi
    echo "release-refs: $1: $2" >&2
    fail=1
}

files() { # patterns...: tracked files in a git work tree, otherwise every matching file under the root
    if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git ls-files -- "$@"
    else
        find . \( -path ./target -o -path ./.git \) -prune -o -type f -print | sed 's|^\./||' | while IFS= read -r f; do
            for pat in "$@"; do
                case $f in $pat) echo "$f"; break ;; esac
            done
        done
    fi
}

# Cargo manifests: a unit is an inline-table line carrying `git =`, or a whole [dependencies.x] table.
files 'Cargo.toml' '*/Cargo.toml' '*/*/Cargo.toml' '*/*/*/Cargo.toml' >"$tmp.list"
while IFS= read -r manifest; do
    [ -f "$manifest" ] || continue
    awk -v file="$manifest" '
        function bad(where, msg) { printf "%s\t%d: %s\n", file, where, msg; status = 1 }
        function verdict(text, where,    branch) {
            if (text !~ /(^|[ \t{,\n])git[ \t]*=/) return
            if (text ~ /(^|[ \t{,\n])rev[ \t]*=/) { bad(where, "git dependency pinned by rev; use tag = \"vX.Y.Z\""); return }
            branch = ""
            if (match(text, /branch[ \t]*=[ \t]*"[^"]*"/)) {
                branch = substr(text, RSTART, RLENGTH); sub(/^[^"]*"/, "", branch); sub(/"$/, "", branch)
                if (branch != "main") { bad(where, "git dependency on branch " branch "; use tag = \"vX.Y.Z\" or branch = \"main\""); return }
            }
            if (branch == "" && text !~ /tag[ \t]*=[ \t]*"v[0-9]+\.[0-9]+\.[0-9]+[^"]*"/)
                bad(where, "git dependency without tag = \"vX.Y.Z\"")
        }
        function flush() { if (block != "") verdict(block, blockline); block = "" }
        /^[ \t]*\[/ { flush(); blockline = NR; if ($0 ~ /dependencies\./ || $0 ~ /^[ \t]*\[patch\./) block = $0 "\n"; next }
        block != "" { block = block $0 "\n"; next }
        /\{.*git[ \t]*=/ { verdict($0, NR) }
        END { flush(); exit status }
    ' "$manifest" >"$tmp" 2>&1 || while IFS="$(printf '\t')" read -r f msg; do report "$f" "$msg"; done <"$tmp"
done <"$tmp.list"

# Workflows: no develop ref (checkout ref:, cargo install --branch, a *_REF variable defaulting to develop).
files '.github/workflows/*.yml' '.github/workflows/*.yaml' >"$tmp.list"
while IFS= read -r wf; do
    [ -f "$wf" ] || continue
    n=$(grep -nE "(ref:|--branch|--rev|_REF).*develop" "$wf" | grep -v "^[0-9]*:[[:space:]]*#" | head -1 | cut -d: -f1)
    [ -n "$n" ] && report "$wf" "line $n: names a develop ref; use a release tag or main"
done <"$tmp.list"

# Documentation and workflow links to a develop branch.
files '*.md' '.github/workflows/*.yml' >"$tmp.list"
while IFS= read -r doc; do
    [ -f "$doc" ] || continue
    n=$(grep -nE "(blob|tree|raw)/develop(/|[^[:alnum:]_-]|$)" "$doc" | head -1 | cut -d: -f1)
    [ -n "$n" ] && report "$doc" "line $n: links to a develop branch; link a tag or main"
done <"$tmp.list"

if [ "$fail" -ne 0 ]; then
    echo "release-refs: FAILED; a final version names release tags or main, never develop, a rev or a bare git dependency." >&2
    exit 1
fi
echo "release-refs: OK"
