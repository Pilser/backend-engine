use engine::registry::{ArgType, CommandSpec, FlagType};
use serde_json::Value as Json;
use std::collections::BTreeMap;

pub fn load() -> Vec<CommandSpec> {
    engine::registry::registry()
}

fn groups(specs: &[CommandSpec]) -> Vec<String> {
    let mut set: Vec<String> = Vec::new();
    for s in specs {
        if !set.contains(&s.group) {
            set.push(s.group.clone());
        }
    }
    set
}

pub fn about() -> String {
    "serverlessEngine-rs — a standalone, embeddable serverless platform.\n\
     \n\
     Boards are apps; each board is one HelixDB tenant. Records are JSON documents\n\
     stored as graph nodes in the board's tenant; recipes are event-driven\n\
     automation; keys authorize read/write/admin access.\n\
     \n\
     First steps:\n\
       srv apps create 'my app'             create a board (returns b_<id>)\n\
       srv records submit b_<id> <table> '{\"name\":\"x\"}'   insert a record\n\
       srv records query b_<id> <table> --filter '...'      query records\n\
       srv records search b_<id> <table> 'laptop'           full-text search\n\
       srv graph link b_<id> <from> KNOWS <to>               link nodes into a graph\n\
       srv graph traverse b_<id> <from> --dir out            walk the graph\n\
       srv --help                           see every command\n\
       srv help <group> <verb>              full spec of one verb\n"
        .to_string()
}

pub fn help_index(specs: &[CommandSpec]) -> String {
    let mut out = String::new();
    out.push_str("usage: srv <group> <verb> [args]\n\n");
    for g in groups(specs) {
        out.push_str(&format!("{g}:\n"));
        for s in specs.iter().filter(|s| s.group == g) {
            out.push_str(&format!("  srv {} {}   {}\n", s.group, s.verb, s.summary));
        }
        out.push('\n');
    }
    out.push_str("help paths:\n");
    out.push_str("  help                      this index\n");
    out.push_str("  help <group>              all verbs in a group\n");
    out.push_str("  help <group> <verb>       full spec of one verb\n");
    out.push_str("  about                     orientation / mental model\n");
    out.push_str("  reference                 full command reference\n");
    out.push_str("  --help works at any depth\n");
    out
}

pub fn group_help(specs: &[CommandSpec], group: &str) -> String {
    let members: Vec<&CommandSpec> = specs.iter().filter(|s| s.group == group).collect();
    if members.is_empty() {
        return format!("unknown group '{group}'. Try 'srv --help'.\n");
    }
    let mut out = String::new();
    out.push_str(&format!("srv {group} <verb> [args]\n"));
    for s in members {
        out.push_str(&format!("  {:<14} {}\n", s.verb, s.summary));
    }
    out
}

pub fn verb_help(specs: &[CommandSpec], name: &str) -> String {
    let Some(spec) = specs.iter().find(|s| s.name() == name) else {
        return format!("no help for '{name}'. Try 'srv --help'.\n");
    };
    let mut out = String::new();
    out.push_str(&format!("srv {} {}\n", spec.group, spec.verb));
    out.push_str(&format!("{}\n\n", spec.description));
    if !spec.positional.is_empty() {
        out.push_str("positional:\n");
        for a in &spec.positional {
            out.push_str(&format!(
                "  {:<14} {:?} {} {}\n",
                a.name, a.r#type, if a.required { "(required)" } else { "(optional)" }, a.help
            ));
        }
        out.push('\n');
    }
    if !spec.flags.is_empty() {
        out.push_str("flags:\n");
        for f in &spec.flags {
            out.push_str(&format!(
                "  --{:<12} {:?} default={:?} {}\n",
                f.name, f.r#type, f.default, f.help
            ));
        }
        out.push('\n');
    }
    if let Some(b) = &spec.body_json {
        out.push_str(&format!("json body: {}\n\n", b));
    }
    out.push_str(&format!("response: {}\n\n", spec.response));
    if let Some(ex) = spec.examples.first() {
        out.push_str(&format!("example: srv {}\n", ex.args));
        out.push_str(&format!("  → {}\n\n", ex.response));
    }
    if !spec.see_also.is_empty() {
        out.push_str(&format!("see also: {}\n", spec.see_also.join(", ")));
    }
    if let Some(d) = &spec.danger_notes {
        out.push_str(&format!("danger: {d}\n"));
    }
    out
}

pub fn reference(specs: &[CommandSpec]) -> String {
    let mut out = String::new();
    for s in specs {
        out.push_str(&verb_help(specs, &s.name()));
        out.push('\n');
    }
    out
}

// ---- argv → tool call ------------------------------------------------

#[derive(Debug)]
pub enum ParseError {
    UnknownVerb {
        did_you_mean: Vec<String>,
    },
    Missing(String),
    InvalidValue(String),
}

pub fn parse_call(
    specs: &[CommandSpec],
    group: &str,
    verb: &str,
    tail: &[String],
) -> Result<crate::client::ToolCall, ParseError> {
    let Some(spec) = specs.iter().find(|s| s.group == group && s.verb == verb) else {
        let candidates: Vec<String> = specs
            .iter()
            .filter(|s| s.group == group)
            .map(|s| s.verb.clone())
            .collect();
        return Err(ParseError::UnknownVerb {
            did_you_mean: candidates,
        });
    };
    let mut args: Vec<&String> = Vec::new();
    let mut flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut i = 0;
    while i < tail.len() {
        let tok = &tail[i];
        if let Some(flag) = tok.strip_prefix("--") {
            let spec_flag = spec.flags.iter().find(|f| f.name == flag).ok_or_else(|| {
                ParseError::InvalidValue(format!("unknown flag --{flag}"))
            })?;
            if matches!(spec_flag.r#type, FlagType::Bool) {
                flags.entry(flag.to_string()).or_default().push("true".into());
            } else {
                let value = tail
                    .get(i + 1)
                    .ok_or_else(|| ParseError::Missing(format!("missing value for --{flag}")))?;
                flags
                    .entry(flag.to_string())
                    .or_default()
                    .push(trim_quotes(value));
                i += 1;
            }
        } else {
            args.push(tok);
        }
        i += 1;
    }

    let mut arguments = serde_json::Map::new();
    for (idx, a) in spec.positional.iter().enumerate() {
        match args.get(idx) {
            Some(v) => {
                arguments.insert(a.name.clone(), typed(&a.r#type, trim_quotes(v)));
            }
            None if a.required => {
                return Err(ParseError::Missing(format!("missing positional '{}'", a.name)))
            }
            None => {}
        }
    }
    if let Some(body) = args.get(spec.positional.len()) {
        if spec.body_json.is_some() {
            arguments.insert(
                "payload".to_string(),
                serde_json::from_str(trim_quotes(body).as_str()).unwrap_or(Json::String(trim_quotes(body))),
            );
        } else {
            return Err(ParseError::InvalidValue(format!(
                "unexpected positional '{}' (no json body for this verb)",
                trim_quotes(body)
            )));
        }
    }
    if args.len() > spec.positional.len() + 1 {
        return Err(ParseError::InvalidValue("too many positional arguments".into()));
    }
    for f in &spec.flags {
        let raw = flags.get(&f.name);
        if raw.is_none() {
            if let Some(d) = &f.default {
                arguments.insert(f.name.clone(), Json::String(d.clone()));
            }
            continue;
        }
        let values = raw.unwrap();
        let first = values.first().cloned().unwrap_or_default();
        let value = match f.r#type {
            FlagType::Bool => Json::Bool(values.len() > 0),
            FlagType::Int => first.parse::<i64>().map(Json::from).unwrap_or(Json::Null),
            FlagType::Json => serde_json::from_str(&first).unwrap_or(Json::String(first)),
            FlagType::Enum => {
                if f.allowed.contains(&first) {
                    Json::String(first)
                } else {
                    return Err(ParseError::InvalidValue(format!(
                        "--{} must be one of: {}",
                        f.name,
                        f.allowed.join(", ")
                    )));
                }
            }
            _ => Json::String(first),
        };
        arguments.insert(f.name.clone(), value);
    }

    Ok(crate::client::ToolCall {
        name: spec.name(),
        arguments: Json::Object(arguments),
    })
}

fn trim_quotes(s: &str) -> String {
    s.trim_matches('"').trim_matches('\'').to_string()
}

fn typed(t: &ArgType, s: String) -> Json {
    match t {
        ArgType::Int | ArgType::Seq => s.parse::<i64>().map(Json::from).unwrap_or(Json::String(s)),
        _ => Json::String(s),
    }
}

pub fn error_text(specs: &[CommandSpec], group: &str, verb: &str, e: &ParseError) -> String {
    match e {
        ParseError::UnknownVerb {
            did_you_mean, ..
        } => {
            let mut out = format!("error: unknown verb 'srv {group} {verb}'\n");
            if !did_you_mean.is_empty() {
                out.push_str(&format!("did you mean: {}\n", did_you_mean.join(", ")));
            }
            out.push_str(&format!("usage:\n{}", group_help(specs, group)));
            out
        }
        ParseError::Missing(m) => {
            let name = format!("{group}.{verb}");
            format!("error: {m}\n\n{}", verb_help(specs, &name))
        }
        ParseError::InvalidValue(m) => {
            let name = format!("{group}.{verb}");
            format!("error: {m}\n\n{}", verb_help(specs, &name))
        }
    }
}