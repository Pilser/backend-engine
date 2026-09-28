//! R2 [`engine::ObjectStore`] adapter.
//!
//! Keys map 1:1 to object keys. Content-type metadata is not needed on the
//! blob itself: file content-types come from record metadata and asset
//! content-types from path extensions (see `engine::files`).

use async_trait::async_trait;
use engine::storage::object_store::{BlobMeta, KeyInfo, ObjectStore, ObjectStoreCaps, PutInfo};
use sha2::{Digest, Sha256};
use worker::Bucket;

pub struct R2Store {
    bucket: Bucket,
}

impl R2Store {
    pub fn new(bucket: Bucket) -> Self {
        Self { bucket }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[async_trait(?Send)]
impl ObjectStore for R2Store {
    async fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<PutInfo> {
        let size = bytes.len() as u64;
        let sha256 = sha256_hex(bytes);
        self.bucket.put(key.to_string(), bytes.to_vec()).execute().await?;
        Ok(PutInfo { key: key.to_string(), size, sha256 })
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(obj) = self.bucket.get(key.to_string()).execute().await? else {
            return Ok(None);
        };
        let Some(body) = obj.body() else {
            return Ok(None);
        };
        Ok(Some(body.bytes().await?))
    }

    async fn head(&self, key: &str) -> anyhow::Result<Option<BlobMeta>> {
        let Some(obj) = self.bucket.head(key.to_string()).await? else {
            return Ok(None);
        };
        Ok(Some(BlobMeta {
            key: obj.key(),
            size: obj.size(),
            // R2's native http_etag changes on every rewrite: a perfect
            // opaque validator (served as the asset ETag).
            sha256: obj.http_etag(),
            content_type: None,
            modified: None,
        }))
    }

    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.bucket.delete(key.to_string()).await?;
        Ok(())
    }

    async fn list(&self, prefix: &str) -> anyhow::Result<Vec<KeyInfo>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut listing = self.bucket.list().prefix(prefix.to_string());
            if let Some(c) = cursor.take() {
                listing = listing.cursor(c);
            }
            let page = listing.execute().await?;
            out.extend(page.objects().into_iter().map(|o| KeyInfo { key: o.key(), size: o.size() }));
            if page.truncated() {
                cursor = page.cursor();
            } else {
                break;
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn copy(&self, from: &str, to: &str) -> anyhow::Result<bool> {
        let Some(bytes) = self.get(from).await? else {
            return Ok(false);
        };
        self.put(to, &bytes).await?;
        Ok(true)
    }

    fn capabilities(&self) -> ObjectStoreCaps {
        ObjectStoreCaps { copy: true, ..ObjectStoreCaps::none() }
    }
}
