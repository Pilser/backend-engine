use engine::storage::object_store::{BlobMeta, KeyInfo, ObjectStore, ObjectStoreCaps, PutInfo};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub struct FsObjectStore {
    root: PathBuf,
}

impl FsObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, key: &str) -> PathBuf {
        let mut p = self.root.clone();
        for seg in key.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." {
                continue;
            }
            p.push(seg);
        }
        p
    }
}

impl ObjectStore for FsObjectStore {
    fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<PutInfo> {
        let path = self.resolve(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let hex = hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let size = bytes.len() as u64;
        Ok(PutInfo { key: key.to_string(), size, sha256: hex })
    }

    fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let path = self.resolve(key);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(path)?))
    }

    fn head(&self, key: &str) -> anyhow::Result<Option<BlobMeta>> {
        let path = self.resolve(key);
        if !path.is_file() {
            return Ok(None);
        }
        let size = std::fs::metadata(&path)?.len();
        let bytes = std::fs::read(&path)?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hex = hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>();
        Ok(Some(BlobMeta {
            key: key.to_string(),
            size,
            sha256: hex,
            content_type: None,
            modified: None,
        }))
    }

    fn delete(&self, key: &str) -> anyhow::Result<()> {
        let path = self.resolve(key);
        if path.is_file() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn list(&self, prefix: &str) -> anyhow::Result<Vec<KeyInfo>> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    let rel = path.strip_prefix(&self.root)?.to_string_lossy().replace('\\', "/");
                    if rel.starts_with(prefix) {
                        out.push(KeyInfo { key: rel, size: std::fs::metadata(&path)?.len() });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn capabilities(&self) -> ObjectStoreCaps {
        ObjectStoreCaps::none()
    }
}