//! Board id -> Helix tenantId. Every board is one tenant; every request for a
//! board is scoped to its tenant so no two boards' data, schema, or recipes
//! mix. Tenant-scoped queries always anchor with `n_with_label` filtered by the
//! `tenantId` property.

/// Property name that carries the tenant scope on every node/edge.
pub const TENANT_PROP: &str = "tenantId";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tenant(pub String);

impl Tenant {
    /// Map a board id to a Helix tenantId. Board ids are already opaque and
    /// unique (`b_<base36-nanos>`); reuse them directly so the mapping is
    /// injective and reversible.
    pub fn from_board(board_id: &str) -> Self {
        Tenant(board_id.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_board_ids_injectively() {
        assert_eq!(Tenant::from_board("b_abc").0, "b_abc");
        assert_ne!(Tenant::from_board("b_abc"), Tenant::from_board("b_abd"));
    }
}
