mod client;
mod registry;
mod render;
mod studio;


use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> anyhow::Result<String> {
    let daemon = daemon_url();
    let specs = registry::load();

    if args.is_empty() || args[0] == "--help" {
        return Ok(registry::help_index(&specs));
    }
    // `help [group] [verb]` delegates to the same dispatch as `--help`, but a
    // bare `help` still prints the index.
    if args[0] == "help" {
        let rest = &args[1..];
        if rest.is_empty() {
            return Ok(registry::help_index(&specs));
        }
        if rest.first().map(|s| s.as_str()) == Some("reference") {
            return Ok(registry::reference(&specs));
        }
        if rest.first().map(|s| s.as_str()) == Some("about") {
            return Ok(registry::about());
        }
        let group = rest[0].clone();
        if rest.len() == 1 {
            return Ok(registry::group_help(&specs, &group));
        }
        let verb = rest[1].clone();
        return Ok(registry::verb_help(&specs, &format!("{group}.{verb}")));
    }
    if args[0] == "about" {
        return Ok(registry::about());
    }
    if args[0] == "--version" || args[0] == "version" {
        return Ok(format!("srv {} — serverless engine CLI\n", server::engine_version()));
    }
    if args[0] == "daemon" {
        return cmd_daemon(&args[1..]);
    }
    if args[0] == "studio" {
        return studio::run(&daemon);
    }
    if args[0] == "reference" {
        return Ok(registry::reference(&specs));
    }

    let Some(group) = args.first() else {
        return Ok(registry::help_index(&specs));
    };
    // CLI-only verbs with host disk I/O: files export/import --dir <path>.
    // They still talk to the daemon (via the MCP tools) but read/write the
    // local filesystem for the folder.
    if group == "files" && args.len() >= 4 {
        let verb = args.get(1).map(|s| s.as_str()).unwrap_or("");
        let board = args.get(2).map(|s| s.as_str()).unwrap_or("");
        let dir = args_flag(&args[3..], "--dir");
        if let Some(dir) = dir {
            if verb == "export" || verb == "import" {
                let slug = args_flag(&args[3..], "--slug");
                let title = args_flag(&args[3..], "--title");
                let rt = tokio::runtime::Runtime::new().map_err(|e| anyhow::anyhow!("{e}"))?;
                let out = match verb {
                    "export" => rt.block_on(cmd_files_export(&daemon, board, &dir, slug.as_deref()))?,
                    _ => rt.block_on(cmd_files_import(&daemon, board, &dir, slug.as_deref(), title.as_deref()))?,
                };
                return Ok(out);
            }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("--help") {
        return Ok(registry::group_help(&specs, group));
    }
    let Some(verb) = args.get(1) else {
        return Ok(registry::group_help(&specs, group));
    };
    if verb == "--help" {
        return Ok(registry::group_help(&specs, group));
    }
    if args.get(2).map(|s| s.as_str()) == Some("--help") {
        let name = format!("{group}.{verb}");
        return Ok(registry::verb_help(&specs, &name));
    }

    let tail = &args[2..];
    match registry::parse_call(&specs, group, verb, tail) {
        Ok(call) => {
            let rt = tokio::runtime::Runtime::new().map_err(|e| anyhow::anyhow!("{e}"))?;
            let out = rt.block_on(client::call(&daemon, &call))?;
            Ok(render::render(&call, &out))
        }
        Err(e) => Ok(registry::error_text(&specs, group, verb, &e)),
    }
}

fn cmd_daemon(args: &[String]) -> anyhow::Result<String> {
    match args.first().map(|s| s.as_str()) {
        Some("start") => {
            let cfg = server::config::Config::load();
            let data_dir = args_flag(&args[1..], "--db")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| cfg.data_dir.clone());
            let backend = if cfg.backend == "memory" {
                "memory"
            } else {
                "helix"
            };
            let database: Box<dyn engine::storage::database::Database> =
                Box::new(engine::storage::cache::CachedDatabase::new(backend_impl(
                    backend, &data_dir, &cfg,
                )));
            let object_store: Box<dyn engine::storage::object_store::ObjectStore> =
                Box::new(engine::storage::cache::CachedObjectStore::new(Box::new(
                    server::store::fs::FsObjectStore::new(data_dir.join("objects")),
                )));
            let engine = std::sync::Arc::new(std::sync::Mutex::new(
                engine::ServerlessEngine::new(database, object_store),
            ));
            let caller = server::http_caller::ReqwestCaller::new()?;
            engine::http::install(Box::new(caller));
            let server = server::Server::from_engine(engine);
            let report_server = server.clone();
            let mcp = std::sync::Arc::new(
                mcp::McpServer::new(server.engine.clone()).with_reporter(std::sync::Arc::new(
                    move |board| server::resources::resource_report_cached(&report_server, board),
                )),
            );
            let addr = cfg.addr();
            println!(
                "daemon listening on http://{addr}/mcp (backend: {backend}, db: {})",
                data_dir.display()
            );
            let rt = tokio::runtime::Runtime::new().map_err(|e| anyhow::anyhow!("{e}"))?;
            rt.block_on(async {
                server.spawn_background();
            });
            rt.block_on(serve_combined(server, mcp, addr)).map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(String::new())
        }
        other => Ok(format!(
            "usage: srv daemon start [--db <dir>]\n\
             note: daemon hosts the engine + MCP over HTTP; other surfaces connect to it.\n\
             --db <dir>   data directory (default ./data; or $SRV_DATA_DIR / .env)\n\
             env: SRV_HOST SRV_PORT SRV_DATA_DIR SRV_DB SRV_HELIX_URL SRV_HTTP_TIMEOUT_MS (see .env.example)\n\
             bg loops: SRV_BG_TTL=1 SRV_BG_JOBS=1 SRV_BG_HOOKS=1 (default off)\n\
             got: '{:?}'\n",
            other
        )),
    }
}

/// `srv files export <board> --dir <path> [--slug <name>]` — download a dist
/// (main or sub-app) from the daemon and write the files to a local folder.
async fn cmd_files_export(
    daemon: &str,
    board: &str,
    dir: &str,
    slug: Option<&str>,
) -> anyhow::Result<String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("board".to_string(), serde_json::json!(board));
    if let Some(s) = slug {
        arguments.insert("slug".to_string(), serde_json::json!(s));
    }
    let call = client::ToolCall {
        name: "files.export".to_string(),
        arguments: serde_json::Value::Object(arguments),
    };
    let out = client::call(daemon, &call).await?;
    let result = out.get("result").cloned().unwrap_or(out.clone());
    let files = result
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("export response missing 'files'"))?;
    use base64::Engine as _;
    let root = std::path::Path::new(dir);
    std::fs::create_dir_all(root)?;
    let mut n = 0usize;
    for f in files {
        let path = f
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("file missing 'path'"))?;
        let b64 = f
            .get("content_base64")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("file '{path}' missing 'content_base64'"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| anyhow::anyhow!("bad base64 for '{path}': {e}"))?;
        let dest = root.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &bytes)?;
        n += 1;
    }
    Ok(format!("exported {n} files to {dir}\n"))
}

/// `srv files import <board> --dir <path> [--slug <name>]` — upload every file
/// in a local folder to the daemon (main dist or a sub-app), replacing same-path
/// files.
async fn cmd_files_import(
    daemon: &str,
    board: &str,
    dir: &str,
    slug: Option<&str>,
    title: Option<&str>,
) -> anyhow::Result<String> {
    use base64::Engine as _;
    let root = std::path::Path::new(dir);
    if !root.is_dir() {
        anyhow::bail!("--dir must be an existing directory: {dir}");
    }
    let mut files: Vec<serde_json::Value> = Vec::new();
    fn walk(p: &std::path::Path, root: &std::path::Path, out: &mut Vec<serde_json::Value>) -> anyhow::Result<()> {
        for e in std::fs::read_dir(p)? {
            let e = e?;
            let path = e.path();
            if path.is_dir() {
                walk(&path, root, out)?;
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(&path)?;
                out.push(serde_json::json!({
                    "path": rel,
                    "content_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                }));
            }
        }
        Ok(())
    }
    walk(root, root, &mut files)?;
    let mut arguments = serde_json::Map::new();
    arguments.insert("board".to_string(), serde_json::json!(board));
    arguments.insert("files".to_string(), serde_json::Value::Array(files));
    if let Some(s) = slug {
        arguments.insert("slug".to_string(), serde_json::json!(s));
    }
    if let Some(t) = title {
        arguments.insert("title".to_string(), serde_json::json!(t));
    }
    let call = client::ToolCall {
        name: "files.import".to_string(),
        arguments: serde_json::Value::Object(arguments),
    };
    let out = client::call(daemon, &call).await?;
    let result = out.get("result").cloned().unwrap_or(out.clone());
    let imported = result.get("imported").and_then(|i| i.as_u64()).unwrap_or(0);
    Ok(format!("imported {imported} files from {dir}\n"))
}

fn backend_impl(
    backend: &str,
    _data_dir: &std::path::Path,
    cfg: &server::config::Config,
) -> Box<dyn engine::storage::database::Database> {
    match backend {
        "memory" => Box::new(engine::storage::memory::InMemoryDatabase::new()),
        _ => match server::db::helix::HelixDatabase::with_timeout(&cfg.helix_url, cfg.http_timeout_ms) {
            Ok(db) => Box::new(db),
            Err(e) => {
                eprintln!("helix backend init failed: {e}");
                std::process::exit(1);
            }
        },
    }
}

fn args_flag(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
    }
    None
}

async fn serve_combined(
    server: server::Server,
    mcp: std::sync::Arc<mcp::McpServer>,
    addr: std::net::SocketAddr,
) -> anyhow::Result<()> {
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("serverless engine + mcp listening on http://{addr}");
    loop {
        let (stream, _) = listener.accept().await?;
        let srv = std::sync::Arc::new(server.clone());
        let mcp = mcp.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| {
                let srv = srv.clone();
                let mcp = mcp.clone();
                let path = req.uri().path().to_string();
                async move {
                    if path == "/mcp" {
                        Ok::<_, std::io::Error>(mcp::transport::handle_hyper(mcp, req).await)
                    } else if server::transport::ws::is_ws(&req) {
                        Ok::<_, std::io::Error>(server::transport::ws::ws_upgrade(srv.clone(), req))
                    } else {
                        server::transport::rest::handle(srv, req)
                            .await
                            .map_err(|never| match never {})
                    }
                }
            });
            let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            if let Err(err) = builder.serve_connection_with_upgrades(io, service).await {
                tracing::warn!("connection error: {err}");
            }
        });
    }
}

fn daemon_url() -> String {
    std::env::var("SERVERLESS_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:7070".to_string())
}