#!/usr/bin/env bash
# Validate migration files: dialect pairing, sequential numbering, no dupes.
# Runs in CI (make check) and catches hand-created files the scaffold would prevent.
set -euo pipefail
cd "$(dirname "$0")/.."

ERRORS=0
fail() { echo "FAIL: $*"; ERRORS=$((ERRORS + 1)); }

# 1. Every .up.sql has a matching .down.sql (and vice versa) in each dialect.
for dir in sqlite postgres; do
  for f in backend/migrations/$dir/*.up.sql; do
    [ -f "$f" ] || continue
    down="${f%.up.sql}.down.sql"
    [ -f "$down" ] || fail "$dir: $(basename "$f") has no .down.sql pair"
  done
  for f in backend/migrations/$dir/*.down.sql; do
    [ -f "$f" ] || continue
    up="${f%.down.sql}.up.sql"
    [ -f "$up" ] || fail "$dir: $(basename "$f") has no .up.sql pair"
  done
done

# 2. Version numbers are sequential 1..N with no gaps or duplicates.
for dir in sqlite postgres; do
  versions=$(ls backend/migrations/$dir/*.up.sql 2>/dev/null | xargs -I{} basename {} | cut -d_ -f1 | sed 's/^0*//' | sort -n)
  expected=1
  while read -r v; do
    [ -z "$v" ] && continue
    if [ "$v" != "$expected" ]; then
      if [ "$v" -lt "$expected" ]; then
        fail "$dir: duplicate version $v (expected $expected)"
      else
        fail "$dir: gap after version $((expected - 1)) — found $v, expected $expected"
      fi
    fi
    expected=$((expected + 1))
  done <<< "$versions"
done

# 3. Both dialects have the same set of version numbers.
sqlite_versions=$(ls backend/migrations/sqlite/*.up.sql 2>/dev/null | xargs -I{} basename {} | cut -d_ -f1 | sort)
pg_versions=$(ls backend/migrations/postgres/*.up.sql 2>/dev/null | xargs -I{} basename {} | cut -d_ -f1 | sort)
if [ "$sqlite_versions" != "$pg_versions" ]; then
  diff <(echo "$sqlite_versions") <(echo "$pg_versions") | while read -r line; do
    fail "dialect mismatch: $line"
  done
fi

# 4. No TODO stubs left in migration files (unfilled scaffolds).
for dir in sqlite postgres; do
  for f in backend/migrations/$dir/*.sql; do
    [ -f "$f" ] || continue
    grep -q "TODO: write" "$f" && fail "$(basename "$f"): contains unfilled scaffold TODO"
  done
done

if [ "$ERRORS" -gt 0 ]; then
  echo ""
  echo "$ERRORS migration problem(s) found."
  echo "Use: make migration name=your_name  (auto-scaffolds correctly)"
  exit 1
fi

TOTAL=$(ls backend/migrations/sqlite/*.up.sql 2>/dev/null | wc -l | tr -d ' ')
echo "OK: $TOTAL migrations, both dialects paired, sequential, no stubs."
