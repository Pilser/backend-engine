#[derive(Debug, Clone)]
pub struct BlobMeta {
    pub key: String,
    pub size: u64,
    pub sha256: String,
    pub content_type: Option<String>,
    pub modified: Option<String>,
}

#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub key: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct PutInfo {
    pub key: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectStoreCaps {
    pub presign: bool,
    pub multipart: bool,
    pub versioning: bool,
    pub copy: bool,
}

impl ObjectStoreCaps {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn full() -> Self {
        Self { presign: true, multipart: true, versioning: true, copy: true }
    }
}

pub trait ObjectStore: Send + Sync + 'static {
    fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<PutInfo>;

    fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>>;

    fn head(&self, key: &str) -> anyhow::Result<Option<BlobMeta>>;

    fn delete(&self, key: &str) -> anyhow::Result<()>;

    fn list(&self, prefix: &str) -> anyhow::Result<Vec<KeyInfo>>;

    fn presign(&self, _key: &str, _method: &str, _ttl: chrono::Duration) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn copy(&self, _from: &str, _to: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn capabilities(&self) -> ObjectStoreCaps;
}
