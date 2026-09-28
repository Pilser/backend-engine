pub mod apps;
pub mod auth;
pub mod email;
pub mod endpoints;
pub mod files;
pub mod graph;
pub mod jobs;
pub mod keys;
pub mod links;
pub mod plugins;
pub mod recipes;
pub mod site;
pub mod records;
pub mod secrets;
pub mod subapps;
pub mod tables;
pub mod users;

use engine::model::Principal;
use engine::registry::CommandSpec;
use engine::ServerlessEngine;
use serde_json::{json, Value as Json};

pub async fn run(
    engine: &mut ServerlessEngine,
    principal: &Principal,
    spec: &CommandSpec,
    arguments: &Json,
) -> Result<Json, String> {
    match spec.name().as_str() {
        "tenant.show" => apps::show(engine, principal, arguments).await,
        "tenant.update" => apps::update(engine, principal, arguments).await,
        "tenant.resources" => apps::resources(engine, principal, arguments).await,
        "endpoints.list" => endpoints::list(engine, principal, arguments),
        "email.send" => email::send(engine, principal, arguments).await,
        "auth.signup" => auth::signup(engine, principal, arguments).await,
        "auth.login" => auth::login(engine, principal, arguments).await,
        "auth.me" => auth::me(engine, principal, arguments).await,
        "auth.logout" => auth::logout(engine, principal, arguments).await,
        "auth.set_role" => auth::set_role(engine, principal, arguments).await,
        "users.list" => users::list(engine, principal, arguments).await,
        "keys.list" => keys::list(engine, principal, arguments).await,
        "records.submit" => records::submit(engine, principal, arguments).await,
        "records.get" => records::get(engine, principal, arguments).await,
        "records.list" => records::list(engine, principal, arguments).await,
        "records.query" => records::query(engine, principal, arguments).await,
        "records.search" => records::search(engine, principal, arguments).await,
        "records.aggregate" => records::aggregate(engine, principal, arguments).await,
        "records.delete" => records::delete(engine, principal, arguments).await,
        "records.update" => records::update(engine, principal, arguments).await,
        "records.patch" => records::patch(engine, principal, arguments).await,
        "records.import" => records::import_records(engine, principal, arguments).await,
        "records.bulk" => records::bulk(engine, principal, arguments).await,
        "tables.create" => tables::create(engine, principal, arguments).await,
        "tables.list" => tables::list(engine, principal, arguments).await,
        "tables.show" => tables::show(engine, principal, arguments).await,
        "tables.delete" => tables::delete(engine, principal, arguments).await,
        "tables.config" => tables::config(engine, principal, arguments).await,
        "secrets.set" => secrets::set(engine, principal, arguments).await,
        "secrets.list" => secrets::list(engine, principal, arguments).await,
        "secrets.show" => secrets::show(engine, principal, arguments).await,
        "secrets.delete" => secrets::delete(engine, principal, arguments).await,
        "recipes.list" => recipes::list(engine, principal, arguments).await,
        "recipes.show" => recipes::show(engine, principal, arguments).await,
        "recipes.add" => recipes::add(engine, principal, arguments).await,
        "keys.show" => keys::show(engine, principal, arguments).await,
        "keys.issue" => keys::issue(engine, principal, arguments).await,
        "files.list" => files::list(engine, principal, arguments).await,
        "files.put" => files::put(engine, principal, arguments).await,
        "files.get" => files::get(engine, principal, arguments).await,
        "files.export" => files::export(engine, principal, arguments).await,
        "files.import" => files::import(engine, principal, arguments).await,
        "files.upload" => files::upload(engine, principal, arguments).await,
        "files.delete" => files::delete(engine, principal, arguments).await,
        "graph.link" => graph::link(engine, principal, arguments).await,
        "graph.unlink" => graph::unlink(engine, principal, arguments).await,
        "graph.delete" => graph::delete(engine, principal, arguments).await,
        "graph.traverse" => graph::traverse(engine, principal, arguments).await,
        "graph.sync" => graph::sync(engine, principal, arguments).await,
        "graph.search_edges" => graph::search_edges(engine, principal, arguments).await,
        "graph.schema" => links::schema(engine, principal, arguments).await,
        "links.list" => links::list(engine, principal, arguments).await,
        "links.show" => links::show(engine, principal, arguments).await,
        "subapps.list" => subapps::list(engine, principal, arguments).await,
        "subapps.remove" => subapps::remove(engine, principal, arguments).await,
        "jobs.add" => jobs::add(engine, principal, arguments).await,
        "jobs.list" => jobs::list(engine, principal, arguments).await,
        "jobs.show" => jobs::show(engine, principal, arguments).await,
        "jobs.remove" => jobs::remove(engine, principal, arguments).await,
        "jobs.runs" => jobs::runs(engine, principal, arguments).await,
        "plugins.install" => plugins::install(engine, principal, arguments).await,
        "plugins.list" => plugins::list(engine, principal, arguments).await,
        "plugins.show" => plugins::show(engine, principal, arguments).await,
        "plugins.remove" => plugins::remove(engine, principal, arguments).await,
        "site.routes" => site::routes(engine, principal, arguments).await,
        "site.add" => site::add(engine, principal, arguments).await,
        "site.remove" => site::remove(engine, principal, arguments).await,
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