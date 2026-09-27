# backend-engine — agent rules

## Build & test (MANDATORY)

- **Local verification = `cargo check` ONLY.** Run
  `cargo check --workspace --exclude backend-engine` after editing engine/mcp,
  and `cargo check -p backend-engine --target wasm32-unknown-unknown` after
  editing the Worker shell (it is wasm-only and cannot check natively).
- **Release builds and tests are done in CI** (`.github/workflows/ci.yml`: native check,
  wasm gate, `worker-build --release`, tests). Never run `cargo build`,
  `cargo build --release`, `cargo test`, `worker-build`, `wasm-opt`, or `wrangler deploy`
  locally.
- **Never** run `cargo clean` or delete `target/`.
- Sub-agents MUST follow the same rule: only `cargo check` locally.

## Source of truth

- Port map, deletions/additions, dep gates, env-var rules:
  [`WORKER-PORT-GUIDE.md`](WORKER-PORT-GUIDE.md). Do not re-decide §0 decisions.
- Big picture: [`docs/OVERVIEW.md`](docs/OVERVIEW.md), [`docs/FEATURES.md`](docs/FEATURES.md).
- Storage seam: `engine::Database` / `engine::ObjectStore` traits
  (`crates/engine/src/storage/`) — all backends are adapters behind them.

## Workflow

- Single tenant: one Worker = one app. No `board_id` routing, no multi-app logic.
- Secrets never in files: `SECRET_KEY` / `WORKER_KEY` via wrangler secret / CI secrets only.
- Ban list locally: never introduce `tokio`, `hyper`, `reqwest`, `bcrypt`, `jsonschema`,
  `jsonwebtoken` into the `engine`/`worker` dependency graph (CI wasm gate enforces this).
