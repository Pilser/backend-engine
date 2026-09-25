use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationAction {
    Exec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migration {
    pub version: u32,
    pub name: String,
    pub action: MigrationAction,
}

impl Migration {
    pub fn new(version: u32, name: impl Into<String>) -> Self {
        Self { version, name: name.into(), action: MigrationAction::Exec }
    }
}

pub trait MigrationBackend {
    fn schema_version(&self) -> anyhow::Result<u32>;

    fn set_schema_version(&mut self, version: u32) -> anyhow::Result<()>;
}

pub struct Migrator {
    migrations: Vec<Migration>,
}

impl Migrator {
    pub fn new(migrations: Vec<Migration>) -> Self {
        let mut sorted = migrations;
        sorted.sort_by_key(|m| m.version);
        Self { migrations: sorted }
    }

    pub fn pending(&self, current: u32) -> Vec<&Migration> {
        self.migrations.iter().filter(|m| m.version > current).collect()
    }

    pub fn run(&self, backend: &mut dyn MigrationBackend) -> anyhow::Result<Vec<u32>> {
        let current = backend.schema_version()?;
        let mut applied = Vec::new();
        for m in self.pending(current) {
            backend.set_schema_version(m.version)?;
            applied.push(m.version);
        }
        Ok(applied)
    }
}
