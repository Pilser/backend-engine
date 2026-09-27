use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Key {
    Int(i64),
    Text(String),
}

impl Key {
    pub fn int(v: i64) -> Self {
        Key::Int(v)
    }

    pub fn text(v: impl Into<String>) -> Self {
        Key::Text(v.into())
    }
}

/// Single-tenant app config. One row per deployment (see `tenant_config`).
/// Board-level leftovers (`owner_key`, per-app schema/computed/…, board-wide
/// TTL) were deleted in the single-tenant collapse: schema/computed/validate/
/// redact/TTL live per table, and there is no owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub title: String,
    #[serde(default)]
    pub public_reads: bool,
    #[serde(default)]
    pub rate_json: Option<Json>,
    #[serde(default)]
    pub audit: bool,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableConfig {
    pub table: String,
    #[serde(default)]
    pub schema_json: Option<Json>,
    #[serde(default)]
    pub unique_key: Option<String>,
    #[serde(default)]
    pub computed_json: Option<Json>,
    #[serde(default)]
    pub validate_json: Option<Json>,
    #[serde(default)]
    pub redact_json: Option<Json>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
    #[serde(default)]
    pub ttl_field: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Per-table read policy (S1: P0 proposals). None = inherit the tenant
    /// `public_reads` flag. Some(true) = anonymous reads allowed on this
    /// table's read endpoints; Some(false) = anonymous reads denied even
    /// when the tenant is public.
    #[serde(default)]
    pub public_read: Option<bool>,
    /// Write-only (append-only) table: reads need admin/owner, but submit
    /// stays open (anonymous included) — the inbox shape (contact forms,
    /// applications, reports).
    #[serde(default)]
    pub write_only: Option<bool>,
    /// Standalone anonymous-submit allowlist (S2: P1): submit open on this
    /// table while reads follow the read policy. `write_only` implies it.
    #[serde(default)]
    pub allow_anon_submit: Option<bool>,
    /// Keep-last-N trim (S3: P2 abuse controls): after each sweep, oldest
    /// rows beyond this count are dropped. Spam/inbox tables stay bounded
    /// without manual cleanup.
    #[serde(default)]
    pub max_rows: Option<i64>,
}

impl TableConfig {
    /// Anonymous-read rule (pure, unit-tested): write-only tables never
    /// open; otherwise the table override wins, else the tenant default.
    pub fn anon_read_open(&self, tenant_public: bool) -> bool {
        if self.write_only.unwrap_or(false) {
            return false;
        }
        self.public_read.unwrap_or(tenant_public)
    }

    /// Anonymous-submit rule: open on write-only tables or with the
    /// standalone `allow_anon_submit` knob.
    pub fn anon_submit_open(&self) -> bool {
        self.write_only.unwrap_or(false) || self.allow_anon_submit.unwrap_or(false)
    }

    /// P2 audit answer (S3): what ANONYMOUS callers can and cannot do on
    /// this table, given the tenant default. Read the answer instead of
    /// scraping: misconfigurations become visible before attackers do.
    pub fn access_audit(&self, tenant_public: bool) -> Json {
        let reads = ["list", "get", "query", "search", "aggregate"];
        let mut anon_can: Vec<&str> = Vec::new();
        let mut anon_cannot: Vec<&str> = Vec::new();
        if self.anon_read_open(tenant_public) {
            anon_can.extend(reads);
        } else {
            anon_cannot.extend(reads);
        }
        if self.anon_submit_open() {
            anon_can.push("submit");
        } else {
            anon_cannot.push("submit");
        }
        serde_json::json!({ "anon_can": anon_can, "anon_cannot": anon_cannot })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub seq: i64,
    pub payload: Json,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub writer: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRecord {
    pub bucket: String,
    pub key_hash: String,
    #[serde(default)]
    pub salt: String,
    pub role: String,
    #[serde(default)]
    pub writer: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
    /// Table scope (S2: P1 proposals). None = all tables; Some(list) =
    /// only these tables on table-data routes. Stored at issuance.
    #[serde(default)]
    pub tables: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    pub when_json: Json,
    #[serde(default)]
    pub match_json: Option<Json>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub dedup_on: Option<String>,
    #[serde(default)]
    pub actions_json: Option<Json>,
    #[serde(default)]
    pub table: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secret {
    pub name: String,
    pub value_encrypted: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub name: String,
    pub schedule: String,
    pub action: Json,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_message: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRun {
    pub job_name: String,
    pub triggered_at: String,
    pub duration_ms: i64,
    pub status: String,
    pub message: String,
    #[serde(default)]
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub child_board: String,
    pub child_table: String,
    pub parent_board: String,
    pub parent_table: String,
    pub from_key: String,
    pub parent_key: String,
}

/// A named sub-app (standalone dist folder) hosted under a board's asset
/// namespace. `index` is the relative asset path of its root html (default
/// "{slug}/index.html").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubApp {
    pub slug: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub index: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub role: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub writer: Option<String>,
    /// Table scope carried from scoped API keys (None = all tables).
    #[serde(default)]
    pub tables: Option<Vec<String>>,
}

impl Principal {
    /// Table-data access for scoped keys: unscoped principals (users,
    /// sessions, unscoped keys) pass; scoped keys must list the table.
    pub fn allows_table(&self, table: &str) -> bool {
        self.tables
            .as_ref()
            .map(|t| t.iter().any(|x| x == table))
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub url: String,
    #[serde(default)]
    pub secret: Option<String>,
}
