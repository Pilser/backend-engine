use crate::resources::ResourceCache;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub struct Obs {
    requests: AtomicU64,
    in_flight: AtomicU64,
    cpu: Mutex<Option<(u64, Instant)>>,
    pub resource_cache: ResourceCache,
}

impl Obs {
    pub fn new() -> Self {
        Self {
            requests: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            cpu: Mutex::new(None),
            resource_cache: ResourceCache::new(),
        }
    }

    pub fn bump(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn enter(&self) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    pub fn leave(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn total(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    pub fn cpu_percent(&self) -> f64 {
        let (ticks, at) = match proc_ticks() {
            Some(v) => v,
            None => return 0.0,
        };
        let mut last = self.cpu.lock().unwrap();
        let pct = match last.as_ref() {
            Some((prev_ticks, prev_at)) => {
                let dt = at.duration_since(*prev_at).as_secs_f64();
                if dt <= 0.0 {
                    0.0
                } else {
                    let d = ticks.saturating_sub(*prev_ticks) as f64;
                    (d / 100.0) / dt * 100.0
                }
            }
            None => 0.0,
        };
        *last = Some((ticks, at));
        pct
    }

    pub fn mem_kb(&self) -> u64 {
        proc_rss_kb().unwrap_or(0)
    }

    pub fn stats(&self) -> serde_json::Value {
        json!({
            "requests": self.total(),
            "in_flight": self.in_flight(),
            "cpu_percent": (self.cpu_percent() * 100.0).round() / 100.0,
            "mem_kb": self.mem_kb(),
        })
    }
}

fn proc_ticks() -> Option<(u64, Instant)> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    let close = s.rfind(')')?;
    let rest = &s[close + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    if f.len() < 13 {
        return None;
    }
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some((utime + stime, Instant::now()))
}

fn proc_rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            let kb: u64 = v.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb);
        }
    }
    None
}
