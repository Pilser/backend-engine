//! CLI-over-MCP: the single `manage_serverless_engine` tool.
//!
//! Grammar: `[serverless] <group> <verb> [positionals…] [--flag value]…`
//! A trailing JSON token feeds verbs that take a body (`records submit`,
//! `records bulk/update/patch`, `files put`). `--help` at every level:
//! bare, per group, per verb. Types and help come straight from the
//! `engine::registry` specs, so the grammar can never drift from the tools.

use engine::registry::{ArgType, CommandSpec, FlagType};
use engine::{model::Principal, ServerlessEngine};
use serde_json::{json, Value as Json};
use std::collections::HashMap;

pub const TOOL_NAME: &str = "manage_serverless_engine";

/// Split input into tokens honoring single/double quotes and backslashes.
fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut quote = None;
    let mut esc = false;
    for ch in input.chars() {
        if esc {
            cur.push(ch);
            esc = false;
            in_tok = true;
            continue;
        }
        match (quote, ch) {
            (_, '\\') if quote.is_some() => esc = true,
            (None, '\'') | (None, '"') => {
                quote = Some(ch);
                in_tok = true;
            }
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if in_tok {
                    out.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
            }
            _ => {
                cur.push(ch);
                in_tok = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote — wrap values in '...' or \"...\"".to_string());
    }
    if in_tok {
        out.push(cur);
    }
    Ok(out)
}

fn type_name_arg(t: &ArgType) -> &'static str {
    match t {
        ArgType::Int | ArgType::Seq => "int",
        ArgType::Json => "json",
        _ => "string",
    }
}

fn type_name_flag(t: &FlagType) -> &'static str {
    match t {
        FlagType::Bool => "bool",
        FlagType::Int => "int",
        FlagType::Json => "json",
        _ => "string",
    }
}

fn specs() -> Vec<CommandSpec> {
    engine::registry::registry()
}

fn find_spec(group: &str, verb: &str) -> Option<CommandSpec> {
    specs().into_iter().find(|s| s.group == group && s.verb == verb)
}

fn groups() -> Vec<String> {
    let mut g: Vec<String> = specs().iter().map(|s| s.group.clone()).collect();
    g.sort();
    g.dedup();
    g
}

fn verbs_of(group: &str) -> Vec<CommandSpec> {
    let mut v: Vec<CommandSpec> =
        specs().into_iter().filter(|s| s.group == group).collect();
    v.sort_by(|a, b| a.verb.cmp(&b.verb));
    v
}

pub fn help_overview() -> String {
    let mut out = String::from(
        "manage_serverless_engine — run the whole backend with one tool\n\
         usage: <group> <verb> [args] [--flags]   (a leading \"serverless\" is optional)\n\
         \n         --help | <group> --help | <group> <verb> --help\n\
         terminal without MCP? GET /mcp?command=<...> or POST /mcp {\"command\":\"...\"}\n\
         \ngroups:\n",
    );
    for g in groups() {
        let verbs: Vec<String> = verbs_of(&g).iter().map(|s| s.verb.clone()).collect();
        out.push_str(&format!("  {g:<10} {}\n", verbs.join(", ")));
    }
    out
}

pub fn help_group(group: &str) -> Result<String, String> {
    let verbs = verbs_of(group);
    if verbs.is_empty() {
        return Err(format!("unknown group '{group}'. groups: {}", groups().join(", ")));
    }
    let mut out = format!("group {group} — pick a verb (<group> <verb> --help for detail):\n");
    for s in verbs {
        out.push_str(&format!("  {:<14} {}\n", s.verb, s.summary));
    }
    Ok(out)
}

pub fn help_verb(spec: &CommandSpec) -> String {
    let mut usage = format!("{} {}", spec.group, spec.verb);
    for a in &spec.positional {
        if a.required {
            usage.push_str(&format!(" <{}>", a.name));
        } else {
            usage.push_str(&format!(" [{}]", a.name));
        }
    }
    let mut out = format!("{}\nusage: {usage}\n\n{}\n", spec.summary, spec.description);
    if !spec.positional.is_empty() {
        out.push_str("\nargs:\n");
        for a in &spec.positional {
            out.push_str(&format!(
                "  {:<12} {:<6} {} {}\n",
                a.name,
                type_name_arg(&a.r#type),
                if a.required { "(required)" } else { "(optional)" },
                a.help
            ));
        }
    }
    if !spec.flags.is_empty() {
        out.push_str("\nflags:\n");
        for f in &spec.flags {
            let mut line = format!("  --{:<11} {:<6} {}", f.name, type_name_flag(&f.r#type), f.help);
            if let Some(d) = &f.default {
                line.push_str(&format!(" (default {d})"));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !spec.examples.is_empty() {
        out.push_str("\nexamples:\n");
        for e in &spec.examples {
            out.push_str(&format!("  {}\n    {}\n", e.args, e.description));
        }
    }
    if !spec.see_also.is_empty() {
        out.push_str(&format!("\nsee also: {}\n", spec.see_also.join(", ")));
    }
    if let Some(d) = &spec.danger_notes {
        out.push_str(&format!("\n! {d}\n"));
    }
    out
}

fn coerce_arg(t: &ArgType, name: &str, raw: &str) -> Result<Json, String> {
    match t {
        ArgType::Int | ArgType::Seq => raw
            .parse::<i64>()
            .map(Json::from)
            .map_err(|_| format!("argument '{name}' expects an integer, got '{raw}'")),
        ArgType::Json => serde_json::from_str(raw)
            .map_err(|_| format!("argument '{name}' expects JSON, got '{raw}'")),
        _ => Ok(Json::String(raw.to_string())),
    }
}

fn coerce_flag(t: &FlagType, name: &str, raw: Option<&str>) -> Result<Json, String> {
    match t {
        FlagType::Bool => match raw {
            None => Ok(Json::Bool(true)),
            Some(v) => match v.to_lowercase().as_str() {
                "true" | "1" | "yes" => Ok(Json::Bool(true)),
                "false" | "0" | "no" => Ok(Json::Bool(false)),
                _ => Err(format!("flag --{name} expects true/false, got '{v}'")),
            },
        },
        FlagType::Int => {
            let v = raw.ok_or_else(|| format!("flag --{name} needs a value"))?;
            v.parse::<i64>()
                .map(Json::from)
                .map_err(|_| format!("flag --{name} expects an integer, got '{v}'"))
        }
        FlagType::Json => {
            let v = raw.ok_or_else(|| format!("flag --{name} needs a value"))?;
            serde_json::from_str(v)
                .map_err(|_| format!("flag --{name} expects JSON, got '{v}'"))
        }
        _ => {
            let v = raw.ok_or_else(|| format!("flag --{name} needs a value"))?;
            Ok(Json::String(v.to_string()))
        }
    }
}

/// Trailing-body verbs: one extra JSON (or raw, for files.put) token feeds
/// the tool's body argument, e.g. `records submit notes '{"a":1}'`.
fn body_key(group: &str, verb: &str) -> Option<(&'static str, bool)> {
    match (group, verb) {
        ("records", "submit") => Some(("payload", true)),
        ("records", "bulk") => Some(("records", true)),
        ("records", "update") => Some(("payload", true)),
        ("records", "patch") => Some(("patch", true)),
        ("files", "put") => Some(("content", false)),
        ("plugins", "install") => Some(("manifest", true)),
        ("site", "add") => Some(("route", true)),
        _ => None,
    }
}

/// Terminal-facing outcome: a value, or help text (success, not an error).
pub enum Outcome {
    Value(Json),
    Help(String),
}

fn split(input: &str) -> Result<(Vec<String>, HashMap<String, Vec<Option<String>>>), String> {
    let mut toks = tokenize(input)?;
    if !toks.is_empty() && toks[0] == "serverless" {
        toks.remove(0);
    }
    // Split flags (with optional =values) from positionals, in order.
    let mut words: Vec<String> = Vec::new();
    let mut flags: HashMap<String, Vec<Option<String>>> = HashMap::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t == "--help" {
            flags.entry("help".to_string()).or_default().push(None);
        } else if let Some(rest) = t.strip_prefix("--") {
            if rest.is_empty() {
                return Err("bare '--' is not valid; use --flag value".to_string());
            }
            if let Some((k, v)) = rest.split_once('=') {
                flags.entry(k.to_string()).or_default().push(Some(v.to_string()));
            } else {
                let v = if i + 1 < toks.len() && !toks[i + 1].starts_with("--") {
                    i += 1;
                    Some(toks[i].clone())
                } else {
                    None
                };
                flags.entry(rest.to_string()).or_default().push(v);
            }
        } else {
            words.push(t.clone());
        }
        i += 1;
    }
    Ok((words, flags))
}

/// Parse + validate a command string against the registry. Returns the
/// matched spec and the ready-to-dispatch arguments object.
fn prepare(input: &str) -> Result<(CommandSpec, Json), String> {
    let (words, flags) = split(input)?;
    if words.is_empty() {
        return Err(help_overview());
    }
    let group = words[0].clone();
    if words.len() == 1 {
        return Err(help_group(&group)?);
    }
    let verb = words[1].clone();
    let Some(spec) = find_spec(&group, &verb) else {
        if verbs_of(&group).is_empty() {
            return Err(format!(
                "unknown group '{group}'. groups: {}\n({TOOL_NAME} --help lists everything)",
                groups().join(", ")
            ));
        }
        let valid: Vec<String> = verbs_of(&group).iter().map(|s| s.verb.clone()).collect();
        return Err(format!(
            "unknown verb '{verb}' for group '{group}'. valid: {}\n({group} --help for detail)",
            valid.join(", ")
        ));
    };
    if flags.contains_key("help") {
        return Err(help_verb(&spec));
    }

    let mut positionals = words[2..].to_vec();
    // Trailing body token for body-taking verbs.
    let mut body: Option<(String, Json)> = None;
    if positionals.len() > spec.positional.len() {
        match body_key(&group, &verb) {
            Some((key, as_json)) if positionals.len() == spec.positional.len() + 1 => {
                let raw = positionals.pop().unwrap();
                let value = if as_json {
                    serde_json::from_str(&raw).map_err(|_| {
                        format!("body for {group}.{verb} must be JSON — quote it: '{{\"a\":1}}'")
                    })?
                } else {
                    Json::String(raw)
                };
                body = Some((key.to_string(), value));
            }
            _ => {
                return Err(format!(
                    "too many arguments for {group}.{verb} (got {}, want {}).\nusage: {} {}",
                    positionals.len(),
                    spec.positional.len(),
                    group,
                    verb
                ))
            }
        }
    }
    let required = spec.positional.iter().filter(|a| a.required).count();
    if positionals.len() < required {
        let want: Vec<String> =
            spec.positional.iter().map(|a| format!("<{}>", a.name)).collect();
        return Err(format!(
            "missing arguments for {group}.{verb}.\nusage: {group} {verb} {}",
            want.join(" ")
        ));
    }

    let mut args = serde_json::Map::new();
    for (spec_arg, raw) in spec.positional.iter().zip(positionals.iter()) {
        args.insert(spec_arg.name.clone(), coerce_arg(&spec_arg.r#type, &spec_arg.name, raw)?);
    }
    for (name, values) in &flags {
        if name == "help" {
            continue;
        }
        let fspec = spec.flags.iter().find(|f| &f.name == name).ok_or_else(|| {
            format!("unknown flag --{name} for {group}.{verb} ({group} {verb} --help lists flags)")
        })?;
        if fspec.repeatable {
            let mut arr = Vec::new();
            for v in values {
                arr.push(coerce_flag(&fspec.r#type, name, v.as_deref())?);
            }
            args.insert(name.clone(), Json::Array(arr));
        } else {
            if values.len() > 1 {
                return Err(format!("flag --{name} is not repeatable"));
            }
            args.insert(
                name.clone(),
                coerce_flag(&fspec.r#type, name, values[0].as_deref())?,
            );
        }
    }
    if let Some((k, v)) = body {
        args.insert(k, v);
    }
    Ok((spec, Json::Object(args)))
}

/// Run a CLI command string against the engine. Returns the tool's result
/// VALUE (the `ok()` envelope is unwrapped). `--help` yields
/// [`Outcome::Help`]; usage/execution problems yield `Err(String)`.
pub async fn execute(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    input: &str,
) -> Result<Outcome, String> {
    let input = input.trim();
    let (words, flags) = split(input)?;
    // Help intent anywhere: bare, per group, or per verb.
    if words.is_empty() || flags.contains_key("help") {
        let text = if words.is_empty() {
            help_overview()
        } else if words.len() == 1 {
            help_group(&words[0])?
        } else {
            match find_spec(&words[0], &words[1]) {
                Some(spec) => help_verb(&spec),
                None => help_group(&words[0])?,
            }
        };
        return Ok(Outcome::Help(text));
    }
    let (spec, args) = prepare(input)?;
    let out = super::tools::run(engine, principal, &spec, &args).await?;
    match &out {
        Json::Object(m)
            if m.get("status") == Some(&json!("ok")) && m.contains_key("result") =>
        {
            Ok(Outcome::Value(m["result"].clone()))
        }
        _ => Ok(Outcome::Value(out)),
    }
}
