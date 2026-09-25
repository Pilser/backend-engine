pub mod app_cache;
pub mod broker;
pub mod config;
pub mod db;
pub mod http_caller;
pub mod identity;
pub mod jobs;
pub mod observability;
pub mod resources;
pub mod store;
pub mod transport;

use engine::policy::{RateLimiter, RateLimits};
use engine::storage::memory::{InMemoryDatabase, InMemoryObjectStore};
use engine::ServerlessEngine;
use std::sync::{Arc, Mutex};

pub use engine;
pub struct Server {
    pub engine: Arc<Mutex<ServerlessEngine>>,
    pub broker: Arc<broker::InProcBroker>,
    pub limiter: Arc<Mutex<RateLimiter>>,
    pub obs: Arc<observability::Obs>,
    /// Cached board metadata (get_app) with a short TTL, so hot paths like
    /// static asset serving don't queue on the engine Mutex behind slow DB ops.
    pub app_cache: app_cache::AppCache,
    /// Shared object-store handle (same instance as inside the engine). Asset
    /// and file bytes are read/written directly through this — no engine lock.
    pub store: Arc<dyn engine::storage::object_store::ObjectStore>,
    /// Admission control for DB-heavy routes (Stage 4): bounds concurrent
    /// engine operations so a burst queues behind at most db_queue_cap
    /// waiters, then sheds load with 503 + Retry-After.
    pub db_permits: Arc<tokio::sync::Semaphore>,
    pub db_queue_cap: usize,
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Clone for Server {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            broker: Arc::clone(&self.broker),
            limiter: Arc::clone(&self.limiter),
            obs: Arc::clone(&self.obs),
            app_cache: self.app_cache.clone(),
            store: Arc::clone(&self.store),
            db_permits: Arc::clone(&self.db_permits),
            db_queue_cap: self.db_queue_cap,
        }
    }
}

impl Server {
    pub fn new(mut engine: ServerlessEngine) -> Self {
        let broker = Arc::new(broker::InProcBroker::new());
        engine.set_notifier(Some(broker_notifier(broker.clone())));
        let store = engine.shared_store();
        Self {
            engine: Arc::new(Mutex::new(engine)),
            broker,
            limiter: Arc::new(Mutex::new(RateLimiter::new())),
            obs: Arc::new(observability::Obs::new()),
            app_cache: app_cache::AppCache::new(),
            store,
            db_permits: Arc::new(tokio::sync::Semaphore::new(env_usize(
                "SRV_MAX_DB_CONCURRENCY",
                8,
            ))),
            db_queue_cap: env_usize("SRV_MAX_DB_QUEUE", 64),
        }
    }


    pub fn from_engine(engine: Arc<Mutex<ServerlessEngine>>) -> Self {
        let broker = Arc::new(broker::InProcBroker::new());
        if let Ok(mut e) = engine.lock() {
            e.set_notifier(Some(broker_notifier(broker.clone())));
        }
        let store = engine.lock().map(|e| e.shared_store()).ok();
        Self {
            engine,
            broker,
            limiter: Arc::new(Mutex::new(RateLimiter::new())),
            obs: Arc::new(observability::Obs::new()),
            app_cache: app_cache::AppCache::new(),
            store: store.expect("engine mutex poisoned at startup"),
            db_permits: Arc::new(tokio::sync::Semaphore::new(env_usize(
                "SRV_MAX_DB_CONCURRENCY",
                8,
            ))),
            db_queue_cap: env_usize("SRV_MAX_DB_QUEUE", 64),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(ServerlessEngine::new(
            Box::new(InMemoryDatabase::new()),
            Box::new(InMemoryObjectStore::new()),
        ))
    }

    pub fn spawn_background(&self) {
        // These loops are opt-in: each one periodically scans the whole store,
        // so idle deployments (no TTL tables, no cron jobs, no webhooks) should
        // leave them off. Enable via SRV_BG_TTL=1 / SRV_BG_JOBS=1 / SRV_BG_HOOKS=1.
        if env_flag("SRV_BG_TTL") {
            jobs::spawn_ttl_sweeper(self.engine.clone());
        }
        if env_flag("SRV_BG_JOBS") {
            jobs::spawn_scheduler(self.engine.clone());
        }
        if env_flag("SRV_BG_HOOKS") {
            jobs::spawn_webhook_worker(self.engine.clone());
        }
    }

    /// Board metadata with a 5s cache. Falls back to the engine on miss.
    /// NEVER use for authorization decisions that must be instantly
    /// consistent; asset/public-read checks accept the 5s window by design.
    pub fn get_app_cached(&self, board: &str) -> Option<engine::Board> {
        if let Some(b) = self.app_cache.get(board) {
            return Some(b);
        }
        let b = self
            .engine
            .lock()
            .ok()
            .and_then(|e| e.get_app(board).ok())
            .flatten()?;
        self.app_cache.put(board, b.clone());
        Some(b)
    }

    pub fn rate_limits_for(&self, board: &engine::Board) -> RateLimits {
        board
            .rate_json
            .as_ref()
            .map(|j| RateLimits::from_json(j))
            .unwrap_or_default()
    }
}

pub struct ServerBuilder {
    engine: Option<ServerlessEngine>,
}

impl ServerBuilder {
    pub fn new() -> Self {
        Self { engine: None }
    }

    pub fn engine(mut self, e: ServerlessEngine) -> Self {
        self.engine = Some(e);
        self
    }

    pub fn build(self) -> Server {
        match self.engine {
            Some(e) => Server::new(e),
            None => Server::with_defaults(),
        }
    }
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn engine_version() -> &'static str {
    engine::version()
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| v == "1").unwrap_or(false)
}

fn broker_notifier(broker: Arc<broker::InProcBroker>) -> engine::Notify {
    Arc::new(move |board, kind, seq, payload| {
        let event = serde_json::json!({
            "board": board,
            "type": format!("record.{kind}"),
            "seq": seq,
            "record": { "payload": payload },
        });
        let _ = broker.publish(&event.to_string());
    })
}

pub async fn serve(server: Server, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("serverless engine listening on http://{addr}");
    loop {
        let (stream, _) = listener.accept().await?;
        let srv = Arc::new(server.clone());
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| {
                let srv = srv.clone();
                async move { transport::rest::handle(srv, req).await }
            });
            let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            if let Err(err) = builder.serve_connection(io, service).await {
                tracing::warn!("connection error: {err}");
            }
        });
    }
}