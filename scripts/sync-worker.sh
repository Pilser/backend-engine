#!/usr/bin/env bash
# Sync the latest Worker build to Cloudflare.
#
#   scripts/sync-worker.sh [--env production] [--artifact DIR] [--skip-build] [--secrets-only]
#
# Modes:
#   1. Local build (default): runs worker-build, then `wrangler deploy`.
#   2. CI artifact: --artifact DIR reuses a `worker-dist/` dir downloaded from CI
#      (gh run download -n worker-dist -R <repo>) instead of building locally.
#   3. --secrets-only: just sync secrets, no deploy.
#
# Secrets (SECRET_KEY, WORKER_KEY) are read from the SHELL ENVIRONMENT ONLY —
# never from files. Export them before running:
#   export SECRET_KEY="$(openssl rand -base64 32)"
#   export WORKER_KEY="..."
# Requires: wrangler (npm i -g wrangler), optionally worker-build + gh CLI.
set -euo pipefail
cd "$(dirname "$0")/.."

ENV="production"
ARTIFACT=""
SKIP_BUILD=0
SECRETS_ONLY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --env) ENV="$2"; shift 2 ;;
    --artifact) ARTIFACT="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --secrets-only) SECRETS_ONLY=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 1 ;;
  esac
done

command -v wrangler >/dev/null || { echo "wrangler not found: npm i -g wrangler" >&2; exit 1; }

if [[ -z "${CLOUDFLARE_API_TOKEN:-}" ]]; then
  echo "CLOUDFLARE_API_TOKEN not set — falling back to wrangler login session." >&2
  export CLOUDFLARE_API_TOKEN="${CLOUDFLARE_API_TOKEN:-}"
fi

put_secret() { # name value
  if [[ -n "${2:-}" ]]; then
    printf '%s' "$2" | wrangler secret put "$1" --env "$ENV"
  else
    echo "skip: $1 not set in environment" >&2
  fi
}

echo "== syncing secrets (env -> worker, --env $ENV) =="
put_secret SECRET_KEY "${SECRET_KEY:-}"
put_secret WORKER_KEY "${WORKER_KEY:-}"

if [[ "$SECRETS_ONLY" == "1" ]]; then echo "secrets synced, no deploy (--secrets-only)."; exit 0; fi

# One-time infra (idempotent): D1/R2 ids must already be pasted into
# wrangler.toml (see README). The webhook queue is account-level — create it
# if missing; without it the scheduler delivers webhooks inline.
echo "== ensuring webhook queue exists =="
wrangler queues create webhook-deliveries 2>/dev/null || echo "(queue already exists or creation deferred — inline delivery fallback applies)"

if [[ -n "$ARTIFACT" ]]; then
  [[ -f "$ARTIFACT/shim.mjs" ]] || { echo "artifact dir $ARTIFACT missing shim.mjs" >&2; exit 1; }
  echo "== deploying prebuilt artifact $ARTIFACT =="
  mkdir -p build/serverless-worker
  cp "$ARTIFACT/shim.mjs" build/serverless-worker/shim.mjs
  [[ -f "$ARTIFACT/serverless-worker.wasm" ]] && cp "$ARTIFACT/serverless-worker.wasm" build/serverless-worker/serverless-worker.wasm
  SKIP_BUILD=1
fi

if [[ "$SKIP_BUILD" == "0" ]]; then
  echo "== building wasm (worker-build --release) =="
  worker-build --release
fi

echo "== deploying to Cloudflare (--env $ENV) =="
wrangler deploy --env "$ENV"
echo "== done. Public URL: see wrangler output; save it as WORKER_URL =="
