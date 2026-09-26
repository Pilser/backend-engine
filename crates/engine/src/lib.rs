pub mod audit;
pub mod auth;
pub mod automation;
pub mod cron;
pub mod crud;
pub mod engine;
pub mod events;
pub mod expr;
pub mod files;
pub mod http;
pub mod import;
pub mod jobs;
pub mod migrations;
pub mod model;
pub mod oauth;
pub mod policy;
pub mod query;
pub mod realtime;
pub mod registry;
pub mod schema;
pub mod secrets;
pub mod storage;
pub mod tables;
pub mod webhooks;

pub use crate::model::{
    Capability, Hook, Job, JobRun, Key, KeyRecord, Link, Principal, Recipe, Record, Secret, Tenant,
};
pub use crate::storage::database::{Cursor, Database, DatabaseCaps, Query, Row, Value};
pub use crate::storage::ir::{parse_filter, Agg, FilterCond, Op, SrvFilter};
pub use crate::storage::object_store::{
    BlobMeta, KeyInfo, ObjectStore, ObjectStoreCaps, PutInfo,
};
pub use crate::engine::{Notify, ServerlessEngine};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Single-tenant id. One Worker = one app: every key scope that used to
/// carry a per-board id now carries this constant. Stored rows no longer
/// carry any tenant field at all. Compile-time by design (there is no
/// `TENANT` env var to drift from).
pub const TENANT: &str = "singleton";

pub fn version() -> &'static str {
    VERSION
}
