//! Off-node copies of backups in an S3-compatible bucket.
//!
//! A backup that only lives on the node dies with the node. When a bucket
//! is configured, every completed archive is copied to
//! `<prefix>/<server>/<id>.tar.gz` with its record beside it as
//! `<id>.json`, so the records can be adopted back after a node rebuild
//! ("sync") and a restore can fetch an archive the node no longer holds.
//! Anything that speaks S3 works: AWS, Backblaze B2, Wasabi, Cloudflare R2,
//! MinIO, Hetzner, DigitalOcean Spaces.
//!
//! Settings come from `NEXUS_BACKUP_S3_*` and can be changed on the
//! Settings page; they are persisted under the data directory with the
//! secret key, so the file is `0600`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;

use object_store::{ObjectStore, PutPayload, WriteMultipart};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::backup::BackupInfo;
use crate::error::{NodeError, Result};

const SETTINGS_FILE: &str = ".nexus/remote_backup.json";
/// Parts of this size go up in parallel, at most four at a time.
const PART_SIZE: usize = 8 * 1024 * 1024;
const PARALLEL_PARTS: usize = 4;

/// Where off-node copies go.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSettings {
    /// Off; nothing is copied.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bucket: String,
    /// Blank for AWS S3; anything else is the provider's endpoint.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Blank means `us-east-1`, which every S3-compatible provider accepts.
    #[serde(default)]
    pub region: Option<String>,
    /// Both blank means the node's own AWS credentials (environment or
    /// instance role) are used.
    #[serde(default)]
    pub access_key: Option<String>,
    #[serde(default)]
    pub secret_key: Option<String>,
    /// Key prefix inside the bucket, without leading or trailing slash.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// `bucket` in the path rather than the host name; what MinIO and most
    /// self-hosted providers expect. AWS accepts either.
    #[serde(default = "default_true")]
    pub path_style: bool,
    /// Keep the archive on the node after it is copied. Off saves disk and
    /// makes every restore a download.
    #[serde(default = "default_true")]
    pub keep_local: bool,
}

fn default_prefix() -> String {
    "nexus".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bucket: String::new(),
            endpoint: None,
            region: None,
            access_key: None,
            secret_key: None,
            prefix: default_prefix(),
            path_style: true,
            keep_local: true,
        }
    }
}

impl RemoteSettings {
    pub fn from_env() -> Self {
        let get = |name: &str| {
            std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        };
        let flag = |name: &str, default: bool| {
            get(name)
                .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "on" | "true" | "yes"))
                .unwrap_or(default)
        };
        let mut s = Self::default();
        s.bucket = get("NEXUS_BACKUP_S3_BUCKET").unwrap_or_default();
        s.enabled = !s.bucket.is_empty();
        s.endpoint = get("NEXUS_BACKUP_S3_ENDPOINT");
        s.region = get("NEXUS_BACKUP_S3_REGION");
        s.access_key = get("NEXUS_BACKUP_S3_ACCESS_KEY");
        s.secret_key = get("NEXUS_BACKUP_S3_SECRET_KEY");
        if let Some(p) = get("NEXUS_BACKUP_S3_PREFIX") {
            s.prefix = p;
        }
        s.path_style = flag("NEXUS_BACKUP_S3_PATH_STYLE", true);
        s.keep_local = flag("NEXUS_BACKUP_KEEP_LOCAL", true);
        s.normalized()
    }

    /// Empty strings mean unset; the prefix loses stray slashes.
    fn normalized(mut self) -> Self {
        let clean = |v: &mut Option<String>| {
            if let Some(s) = v.as_mut() {
                *s = s.trim().to_string();
            }
            if v.as_deref().map(str::is_empty).unwrap_or(false) {
                *v = None;
            }
        };
        clean(&mut self.endpoint);
        clean(&mut self.region);
        clean(&mut self.access_key);
        clean(&mut self.secret_key);
        self.bucket = self.bucket.trim().to_string();
        self.prefix = self.prefix.trim().trim_matches('/').to_string();
        if self.prefix.is_empty() {
            self.prefix = default_prefix();
        }
        self
    }

    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let bucket_ok = (3..=63).contains(&self.bucket.len())
            && self
                .bucket
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
        if !bucket_ok {
            return Err(NodeError::InvalidInput(
                "bucket must be 3–63 lowercase letters, digits, dots or dashes".to_string(),
            ));
        }
        if let Some(e) = &self.endpoint {
            if !(e.starts_with("https://") || e.starts_with("http://")) || e.contains(' ') {
                return Err(NodeError::InvalidInput(
                    "endpoint must be an http(s) URL".to_string(),
                ));
            }
        }
        if self.access_key.is_some() != self.secret_key.is_some() {
            return Err(NodeError::InvalidInput(
                "access key and secret key go together".to_string(),
            ));
        }
        if self.prefix.split('/').any(|p| p.is_empty() || p == "." || p == "..")
            || self.prefix.len() > 200
        {
            return Err(NodeError::InvalidInput(
                "prefix must be a plain path like backups/node1".to_string(),
            ));
        }
        Ok(())
    }

    /// The settings without the secret, for the API.
    pub fn view(&self) -> RemoteSettingsView {
        RemoteSettingsView {
            enabled: self.enabled,
            bucket: self.bucket.clone(),
            endpoint: self.endpoint.clone(),
            region: self.region.clone(),
            access_key: self.access_key.clone(),
            has_secret_key: self.secret_key.is_some(),
            prefix: self.prefix.clone(),
            path_style: self.path_style,
            keep_local: self.keep_local,
        }
    }

    fn build(&self) -> Result<Arc<dyn ObjectStore>> {
        let mut b = AmazonS3Builder::new()
            .with_bucket_name(&self.bucket)
            .with_region(self.region.clone().unwrap_or_else(|| "us-east-1".to_string()))
            .with_virtual_hosted_style_request(!self.path_style);
        if let Some(e) = &self.endpoint {
            b = b.with_endpoint(e).with_allow_http(e.starts_with("http://"));
        }
        if let (Some(a), Some(s)) = (&self.access_key, &self.secret_key) {
            b = b.with_access_key_id(a).with_secret_access_key(s);
        }
        let store = b
            .build()
            .map_err(|e| NodeError::InvalidInput(format!("bucket settings: {}", e)))?;
        Ok(Arc::new(store))
    }
}

/// What the API shows and accepts: the secret key never comes back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSettingsView {
    pub enabled: bool,
    pub bucket: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub access_key: Option<String>,
    #[serde(default)]
    pub has_secret_key: bool,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    #[serde(default = "default_true")]
    pub path_style: bool,
    #[serde(default = "default_true")]
    pub keep_local: bool,
}

/// A settings update from the API. A blank secret keeps the one on file.
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteSettingsUpdate {
    #[serde(flatten)]
    pub view: RemoteSettingsView,
    #[serde(default)]
    pub secret_key: Option<String>,
}

/// Where a backup's off-node copy is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteRef {
    pub bucket: String,
    pub key: String,
    pub uploaded_at: i64,
}

/// How the last few operations went.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RemoteStatus {
    pub last_ok_at: Option<i64>,
    pub last_error: Option<String>,
}

/// The result of a connectivity test.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
}

pub struct RemoteBackupStore {
    path: PathBuf,
    settings: RwLock<RemoteSettings>,
    client: RwLock<Option<Arc<dyn ObjectStore>>>,
    status: RwLock<RemoteStatus>,
    /// A backend given directly (tests); settings changes keep it.
    fixed: Option<Arc<dyn ObjectStore>>,
}

impl RemoteBackupStore {
    /// The persisted settings if there are any, else the environment's.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SETTINGS_FILE);
        let settings = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<RemoteSettings>(&b).ok())
            .map(RemoteSettings::normalized)
            .unwrap_or_else(RemoteSettings::from_env);
        let client = if settings.enabled {
            match settings.validate().and_then(|_| settings.build()) {
                Ok(c) => Some(c),
                Err(e) => {
                    warn!("Off-node backups are configured but unusable: {}", e);
                    None
                }
            }
        } else {
            None
        };
        Self {
            path,
            settings: RwLock::new(settings),
            client: RwLock::new(client),
            status: RwLock::new(RemoteStatus::default()),
            fixed: None,
        }
    }

    /// A store over a backend of the caller's choosing.
    pub fn with_backend(data_dir: &Path, backend: Arc<dyn ObjectStore>, prefix: &str) -> Self {
        let settings = RemoteSettings {
            enabled: true,
            bucket: "local".to_string(),
            prefix: prefix.to_string(),
            ..Default::default()
        };
        Self {
            path: data_dir.join(SETTINGS_FILE),
            settings: RwLock::new(settings),
            client: RwLock::new(Some(backend.clone())),
            status: RwLock::new(RemoteStatus::default()),
            fixed: Some(backend),
        }
    }

    pub async fn settings(&self) -> RemoteSettings {
        self.settings.read().await.clone()
    }

    pub async fn view(&self) -> RemoteSettingsView {
        self.settings.read().await.view()
    }

    pub async fn status(&self) -> RemoteStatus {
        self.status.read().await.clone()
    }

    pub async fn enabled(&self) -> bool {
        self.client.read().await.is_some()
    }

    pub async fn keep_local(&self) -> bool {
        let s = self.settings.read().await;
        !s.enabled || s.keep_local
    }

    /// Apply an update from the API: a blank secret keeps the existing one.
    pub async fn update(&self, update: RemoteSettingsUpdate) -> Result<RemoteSettingsView> {
        let current = self.settings.read().await.clone();
        let v = update.view;
        let secret_key = update
            .secret_key
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or(current.secret_key);
        let settings = RemoteSettings {
            enabled: v.enabled,
            bucket: v.bucket,
            endpoint: v.endpoint,
            region: v.region,
            access_key: v.access_key,
            secret_key,
            prefix: v.prefix,
            path_style: v.path_style,
            keep_local: v.keep_local,
        };
        self.set(settings).await
    }

    pub async fn set(&self, settings: RemoteSettings) -> Result<RemoteSettingsView> {
        let settings = settings.normalized();
        settings.validate()?;
        let client = match &self.fixed {
            Some(fixed) if settings.enabled => Some(fixed.clone()),
            Some(_) => None,
            None if settings.enabled => Some(settings.build()?),
            None => None,
        };
        self.persist(&settings).await?;
        *self.client.write().await = client;
        *self.settings.write().await = settings.clone();
        *self.status.write().await = RemoteStatus::default();
        Ok(settings.view())
    }

    /// Back to the environment's defaults.
    pub async fn reset(&self) -> Result<RemoteSettingsView> {
        let _ = tokio::fs::remove_file(&self.path).await;
        let settings = RemoteSettings::from_env();
        let client = match &self.fixed {
            Some(fixed) if settings.enabled => Some(fixed.clone()),
            Some(_) => None,
            None if settings.enabled => settings.validate().and_then(|_| settings.build()).ok(),
            None => None,
        };
        *self.client.write().await = client;
        *self.settings.write().await = settings.clone();
        *self.status.write().await = RemoteStatus::default();
        Ok(settings.view())
    }

    async fn persist(&self, settings: &RemoteSettings) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(settings)
            .map_err(|e| NodeError::Internal(format!("serialize remote backup settings: {}", e)))?;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await;
        }
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    async fn client(&self) -> Result<Arc<dyn ObjectStore>> {
        self.client.read().await.clone().ok_or_else(|| {
            NodeError::InvalidInput("off-node backups are not configured".to_string())
        })
    }

    async fn record_ok(&self) {
        let mut st = self.status.write().await;
        st.last_ok_at = Some(chrono::Utc::now().timestamp());
        st.last_error = None;
    }

    async fn record_err(&self, e: &NodeError) {
        self.status.write().await.last_error = Some(e.to_string());
    }

    async fn keys(&self, container_id: &str, backup_id: &str) -> (ObjectPath, ObjectPath, String) {
        let s = self.settings.read().await;
        let base = format!("{}/{}/{}", s.prefix, container_id, backup_id);
        (
            ObjectPath::from(format!("{}.tar.gz", base)),
            ObjectPath::from(format!("{}.json", base)),
            s.bucket.clone(),
        )
    }

    /// Write a small object, read it back and remove it.
    pub async fn probe(&self) -> ProbeResult {
        let started = std::time::Instant::now();
        let result = self.probe_inner().await;
        let latency_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(()) => {
                self.record_ok().await;
                ProbeResult {
                    ok: true,
                    message: "bucket is writable".to_string(),
                    latency_ms,
                }
            }
            Err(e) => {
                self.record_err(&e).await;
                ProbeResult {
                    ok: false,
                    message: e.to_string(),
                    latency_ms,
                }
            }
        }
    }

    async fn probe_inner(&self) -> Result<()> {
        let client = self.client().await?;
        let prefix = self.settings.read().await.prefix.clone();
        let key = ObjectPath::from(format!(
            "{}/.nexus-probe-{}",
            prefix,
            uuid::Uuid::new_v4().simple()
        ));
        client
            .put(&key, PutPayload::from(b"nexus".to_vec()))
            .await
            .map_err(|e| NodeError::Internal(format!("write failed: {}", describe(e))))?;
        let back = client
            .get(&key)
            .await
            .map_err(|e| NodeError::Internal(format!("read back failed: {}", describe(e))))?
            .bytes()
            .await
            .map_err(|e| NodeError::Internal(format!("read back failed: {}", describe(e))))?;
        let _ = client.delete(&key).await;
        if back.as_ref() != b"nexus" {
            return Err(NodeError::Internal("read back the wrong bytes".to_string()));
        }
        Ok(())
    }

    /// Copy an archive and its record to the bucket.
    pub async fn upload(&self, info: &BackupInfo, archive: &Path) -> Result<RemoteRef> {
        let result = self.upload_inner(info, archive).await;
        match &result {
            Ok(_) => self.record_ok().await,
            Err(e) => self.record_err(e).await,
        }
        result
    }

    async fn upload_inner(&self, info: &BackupInfo, archive: &Path) -> Result<RemoteRef> {
        let client = self.client().await?;
        let (data_key, record_key, bucket) = self.keys(&info.container_id, &info.id).await;

        let mut file = tokio::fs::File::open(archive).await?;
        let upload = client
            .put_multipart(&data_key)
            .await
            .map_err(|e| NodeError::Internal(format!("start upload: {}", describe(e))))?;
        let mut writer = WriteMultipart::new_with_chunk_size(upload, PART_SIZE);
        let pumped = pump(&mut file, &mut writer).await;
        match pumped {
            Ok(()) => {
                writer
                    .finish()
                    .await
                    .map_err(|e| NodeError::Internal(format!("finish upload: {}", describe(e))))?;
            }
            Err(e) => {
                let _ = writer.abort().await;
                return Err(e);
            }
        }

        let remote = RemoteRef {
            bucket,
            key: data_key.to_string(),
            uploaded_at: chrono::Utc::now().timestamp(),
        };
        let mut record = info.clone();
        record.remote = Some(remote.clone());
        record.remote_error = None;
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|e| NodeError::Internal(format!("serialize backup record: {}", e)))?;
        if let Err(e) = client.put(&record_key, PutPayload::from(bytes)).await {
            let _ = client.delete(&data_key).await;
            return Err(NodeError::Internal(format!(
                "write record: {}",
                describe(e)
            )));
        }
        info!(
            "Copied backup {} of {} to {}/{}",
            info.id, info.container_id, remote.bucket, remote.key
        );
        Ok(remote)
    }

    /// Fetch an archive to `dest` (written beside it first, then renamed).
    pub async fn download(&self, container_id: &str, backup_id: &str, dest: &Path) -> Result<()> {
        let result = self.download_inner(container_id, backup_id, dest).await;
        match &result {
            Ok(()) => self.record_ok().await,
            Err(e) => self.record_err(e).await,
        }
        result
    }

    async fn download_inner(&self, container_id: &str, backup_id: &str, dest: &Path) -> Result<()> {
        let client = self.client().await?;
        let (data_key, _, _) = self.keys(container_id, backup_id).await;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = dest.with_extension("tar.gz.part");
        let mut stream = client
            .get(&data_key)
            .await
            .map_err(|e| NodeError::Internal(format!("fetch: {}", describe(e))))?
            .into_stream();
        let mut out = tokio::fs::File::create(&tmp).await?;
        let copied: Result<()> = async {
            use tokio::io::AsyncWriteExt;
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|e| NodeError::Internal(format!("fetch: {}", describe(e))))?;
                out.write_all(&chunk).await?;
            }
            out.flush().await?;
            Ok(())
        }
        .await;
        drop(out);
        if let Err(e) = copied {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
        tokio::fs::rename(&tmp, dest).await?;
        info!(
            "Fetched backup {} of {} from the bucket",
            backup_id, container_id
        );
        Ok(())
    }

    /// Remove a backup's objects; already gone is fine.
    pub async fn delete(&self, container_id: &str, backup_id: &str) -> Result<()> {
        let client = self.client().await?;
        let (data_key, record_key, _) = self.keys(container_id, backup_id).await;
        for key in [data_key, record_key] {
            match client.delete(&key).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => {
                    let err = NodeError::Internal(format!("delete {}: {}", key, describe(e)));
                    self.record_err(&err).await;
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    /// Remove everything under a server's prefix.
    pub async fn delete_all(&self, container_id: &str) -> Result<usize> {
        let client = self.client().await?;
        let prefix = {
            let s = self.settings.read().await;
            ObjectPath::from(format!("{}/{}", s.prefix, container_id))
        };
        let mut listing = client.list(Some(&prefix));
        let mut removed = 0;
        while let Some(meta) = listing.next().await {
            let meta = meta.map_err(|e| NodeError::Internal(format!("list: {}", describe(e))))?;
            match client.delete(&meta.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => removed += 1,
                Err(e) => {
                    return Err(NodeError::Internal(format!(
                        "delete {}: {}",
                        meta.location,
                        describe(e)
                    )))
                }
            }
        }
        Ok(removed)
    }

    /// Every backup record in the bucket, for adopting after a rebuild.
    pub async fn records(&self) -> Result<Vec<BackupInfo>> {
        let client = self.client().await?;
        let prefix = ObjectPath::from(self.settings.read().await.prefix.clone());
        let mut listing = client.list(Some(&prefix));
        let mut records = Vec::new();
        while let Some(meta) = listing.next().await {
            let meta = meta.map_err(|e| NodeError::Internal(format!("list: {}", describe(e))))?;
            let key = meta.location.to_string();
            if !key.ends_with(".json") {
                continue;
            }
            let bytes = match client.get(&meta.location).await {
                Ok(r) => match r.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("Could not read {}: {}", key, describe(e));
                        continue;
                    }
                },
                Err(e) => {
                    warn!("Could not read {}: {}", key, describe(e));
                    continue;
                }
            };
            match serde_json::from_slice::<BackupInfo>(&bytes) {
                Ok(mut info)
                    if key.ends_with(&format!("/{}/{}.json", info.container_id, info.id)) =>
                {
                    if info.remote.is_none() {
                        info.remote = Some(RemoteRef {
                            bucket: self.settings.read().await.bucket.clone(),
                            key: key.trim_end_matches(".json").to_string() + ".tar.gz",
                            uploaded_at: meta.last_modified.timestamp(),
                        });
                    }
                    records.push(info);
                }
                _ => warn!("Skipping {}: not a backup record", key),
            }
        }
        self.record_ok().await;
        Ok(records)
    }
}

/// Feed a file into a multipart upload without holding more than a few
/// parts in memory.
async fn pump(file: &mut tokio::fs::File, writer: &mut WriteMultipart) -> Result<()> {
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write(&buf[..n]);
        writer
            .wait_for_capacity(PARALLEL_PARTS)
            .await
            .map_err(|e| NodeError::Internal(format!("upload part: {}", describe(e))))?;
    }
    Ok(())
}

/// Provider errors mention the request in full; keep the useful line.
fn describe(e: object_store::Error) -> String {
    let text = e.to_string();
    match text.split_once("\n") {
        Some((first, _)) => first.trim().to_string(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::BackupStatus;
    use object_store::memory::InMemory;

    fn record(container: &str, id: &str) -> BackupInfo {
        BackupInfo {
            id: id.to_string(),
            container_id: container.to_string(),
            name: "nightly".to_string(),
            size: 5,
            created_at: 1_700_000_000,
            checksum: "abc".to_string(),
            status: BackupStatus::Completed,
            error: None,
            include: vec![],
            exclude: vec![],
            remote: None,
            remote_error: None,
        }
    }

    #[test]
    fn settings_validate_and_hide_the_secret() {
        let ok = RemoteSettings {
            enabled: true,
            bucket: "my-backups".into(),
            endpoint: Some("https://s3.eu-central-1.wasabisys.com".into()),
            access_key: Some("AKIA".into()),
            secret_key: Some("shh".into()),
            prefix: "/nodes/one/".into(),
            ..Default::default()
        }
        .normalized();
        ok.validate().unwrap();
        assert_eq!(ok.prefix, "nodes/one");
        let view = ok.view();
        assert!(view.has_secret_key);
        assert!(!serde_json::to_string(&view).unwrap().contains("shh"));

        for bad in [
            RemoteSettings {
                bucket: "UP".into(),
                ..ok.clone()
            },
            RemoteSettings {
                endpoint: Some("ftp://x".into()),
                ..ok.clone()
            },
            RemoteSettings {
                secret_key: None,
                ..ok.clone()
            },
            RemoteSettings {
                prefix: "a/../b".into(),
                ..ok.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{:?}", bad.bucket);
        }
        // Disabled settings need not be complete.
        RemoteSettings::default().validate().unwrap();
    }

    #[tokio::test]
    async fn round_trip_through_a_memory_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let store = RemoteBackupStore::with_backend(dir.path(), backend.clone(), "nexus");
        assert!(store.probe().await.ok);

        let archive = dir.path().join("a.tar.gz");
        let payload: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&archive, &payload).unwrap();
        let info = record("srv-1", "b1");
        let r = store.upload(&info, &archive).await.unwrap();
        assert_eq!(r.key, "nexus/srv-1/b1.tar.gz");
        assert!(store.status().await.last_error.is_none());

        let records = store.records().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "b1");
        assert_eq!(records[0].remote.as_ref().unwrap().key, r.key);

        let dest = dir.path().join("restored").join("b1.tar.gz");
        store.download("srv-1", "b1", &dest).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);

        store.delete("srv-1", "b1").await.unwrap();
        store.delete("srv-1", "b1").await.unwrap();
        assert!(store.records().await.unwrap().is_empty());
        assert!(store.download("srv-1", "b1", &dest).await.is_err());
        assert!(store.status().await.last_error.is_some());

        store.upload(&record("srv-2", "x"), &archive).await.unwrap();
        store.upload(&record("srv-2", "y"), &archive).await.unwrap();
        assert_eq!(store.delete_all("srv-2").await.unwrap(), 4);
        assert!(store.records().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn settings_persist_with_the_secret_kept_on_a_blank_update() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteBackupStore::load(dir.path());
        assert!(!store.enabled().await);
        let view = RemoteSettingsView {
            enabled: true,
            bucket: "backups".into(),
            endpoint: Some("http://127.0.0.1:9000".into()),
            region: None,
            access_key: Some("minio".into()),
            has_secret_key: false,
            prefix: "nexus".into(),
            path_style: true,
            keep_local: false,
        };
        let saved = store
            .update(RemoteSettingsUpdate {
                view: view.clone(),
                secret_key: Some("minio123".into()),
            })
            .await
            .unwrap();
        assert!(saved.has_secret_key);
        assert!(store.enabled().await);
        assert!(!store.keep_local().await);

        // Blank secret on the next update keeps the stored one.
        let saved = store
            .update(RemoteSettingsUpdate {
                view: RemoteSettingsView {
                    prefix: "other".into(),
                    ..view.clone()
                },
                secret_key: Some("".into()),
            })
            .await
            .unwrap();
        assert!(saved.has_secret_key);
        assert_eq!(
            store.settings().await.secret_key.as_deref(),
            Some("minio123")
        );

        // Missing key pair is refused, and nothing changes.
        assert!(store
            .update(RemoteSettingsUpdate {
                view: RemoteSettingsView {
                    access_key: None,
                    ..view.clone()
                },
                secret_key: None,
            })
            .await
            .is_err());
        assert_eq!(store.view().await.prefix, "other");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                std::fs::metadata(dir.path().join(SETTINGS_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let again = RemoteBackupStore::load(dir.path());
        assert_eq!(again.view().await.prefix, "other");
        assert!(again.enabled().await);
        assert!(!again.reset().await.unwrap().enabled);
        assert!(!RemoteBackupStore::load(dir.path()).enabled().await);
    }
}
