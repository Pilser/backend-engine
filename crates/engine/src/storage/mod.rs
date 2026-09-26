// In-RAM decorator cache: uses `Instant::now`, which panics on
// wasm32-unknown-unknown. It has no in-engine callers, so it is compiled
// out on wasm (the durable backend is always the source of truth).
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
pub mod database;
pub mod ir;
pub mod memory;
pub mod object_store;
