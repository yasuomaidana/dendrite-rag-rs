#!/bin/sh
# Self-test of scripts/check-release-refs.sh with passing and failing fixture trees.
# Usage: scripts/tests/check-release-refs.test.sh
here=$(cd "$(dirname "$0")" && pwd)
guard="$here/../check-release-refs.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0
n=0

run_case() { # name expected(pass|fail) build-snippet
    name=$1 expect=$2 build=$3
    n=$((n + 1))
    dir="$tmp/case$n"
    mkdir -p "$dir/.github/workflows"
    (cd "$dir" && eval "$build")
    if sh "$guard" "$dir" >/dev/null 2>&1; then got=pass; else got=fail; fi
    if [ "$got" = "$expect" ]; then
        echo "ok   $name"
    else
        echo "FAIL $name (expected $expect, got $got)"
        failures=$((failures + 1))
    fi
}

run_case "registry dependencies only" pass \
    'printf "[dependencies]\nserde = \"1\"\n" > Cargo.toml'
run_case "git dependency with a release tag" pass \
    'printf "[dependencies]\nx = { git = \"https://github.com/o/x\", tag = \"v0.1.0\", version = \"0.1.0\" }\n" > Cargo.toml'
run_case "git dependency on main" pass \
    'printf "[dependencies]\nx = { git = \"https://github.com/o/x\", branch = \"main\" }\n" > Cargo.toml'
run_case "table form with a tag" pass \
    'printf "[dependencies.x]\ngit = \"https://github.com/o/x\"\ntag = \"v1.2.3\"\n" > Cargo.toml'
run_case "git dependency without tag" fail \
    'printf "[dependencies]\nx = { git = \"https://github.com/o/x\", version = \"0.1.0\" }\n" > Cargo.toml'
run_case "git dependency on develop" fail \
    'printf "[dependencies]\nx = { git = \"https://github.com/o/x\", branch = \"develop\" }\n" > Cargo.toml'
run_case "git dependency pinned by rev" fail \
    'printf "[dependencies]\nx = { git = \"https://github.com/o/x\", rev = \"abc123\" }\n" > Cargo.toml'
run_case "table form without tag" fail \
    'printf "[dependencies.x]\ngit = \"https://github.com/o/x\"\n" > Cargo.toml'
run_case "patch section without tag" fail \
    'printf "[patch.crates-io]\nx = { git = \"https://github.com/o/x\" }\n" > Cargo.toml'
run_case "nested crate manifest without tag" fail \
    'mkdir -p crates/a; printf "[dependencies]\nx = { git = \"https://github.com/o/x\" }\n" > crates/a/Cargo.toml'
run_case "workflow checks out main" pass \
    'printf "steps:\n  - uses: actions/checkout@v4\n    with:\n      ref: main\n" > .github/workflows/ci.yml'
run_case "workflow checks out a tag" pass \
    'printf "steps:\n  - uses: actions/checkout@v4\n    with:\n      ref: v0.1.0\n" > .github/workflows/ci.yml'
run_case "workflow checks out develop" fail \
    'printf "steps:\n  - uses: actions/checkout@v4\n    with:\n      ref: develop\n" > .github/workflows/ci.yml'
run_case "workflow defaults a ref to develop" fail \
    'printf "      ref: ${REF:-develop}\n" > .github/workflows/ci.yml'
run_case "workflow installs a tool from a develop branch" fail \
    'printf "      run: cargo install --git https://github.com/o/x --branch develop\n" > .github/workflows/ci.yml'
run_case "workflow installs a tool from a tag" pass \
    'printf "      run: cargo install --git https://github.com/o/x --tag v0.1.0\n" > .github/workflows/ci.yml'
run_case "workflow triggers on develop pushes" pass \
    'printf "on:\n  push:\n    branches: [develop, main]\n" > .github/workflows/ci.yml'
run_case "markdown links to main" pass \
    'printf "[a](https://github.com/o/x/blob/main/a.md)\n" > README.md'
run_case "markdown links to develop" fail \
    'printf "[a](https://github.com/o/x/blob/develop/a.md)\n" > README.md'

echo "$n cases, $failures failures"
[ "$failures" -eq 0 ]
