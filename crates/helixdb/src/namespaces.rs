//! Label-namespace contract. Engine metadata lives in the same storage as user
//! data, so engine tables (`wb_*`) map to a RESERVED label namespace that user
//! labels can never collide with.

use crate::{Error, Result};

/// Prefix that reserves a label for engine metadata.
pub const RESERVED_PREFIX: &str = "__srv__";

/// Map an engine `wb_*` table name to its reserved Helix label.
///
/// `wb_records` is the engine's only "table of records" table (rows carry
/// `board_id` + `table`); it maps to `__srv__record`. Every other `wb_*` table
/// maps directly to `__srv__` + the name after the `wb_` prefix.
pub fn engine_label(table: &str) -> String {
    match table {
        "wb_records" => format!("{RESERVED_PREFIX}record"),
        t if t.starts_with("wb_") => format!("{RESERVED_PREFIX}{}", &t[3..]),
        // srv_board_index -> __srv__board_index
        t if t.starts_with("srv_") => format!("{RESERVED_PREFIX}{}", &t[4..]),
        other => format!("{RESERVED_PREFIX}{other}"),
    }
}

/// Sanitized user label. Rewrites any attempt to use the reserved prefix so
/// user-created tables can never shadow engine metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserLabel(pub String);

impl UserLabel {
    pub fn new(name: &str) -> Result<Self> {
        if name.is_empty() {
            return Err(Error::Namespace("table name cannot be empty".into()));
        }
        if name.starts_with(RESERVED_PREFIX) || name.starts_with("wb_") || name.starts_with("srv_") {
            return Err(Error::Namespace(format!(
                "table name '{name}' uses the reserved engine prefix"
            )));
        }
        if name.contains('/') || name.contains(' ') {
            return Err(Error::Namespace(format!(
                "table name '{name}' contains invalid characters"
            )));
        }
        Ok(UserLabel(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_engine_tables_to_reserved_labels() {
        assert_eq!(engine_label("wb_records"), "__srv__record");
        assert_eq!(engine_label("wb_tables"), "__srv__tables");
        assert_eq!(engine_label("wb_keys"), "__srv__keys");
        assert_eq!(engine_label("srv_board_index"), "__srv__board_index");
    }

    #[test]
    fn rejects_reserved_user_labels() {
        assert!(UserLabel::new("__srv__tables").is_err());
        assert!(UserLabel::new("wb_records").is_err());
        assert!(UserLabel::new("srv_board_index").is_err());
    }

    #[test]
    fn accepts_plain_labels() {
        assert_eq!(UserLabel::new("learners").unwrap().as_str(), "learners");
    }
}
