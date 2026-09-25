//! Row mapping: engine `Row { key, data }` <-> Helix node property documents.
//!
//! Engine rows carry `data` = `{ board_id, table, seq, payload, created_at,
//! writer }` (for `wb_records`) or the raw metadata JSON for other tables. In
//! Helix we store the full engine JSON as node properties, so rows round-trip
//! losslessly; the `$id` is the Helix node id, and the engine key is stored in
//! `_srv_key`.

use serde_json::{json, Value as Json};

/// Property that stores the engine `Key` (its serialized form) on every node.
pub const KEY_PROP: &str = "_srv_key";

/// Encode an engine key into its Helix property value.
pub fn key_value(key: &crate::row::Key) -> Json {
    match key {
        crate::row::Key::Int(v) => json!({ "i64": v }),
        crate::row::Key::Text(s) => json!({ "string": s }),
    }
}

/// The engine `Key` type (mirrored here so the crate has no engine dependency).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    Int(i64),
    Text(String),
}

/// A row as stored in Helix: node properties + `$id`.
#[derive(Debug, Clone)]
pub struct Row {
    pub key: Key,
    pub data: Json,
}

impl Row {
    pub fn new(key: Key, data: Json) -> Self {
        Self { key, data }
    }

    /// Properties to write for this row (data + `_srv_key`).
    pub fn helix_props(&self) -> Json {
        let mut obj = self.data.as_object().cloned().unwrap_or_default();
        obj.insert(KEY_PROP.to_string(), key_value(&self.key));
        Json::Object(obj)
    }
}
