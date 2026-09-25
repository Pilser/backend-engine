//! Minimal HelixDB v3 client: raw `POST /v2/query` JSON transport with a
//! typed envelope builder and the multi-tenant `__srv__` namespace contract.

pub mod client;
pub mod namespaces;
pub mod predicate;
pub mod request;
pub mod row;
pub mod tenant;

pub use client::{Client, Response};
pub use namespaces::{engine_label, RESERVED_PREFIX, UserLabel};
pub use predicate::Predicate;
pub use row::{Key, Row};
pub use tenant::Tenant;

/// Top-level error type for the HelixDB crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("helix returned status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("namespace violation: {0}")]
    Namespace(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
