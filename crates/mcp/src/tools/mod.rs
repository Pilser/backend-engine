pub mod apps;
pub mod auth;
pub mod endpoints;
pub mod files;
pub mod graph;
pub mod jobs;
pub mod keys;
pub mod links;
pub mod recipes;
pub mod records;
pub mod secrets;
pub mod subapps;
pub mod tables;
pub mod users;

use engine::model::Principal;
use engine::registry::CommandSpec;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub fn run(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    spec: &CommandSpec,
    arguments: &Json,
) -> Result<Json, String> {
    match spec.name().as_str() {
        "apps.create" => apps::create(engine, principal, arguments),
        "apps.list" => apps::list(engine, principal, arguments),
        "apps.show" => apps::show(engine, principal, arguments),
        "apps.delete" => apps::delete(engine, principal, arguments),
        "apps.update" => apps::update(engine, principal, arguments),
        "apps.resources" => apps::resources(engine, principal, arguments),
        "endpoints.list" => endpoints::list(engine, principal, arguments),
        "auth.signup" => auth::signup(engine, principal, arguments),
        "auth.login" => auth::login(engine, principal, arguments),
        "auth.me" => auth::me(engine, principal, arguments),
        "auth.set_role" => auth::set_role(engine, principal, arguments),
        "users.list" => users::list(engine, principal, arguments),
        "keys.list" => keys::list(engine, principal, arguments),
        "records.submit" => records::submit(engine, principal, arguments),
        "records.get" => records::get(engine, principal, arguments),
        "records.list" => records::list(engine, principal, arguments),
        "records.query" => records::query(engine, principal, arguments),
        "records.search" => records::search(engine, principal, arguments),
        "records.aggregate" => records::aggregate(engine, principal, arguments),
        "records.delete" => records::delete(engine, principal, arguments),
        "records.import" => records::import_records(engine, principal, arguments),
        "records.bulk" => records::bulk(engine, principal, arguments),
        "tables.create" => tables::create(engine, principal, arguments),
        "tables.list" => tables::list(engine, principal, arguments),
        "tables.show" => tables::show(engine, principal, arguments),
        "tables.delete" => tables::delete(engine, principal, arguments),
        "secrets.set" => secrets::set(engine, principal, arguments),
        "secrets.list" => secrets::list(engine, principal, arguments),
        "secrets.show" => secrets::show(engine, principal, arguments),
        "secrets.delete" => secrets::delete(engine, principal, arguments),
        "recipes.list" => recipes::list(engine, principal, arguments),
        "recipes.show" => recipes::show(engine, principal, arguments),
        "recipes.add" => recipes::add(engine, principal, arguments),
        "keys.show" => keys::show(engine, principal, arguments),
        "files.list" => files::list(engine, principal, arguments),
        "files.put" => files::put(engine, principal, arguments),
        "files.get" => files::get(engine, principal, arguments),
        "files.export" => files::export(engine, principal, arguments),
        "files.import" => files::import(engine, principal, arguments),
        "files.upload" => files::upload(engine, principal, arguments),
        "files.delete" => files::delete(engine, principal, arguments),
        "graph.link" => graph::link(engine, principal, arguments),
        "graph.unlink" => graph::unlink(engine, principal, arguments),
        "graph.delete" => graph::delete(engine, principal, arguments),
        "graph.traverse" => graph::traverse(engine, principal, arguments),
        "graph.sync" => graph::sync(engine, principal, arguments),
        "graph.search_edges" => graph::search_edges(engine, principal, arguments),
        "graph.schema" => links::schema(engine, principal, arguments),
        "links.list" => links::list(engine, principal, arguments),
        "links.show" => links::show(engine, principal, arguments),
        "subapps.list" => subapps::list(engine, principal, arguments),
        "subapps.remove" => subapps::remove(engine, principal, arguments),
        "jobs.add" => jobs::add(engine, principal, arguments),
        "jobs.list" => jobs::list(engine, principal, arguments),
        "jobs.show" => jobs::show(engine, principal, arguments),
        "jobs.remove" => jobs::remove(engine, principal, arguments),
        "jobs.runs" => jobs::runs(engine, principal, arguments),
        other => Err(format!("tool '{other}' not implemented yet")),
    }
}

pub fn arg<'a>(arguments: &'a Json, name: &str) -> Result<&'a Json, String> {
    arguments
        .get(name)
        .ok_or_else(|| format!("missing argument '{name}'"))
}

pub fn arg_str<'a>(arguments: &'a Json, name: &str) -> Result<&'a str, String> {
    arg(arguments, name)?
        .as_str()
        .ok_or_else(|| format!("argument '{name}' must be a string"))
}

pub fn arg_i64(arguments: &Json, name: &str, default: i64) -> i64 {
    arguments
        .get(name)
        .and_then(|v| v.as_i64())
        .unwrap_or(default)
}

pub fn ok(v: Json) -> Result<Json, String> {
    Ok(json!({ "status": "ok", "result": v }))
}