// Table-name constants shared by every engine module. The in-memory `Database`
// adapter stores rows in string-keyed tables; all modules must agree on names.

pub const TABLE_APPS: &str = "wb_apps";
pub const TABLE_RECORDS: &str = "wb_records";
pub const TABLE_KEYS: &str = "wb_keys";
pub const TABLE_RECIPES: &str = "wb_recipes";
pub const TABLE_RECIPE_RUNS: &str = "wb_recipe_runs";
pub const TABLE_APP_SECRETS: &str = "wb_app_secrets";
pub const TABLE_HOOKS: &str = "wb_hooks";
pub const TABLE_HOOK_DELIVERIES: &str = "wb_hook_deliveries";
pub const TABLE_LINKS: &str = "wb_links";
pub const TABLE_SUBAPPS: &str = "wb_subapps";
pub const TABLE_AUDIT: &str = "wb_audit";
pub const TABLE_JOBS: &str = "app_jobs";
pub const TABLE_JOB_RUNS: &str = "wb_job_runs";
pub const TABLE_INDEX: &str = "srv_board_index";
pub const TABLE_USERS: &str = "wb_users";
pub const TABLE_SESSIONS: &str = "wb_sessions";
pub const TABLE_TABLES: &str = "wb_tables";

pub fn scoped_key(board_id: &str, name: &str) -> String {
    format!("{board_id}/{name}")
}
