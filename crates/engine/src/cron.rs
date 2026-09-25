use chrono::{Datelike, Duration, NaiveDateTime, Timelike};
use std::collections::BTreeSet;

pub fn fmt_iso(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn every_seconds(rest: &str) -> anyhow::Result<i64> {
    let mut tokens = rest.split_whitespace();
    let first = tokens.next().ok_or_else(|| anyhow::anyhow!("@every requires a duration like 30s, 5m, 2h"))?;
    let (num, unit) = match tokens.next() {
        Some(u) => (first, u),
        None => {
            let idx = first.find(|c: char| !c.is_ascii_digit()).unwrap_or(first.len());
            if idx == 0 {
                anyhow::bail!("@every duration must start with a number");
            }
            let (n, u) = first.split_at(idx);
            (n, u)
        }
    };
    let n: i64 = num.parse().map_err(|_| anyhow::anyhow!("bad duration {num}"))?;
    let secs = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => n,
        "m" | "min" | "mins" | "minute" | "minutes" => n * 60,
        "h" | "hr" | "hour" | "hours" => n * 3600,
        "d" | "day" | "days" => n * 86400,
        other => anyhow::bail!("unknown duration unit '{other}'"),
    };
    Ok(secs)
}

#[derive(Clone)]
struct Field {
    values: BTreeSet<i64>,
    star: bool,
}

impl Field {
    fn matches(&self, v: i64) -> bool {
        self.values.contains(&v)
    }

    fn is_star(&self) -> bool {
        self.star
    }
}

fn parse_part(part: &str, lo: i64, hi: i64) -> anyhow::Result<BTreeSet<i64>> {
    let mut out = BTreeSet::new();
    for piece in part.split(',') {
        if piece.is_empty() {
            continue;
        }
        let (range, step) = match piece.split_once('/') {
            Some((r, st)) => (r, st.parse::<i64>().map_err(|_| anyhow::anyhow!("bad step in {piece}"))?),
            None => (piece, 1),
        };
        if step < 1 {
            anyhow::bail!("step must be >= 1 in {piece}");
        }
        if range == "*" {
            let mut v = lo;
            while v <= hi {
                out.insert(v);
                v += step;
            }
        } else if let Some((a, b)) = range.split_once('-') {
            let a: i64 = a.parse().map_err(|_| anyhow::anyhow!("bad range in {piece}"))?;
            let b: i64 = b.parse().map_err(|_| anyhow::anyhow!("bad range in {piece}"))?;
            if a < lo || b > hi || a > b {
                anyhow::bail!("range {range} out of bounds [{lo},{hi}]");
            }
            let mut v = a;
            while v <= b {
                out.insert(v);
                v += step;
            }
        } else {
            let v: i64 = piece.parse().map_err(|_| anyhow::anyhow!("bad value in {piece}"))?;
            if v < lo || v > hi {
                anyhow::bail!("value {v} out of bounds [{lo},{hi}]");
            }
            out.insert(v);
        }
    }
    if out.is_empty() {
        anyhow::bail!("empty field {part}");
    }
    Ok(out)
}

fn parse_field(s: &str, lo: i64, hi: i64) -> anyhow::Result<Field> {
    let values = parse_part(s, lo, hi)?;
    Ok(Field { star: s == "*", values })
}

fn weekday(dt: NaiveDateTime) -> i64 {
    let dow = dt.weekday().num_days_from_sunday();
    if dow == 0 {
        7
    } else {
        dow as i64
    }
}

fn day_matches(dom: &Field, dow: &Field, day: i64, wd: i64) -> bool {
    let d = dom.matches(day);
    let w = dow.matches(wd);
    if dom.is_star() || dow.is_star() {
        d && w
    } else {
        d || w
    }
}

pub fn next_run(schedule: &str, after: NaiveDateTime) -> Option<NaiveDateTime> {
    let s = schedule.trim();
    if let Some(rest) = s.strip_prefix("@every") {
        let secs = every_seconds(rest.trim()).ok()?;
        if secs <= 0 {
            return None;
        }
        return Some(after + Duration::seconds(secs));
    }
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    let minute = parse_field(parts[0], 0, 59).ok()?;
    let hour = parse_field(parts[1], 0, 23).ok()?;
    let dom = parse_field(parts[2], 1, 31).ok()?;
    let month = parse_field(parts[3], 1, 12).ok()?;
    let mut dow = parse_field(parts[4], 0, 7).ok()?;
    if dow.values.contains(&0) {
        dow.values.insert(7);
    }

    let start = match after.date().and_hms_opt(after.hour(), after.minute(), 0) {
        Some(trunc) => trunc + Duration::minutes(1),
        None => after + Duration::minutes(1),
    };
    let limit = after + Duration::days(366 * 3);
    let mut dt = start;
    while dt <= limit {
        if month.matches(dt.month() as i64)
            && day_matches(&dom, &dow, dt.day() as i64, weekday(dt))
            && hour.matches(dt.hour() as i64)
            && minute.matches(dt.minute() as i64)
        {
            return Some(dt);
        }
        dt += Duration::seconds(60);
    }
    None
}