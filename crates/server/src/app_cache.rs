use engine::Board;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(5);

struct Entry {
    board: Board,
    at: Instant,
}

/// Short-TTL cache of `get_app` results so hot paths (static asset serving,
/// authz checks) don't queue on the engine Mutex behind slow Helix operations.
/// A board create/update/delete invalidates via `invalidate`.
#[derive(Clone)]
pub struct AppCache {
    inner: Arc<std::sync::Mutex<HashMap<String, Entry>>>,
}

impl AppCache {
    pub fn new() -> Self {
        Self { inner: Arc::new(std::sync::Mutex::new(HashMap::new())) }
    }

    pub fn get(&self, board: &str) -> Option<Board> {
        let mut m = self.inner.lock().ok()?;
        match m.get(board) {
            Some(e) if e.at.elapsed() < TTL => Some(e.board.clone()),
            Some(_) => {
                m.remove(board);
                None
            }
            None => None,
        }
    }

    pub fn put(&self, board: &str, b: Board) {
        if let Ok(mut m) = self.inner.lock() {
            m.insert(board.to_string(), Entry { board: b, at: Instant::now() });
        }
    }

    pub fn invalidate(&self, board: &str) {
        if let Ok(mut m) = self.inner.lock() {
            m.remove(board);
        }
    }
}
