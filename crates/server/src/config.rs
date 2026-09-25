use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub backend: String,
    pub helix_url: String,
    pub http_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7070,
            data_dir: PathBuf::from("data"),
            backend: "helix".to_string(),
            helix_url: "http://127.0.0.1:7979".to_string(),
            http_timeout_ms: 15_000,
        }
    }
}

impl Config {
    /// Load configuration from an optional `.env` file (in the current working
    /// directory) merged with the process environment. Real environment
    /// variables take precedence over the `.env` file; missing values fall back
    /// to defaults.
    pub fn load() -> Self {
        let dotenv = load_dotenv(".env");
        Self {
            host: get(&dotenv, "SRV_HOST", "127.0.0.1"),
            port: get(&dotenv, "SRV_PORT", "7070")
                .parse()
                .unwrap_or(7070),
            data_dir: PathBuf::from(get(&dotenv, "SRV_DATA_DIR", "data")),
            backend: get(&dotenv, "SRV_DB", "helix"),
            helix_url: get(&dotenv, "SRV_HELIX_URL", "http://127.0.0.1:7979"),
            http_timeout_ms: get(&dotenv, "SRV_HTTP_TIMEOUT_MS", "15000")
                .parse()
                .unwrap_or(15_000),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        format!("{}:{}", self.host, self.port)
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:7070".parse().unwrap())
    }
}

fn get(dotenv: &HashMap<String, String>, key: &str, default: &str) -> String {
    if let Ok(v) = std::env::var(key) {
        return v;
    }
    dotenv.get(key).cloned().unwrap_or_else(|| default.to_string())
}

/// Minimal `.env` parser: `KEY=VALUE` lines, `#` comments, blank lines ignored,
/// `export ` prefix stripped, values optionally quoted. Real environment
/// variables are left untouched (they win over the file).
///
/// Background-loop toggles (all default OFF — each loop periodically scans the
/// whole store, so leave them off unless the feature is actually used):
///   SRV_BG_TTL=1   enable the TTL sweeper (needed only with TTL tables)
///   SRV_BG_JOBS=1  enable the cron/job scheduler (needed only with cron jobs)
///   SRV_BG_HOOKS=1 enable the webhook delivery worker (needed only with webhooks)
fn load_dotenv(path: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return map;
    };
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((k, mut v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_string();
        let v = v.trim();
        let v = v.trim_matches('"').trim_matches('\'').to_string();
        if !k.is_empty() {
            map.insert(k, v);
        }
    }
    map
}
