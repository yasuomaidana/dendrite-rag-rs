#!/bin/sh
# SQL databases are reached through Diesel only (docs/specs/store.md, "Database access").
#
# Fails when
#   - a Cargo.toml declares sqlx, rusqlite, sea-orm, mysql or mysql_async, or the bare
#     postgres / tokio-postgres / libsql drivers without diesel in the same manifest
#     (diesel-async and diesel-libsql are Diesel backends and always fine);
#   - Rust code uses rusqlite::, sqlx::, sea_orm::, mysql::, mysql_async::, or the bare
#     postgres:: / tokio_postgres:: / libsql:: paths;
#   - sql_query, batch_execute or dsl::sql appears outside a module named `raw` (test code
#     in tests/, tests.rs and testutil.rs may use them).
# Exceptions: .github/db-access-allow.txt, one `path: reason` per line (the path is a file, or a
# directory ending in /). A line without a reason does not count. redb and tantivy are not SQL.
#
# usage: scripts/check-db-access.sh [root]    (default: the repository root)
root=${1:-$(cd "$(dirname "$0")/.." && pwd)}
cd "$root" || exit 2
allow=${DB_ACCESS_ALLOW:-.github/db-access-allow.txt}
fail=0
report() { printf 'db-access: %s\n' "$1" >&2; fail=1; }

allowed() { # $1 = repo-relative path
  [ -f "$allow" ] || return 1
  while IFS= read -r line; do
    case $line in ''|'#'*) continue ;; esac
    p=${line%%:*}; reason=${line#*:}
    [ -n "$(printf %s "$reason" | tr -d '[:space:]')" ] && [ "$reason" != "$line" ] || continue
    case $p in */) case $1 in "$p"*) return 0 ;; esac ;; *) [ "$1" = "$p" ] && return 0 ;; esac
  done < "$allow"
  return 1
}

list() { # $1 = name pattern
  find . \( -name target -o -name .git -o -name node_modules -o -path ./scripts/tests \) -prune -o \
    -type f -name "$1" -print | sed 's#^\./##' | sort
}

# --- manifests ---------------------------------------------------------------------------
for m in $(list Cargo.toml); do
  allowed "$m" && continue
  clean=$(sed 's/#.*//' "$m")
  # dependency keys: `name = ...` lines whose value is a version or an inline table (a
  # `name = [..]` line is a feature), plus [dependencies.name] table headers.
  keys=$(printf '%s\n' "$clean" | awk '
    /^\[/ { dep = ($0 ~ /dependencies/); h = $0
            if (dep && match(h, /dependencies\.[A-Za-z0-9_-]+\]/)) { n = substr(h, RSTART + 13, RLENGTH - 14); print n }
            next }
    dep && match($0, /^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*(=|\.)/) {
      k = $0; sub(/^[[:space:]]*/, "", k); sub(/[[:space:]]*(=|\.).*/, "", k)
      v = $0; sub(/^[^=]*=[[:space:]]*/, "", v)
      if (v ~ /^[{"]/ || $0 ~ /^[[:space:]]*[A-Za-z0-9_-]+\./) print k }')
  has_diesel=$(printf '%s\n' "$keys" | grep -cE '^diesel($|[-_])')
  for k in $(printf '%s\n' "$keys" | sort -u); do
    case $k in
      sqlx|rusqlite|sea-orm|sea_orm|mysql|mysql_async|mysql-async)
        report "$m: dependency '$k' is not Diesel (use diesel, or list the manifest in $allow)" ;;
      postgres|tokio-postgres|tokio_postgres|libsql)
        [ "$has_diesel" -gt 0 ] || report "$m: driver '$k' without diesel in the same manifest" ;;
    esac
  done
done

# --- sources -----------------------------------------------------------------------------
foreign='(^|[^A-Za-z0-9_])(rusqlite|sqlx|sea_orm|mysql|mysql_async|postgres|tokio_postgres|libsql)::'
rawsql='(^|[^A-Za-z0-9_])(sql_query|batch_execute)\(|dsl::sql'
for f in $(list '*.rs'); do
  allowed "$f" && continue
  code=$(grep -vE '^[[:space:]]*//' "$f")
  printf '%s\n' "$code" | grep -qE "$foreign" &&
    report "$f: uses a database crate other than Diesel ($(printf '%s\n' "$code" | grep -oE "$foreign" | head -1 | tr -d ' '))"
  case /$f in
    */raw/*|*/raw.rs|*/tests/*|*/tests.rs|*/testutil.rs|*/benches/*) continue ;;
  esac
  printf '%s\n' "$code" | grep -qE "$rawsql" && report "$f: hand-written SQL outside a module named raw"
done

[ "$fail" -eq 0 ] && echo "db-access: ok (SQL goes through Diesel)"
exit "$fail"
