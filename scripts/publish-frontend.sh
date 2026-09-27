#!/usr/bin/env bash
# Copies the frontend build (web/dist) INTO the engine: each file becomes an
# asset via PUT /api/assets/*, served back at GET /srv/* with an index.html
# SPA fallback. No wrangler [assets], no separate hosting — the engine IS
# the host.
#
#   ENGINE_URL=http://localhost:8788 ENGINE_KEY=local-demo-key bash scripts/publish-frontend.sh
#   ENGINE_URL=https://demo-example.<acct>.workers.dev ENGINE_KEY="$WORKER_KEY" bash scripts/publish-frontend.sh
set -euo pipefail

BASE="${ENGINE_URL:-http://localhost:8788}"
KEY="${ENGINE_KEY:-local-demo-key}"
DIST="${FRONTEND_DIST:-web/dist}"

[ -d "$DIST" ] || { echo "missing $DIST — run 'bun run build:web' first"; exit 1; }

count=0
while IFS= read -r f; do
  rel="${f#$DIST/}"
  code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 30 --data-binary "@$f" -X PUT "$BASE/api/assets/$rel?key=$KEY")
  if [ "$code" != "200" ]; then echo "PUT $rel -> HTTP $code"; exit 1; fi
  count=$((count + 1))
done < <(find "$DIST" -type f | sort)

echo "uploaded $count files: $DIST -> $BASE/srv/ (open $BASE/srv/ in a browser)"
