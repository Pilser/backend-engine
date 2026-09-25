use serde_json::{Map, Value as Json};

/// Parse a dump into a list of record payloads.
///
/// - `"json"`: accepts either a JSON array `[...]` or newline-delimited JSON
///   (one object per line, blank lines ignored).
/// - `"csv"`: a header row followed by data rows. Each row becomes an object
///   keyed by the header. Cells are parsed as JSON where possible (numbers,
///   booleans, null) and kept as strings otherwise.
pub fn parse(format: &str, data: &str, separator: char) -> anyhow::Result<Vec<Json>> {
    match format {
        "json" => parse_json(data),
        "csv" => parse_csv(data, separator),
        other => anyhow::bail!("unknown import format '{other}' (json or csv)"),
    }
}

fn parse_json(data: &str) -> anyhow::Result<Vec<Json>> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        let v: Json = serde_json::from_str(trimmed)?;
        v.as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("json import must be an array of objects"))
    } else {
        let mut out = Vec::new();
        for line in trimmed.lines() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            out.push(serde_json::from_str(l)?);
        }
        Ok(out)
    }
}

fn parse_csv(data: &str, sep: char) -> anyhow::Result<Vec<Json>> {
    let rows = csv_rows(data, sep);
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let headers = rows[0].clone();
    if headers.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for row in rows.into_iter().skip(1) {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let mut obj = Map::new();
        for (i, h) in headers.iter().enumerate() {
            if h.is_empty() {
                continue;
            }
            let cell = row.get(i).cloned().unwrap_or_default();
            obj.insert(h.clone(), typed(&cell));
        }
        out.push(Json::Object(obj));
    }
    Ok(out)
}

fn csv_rows(data: &str, sep: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut cur = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = data.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => in_quotes = true,
                '\n' => {
                    cur.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut cur));
                }
                '\r' => {}
                c if c == sep => {
                    cur.push(std::mem::take(&mut field));
                }
                c => field.push(c),
            }
        }
    }
    if in_quotes || !field.is_empty() || !cur.is_empty() {
        cur.push(std::mem::take(&mut field));
        rows.push(cur);
    }
    rows
}

/// Parse a CSV cell as JSON when it clearly is one (numbers, booleans, null);
/// otherwise keep it as a string.
fn typed(s: &str) -> Json {
    if s.trim().is_empty() {
        return Json::String(String::new());
    }
    match serde_json::from_str::<Json>(s) {
        Ok(v) if !v.is_string() => v,
        Ok(Json::String(_)) => Json::String(s.to_string()),
        _ => Json::String(s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_json_array() {
        let v = parse("json", r#"[{"a":1},{"a":2}]"#, ',').unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["a"], json!(1));
    }

    #[test]
    fn parses_jsonl() {
        let v = parse("json", "{\"a\":1}\n{\"a\":2}\n", ',').unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn parses_csv_with_typing_and_quotes() {
        let csv = "name,score,ok\nada,\"85.5\",true\nzed,90,false\n";
        let v = parse("csv", csv, ',').unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["name"], json!("ada"));
        assert_eq!(v[0]["score"], json!(85.5));
        assert_eq!(v[0]["ok"], json!(true));
    }
}
