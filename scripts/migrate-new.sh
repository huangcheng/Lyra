#!/usr/bin/env bash
# Scaffold a new migration for both dialects with the next version number.
# Usage: make migration name=add_dav_sync_columns
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="${1:?Usage: make migration name=some_snake_case_name}"
# Sanitize: lowercase, [a-z0-9_]
NAME=$(echo "$NAME" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9_\n' '_' | sed 's/^_*//;s/_*$//')
[ -n "$NAME" ] || { echo "error: name sanitizes to empty"; exit 1; }

# Next version: max across BOTH dialects + 1 (prevents parallel-session collisions
# only within a single checkout; CI check catches cross-branch collisions).
MAX=0
for dir in backend/migrations/sqlite backend/migrations/postgres; do
  for f in "$dir"/*.up.sql; do
    [ -f "$f" ] || continue
    V=$(basename "$f" | cut -d_ -f1)
    V=$((10#$V))  # force base-10 (bash treats 017 as octal 15)
    [ "$V" -gt "$MAX" ] 2>/dev/null && MAX=$V
  done
done
NEXT=$((MAX + 1))
PADDED=$(printf "%04d" "$NEXT")

for dir in sqlite postgres; do
  UP="backend/migrations/$dir/${PADDED}_${NAME}.up.sql"
  DOWN="backend/migrations/$dir/${PADDED}_${NAME}.down.sql"
  if [ -f "$UP" ]; then
    echo "error: $UP already exists"; exit 1
  fi
  cat > "$UP" <<SQL
-- TODO: write the ${NAME} migration for ${dir}.
-- Both dialects must produce the same logical schema.
-- SQLite: no IF NOT EXISTS on ADD COLUMN; PG: use IF NOT EXISTS.
-- See existing migrations for dialect-specific patterns.

SQL
  cat > "$DOWN" <<SQL
-- TODO: reverse ${NAME} for ${dir}.

SQL
  echo "created $UP"
  echo "created $DOWN"
done

echo ""
echo "Next: edit both .up.sql files, then run:"
echo "  make migration-check"
