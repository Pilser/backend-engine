use engine::storage::memory::InMemoryDatabase;

pub fn in_memory() -> InMemoryDatabase {
    InMemoryDatabase::new()
}