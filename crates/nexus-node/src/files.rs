//! File management service for container server files
//!
//! Provides secure file operations within container data directories including:
//! - Directory listing and navigation
//! - File read/write operations
//! - File/directory creation, deletion, rename, copy
//! - Archive compression and decompression
//! - Streaming upload/download

use crate::error::{NodeError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

/// Maximum file size for direct read operations (10MB)
const MAX_DIRECT_READ_SIZE: u64 = 10 * 1024 * 1024;

/// Chunk size for streaming operations (64KB)
const CHUNK_SIZE: usize = 64 * 1024;

/// File information structure
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
    pub modified_at: i64,
    pub mime_type: String,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
}

/// File manager for a specific container
pub struct FileManager {
    /// Container ID
    container_id: String,

    /// Base data directory
    _data_dir: PathBuf,

    /// Container-specific directory
    server_dir: PathBuf,

    /// Who should own what this manager creates: the user the game runs as.
    /// The node runs as root, and a file it writes as root is one the game
    /// can no longer modify.
    owner: Option<(u32, u32)>,
}

impl FileManager {
    /// Create a new file manager for a container
    pub fn new(container_id: &str, data_dir: &Path) -> Self {
        let server_dir = data_dir.join(container_id);
        Self {
            container_id: container_id.to_string(),
            _data_dir: data_dir.to_path_buf(),
            server_dir,
            owner: None,
        }
    }

    /// Give everything this manager creates to `uid:gid`.
    pub fn with_owner(mut self, owner: (u32, u32)) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Resolve a path for a write that will happen outside this manager (a
    /// streamed upload), creating its parent directories. The caller
    /// finishes with [`claim_path`](Self::claim_path).
    pub async fn resolve_for_write(&self, path: &str) -> Result<PathBuf> {
        let file_path = self.sanitize_path(path)?;
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
            self.claim(parent);
        }
        Ok(file_path)
    }

    /// Hand a path this manager did not itself write to the game's user.
    pub fn claim_path(&self, path: &Path) {
        self.claim(path);
    }

    /// Hand a path (and, for a directory, its contents) to the owner.
    /// Best-effort: a node not running as root cannot, and its game runs as
    /// the same user anyway.
    fn claim(&self, path: &Path) {
        let Some((uid, gid)) = self.owner else {
            return;
        };
        for entry in walkdir::WalkDir::new(path).follow_links(false).into_iter().flatten() {
            if let Err(e) = std::os::unix::fs::lchown(entry.path(), Some(uid), Some(gid)) {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    return;
                }
                warn!("Could not chown {:?}: {}", entry.path(), e);
            }
        }
    }

    /// Validate and sanitize a path to prevent directory traversal.
    ///
    /// This rejects any `..` (parent-dir), absolute-root, or prefix components
    /// *lexically*, before touching the filesystem. That is essential because
    /// [`Path::canonicalize`] fails for paths that do not exist yet (creating a
    /// new file, making a directory, or the destination of a rename), and a
    /// canonicalize-or-fall-back approach would let `../` escape the jail on
    /// exactly those write operations. As defense in depth we then resolve the
    /// nearest existing ancestor through symlinks and confirm it is still
    /// contained within the (canonicalized) server directory.
    fn sanitize_path(&self, path: &str) -> Result<PathBuf> {
        use std::path::Component;

        // Strip leading slashes so the input is always treated as relative.
        let rel = path.trim_start_matches('/');

        // Rebuild the path from only "normal" components. Any attempt to
        // ascend (`..`), anchor to root, or use a Windows prefix is rejected
        // outright — this is what makes the check safe for non-existent targets.
        let mut normalized = PathBuf::new();
        for component in Path::new(rel).components() {
            match component {
                Component::Normal(c) => normalized.push(c),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(NodeError::InvalidInput(format!(
                        "Path traversal attempt detected: {}",
                        path
                    )));
                }
            }
        }

        let full_path = self.server_dir.join(&normalized);

        // Defense in depth against symlinks: resolve the path (or, if it does
        // not exist yet, its nearest existing ancestor) and require the
        // resolved location to stay inside the server directory.
        let base = self.server_dir.canonicalize().unwrap_or_else(|_| self.server_dir.clone());

        let resolved = if full_path.exists() {
            full_path.canonicalize().ok()
        } else {
            full_path.ancestors().find(|p| p.exists()).and_then(|p| p.canonicalize().ok())
        };

        if let Some(resolved) = resolved {
            if !resolved.starts_with(&base) {
                return Err(NodeError::InvalidInput(format!(
                    "Path traversal attempt detected: {}",
                    path
                )));
            }
        }

        Ok(full_path)
    }

    /// Resolve a caller-supplied relative path to a jail-safe absolute path
    /// inside this container's directory. Rejects traversal the same way the
    /// rest of the file API does; does not require the path to exist yet.
    pub fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        self.sanitize_path(path)
    }

    /// List files in a directory
    pub async fn list_files(&self, path: &str) -> Result<Vec<FileInfo>> {
        let dir_path = self.sanitize_path(path)?;

        // Ensure directory exists
        if !dir_path.exists() {
            return Err(NodeError::InvalidInput(format!(
                "Directory does not exist: {}",
                path
            )));
        }

        if !dir_path.is_dir() {
            return Err(NodeError::InvalidInput(format!(
                "Path is not a directory: {}",
                path
            )));
        }

        let mut files = Vec::new();
        let mut entries = fs::read_dir(&dir_path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let metadata = entry.metadata().await?;
            let file_type = entry.file_type().await?;

            let name = entry.file_name().to_string_lossy().to_string();
            let relative_path = entry
                .path()
                .strip_prefix(&self.server_dir)
                .map(|p| format!("/{}", p.display()))
                .unwrap_or_else(|_| name.clone());

            let mime_type = if metadata.is_file() {
                mime_guess::from_path(&name).first_or_octet_stream().to_string()
            } else {
                "inode/directory".to_string()
            };

            let modified_at = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let symlink_target = if file_type.is_symlink() {
                fs::read_link(entry.path()).await.ok().map(|p| p.display().to_string())
            } else {
                None
            };

            files.push(FileInfo {
                name,
                path: relative_path,
                is_directory: metadata.is_dir(),
                size: metadata.len(),
                modified_at,
                mime_type,
                is_symlink: file_type.is_symlink(),
                symlink_target,
            });
        }

        // Sort: directories first, then by name
        files.sort_by(|a, b| match (a.is_directory, b.is_directory) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });

        Ok(files)
    }

    /// Read file contents
    pub async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        length: Option<u64>,
    ) -> Result<(Vec<u8>, u64, String)> {
        let file_path = self.sanitize_path(path)?;

        if !file_path.exists() {
            return Err(NodeError::InvalidInput(format!(
                "File does not exist: {}",
                path
            )));
        }

        if !file_path.is_file() {
            return Err(NodeError::InvalidInput(format!(
                "Path is not a file: {}",
                path
            )));
        }

        let metadata = fs::metadata(&file_path).await?;
        let total_size = metadata.len();

        // Check file size
        let read_length = length.unwrap_or(total_size);
        if read_length > MAX_DIRECT_READ_SIZE {
            return Err(NodeError::InvalidInput(format!(
                "File too large for direct read ({}MB max). Use streaming download.",
                MAX_DIRECT_READ_SIZE / 1024 / 1024
            )));
        }

        let mime_type = mime_guess::from_path(&file_path).first_or_octet_stream().to_string();

        let mut file = fs::File::open(&file_path).await?;

        // Seek to offset if specified
        if let Some(off) = offset {
            use tokio::io::AsyncSeekExt;
            file.seek(std::io::SeekFrom::Start(off)).await?;
        }

        // Read content
        let mut content = vec![0u8; read_length as usize];
        let bytes_read = file.read(&mut content).await?;
        content.truncate(bytes_read);

        Ok((content, total_size, mime_type))
    }

    /// Write file contents
    pub async fn write_file(&self, path: &str, content: &[u8], create_dirs: bool) -> Result<u64> {
        let file_path = self.sanitize_path(path)?;

        // Create parent directories if requested
        if create_dirs {
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).await?;
            }
        }

        let mut file = fs::File::create(&file_path).await?;
        file.write_all(content).await?;
        file.flush().await?;
        drop(file);
        self.claim(&file_path);

        info!(
            "Wrote {} bytes to file {} in container {}",
            content.len(),
            path,
            self.container_id
        );

        Ok(content.len() as u64)
    }

    /// Delete files or directories
    pub async fn delete(&self, paths: &[String], recursive: bool) -> Result<u32> {
        let mut deleted = 0;

        for path in paths {
            let file_path = self.sanitize_path(path)?;

            if !file_path.exists() {
                warn!("Path does not exist, skipping: {}", path);
                continue;
            }

            if file_path.is_dir() {
                if recursive {
                    fs::remove_dir_all(&file_path).await?;
                } else {
                    fs::remove_dir(&file_path).await?;
                }
            } else {
                fs::remove_file(&file_path).await?;
            }

            info!("Deleted {} in container {}", path, self.container_id);
            deleted += 1;
        }

        Ok(deleted)
    }

    /// Rename a file or directory
    pub async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let old = self.sanitize_path(old_path)?;
        let new = self.sanitize_path(new_path)?;

        if !old.exists() {
            return Err(NodeError::InvalidInput(format!(
                "Source does not exist: {}",
                old_path
            )));
        }

        fs::rename(&old, &new).await?;

        info!(
            "Renamed {} to {} in container {}",
            old_path, new_path, self.container_id
        );

        Ok(())
    }

    /// Copy a file or directory
    pub async fn copy(&self, source: &str, dest: &str, overwrite: bool) -> Result<()> {
        let src_path = self.sanitize_path(source)?;
        let dst_path = self.sanitize_path(dest)?;

        if !src_path.exists() {
            return Err(NodeError::InvalidInput(format!(
                "Source does not exist: {}",
                source
            )));
        }

        if dst_path.exists() && !overwrite {
            return Err(NodeError::InvalidInput(format!(
                "Destination already exists: {}",
                dest
            )));
        }

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path).await?;
        } else {
            fs::copy(&src_path, &dst_path).await?;
        }
        self.claim(&dst_path);

        info!(
            "Copied {} to {} in container {}",
            source, dest, self.container_id
        );

        Ok(())
    }

    /// Create a directory
    pub async fn create_directory(&self, path: &str, recursive: bool) -> Result<()> {
        let dir_path = self.sanitize_path(path)?;

        if recursive {
            fs::create_dir_all(&dir_path).await?;
        } else {
            fs::create_dir(&dir_path).await?;
        }
        self.claim(&dir_path);

        info!(
            "Created directory {} in container {}",
            path, self.container_id
        );

        Ok(())
    }

    /// Compress files into an archive
    pub async fn compress(
        &self,
        paths: &[String],
        output: &str,
        format: &str,
    ) -> Result<(String, u64)> {
        let output_path = self.sanitize_path(output)?;

        // Collect source paths
        let mut sources: Vec<PathBuf> = Vec::new();
        for path in paths {
            sources.push(self.sanitize_path(path)?);
        }

        // Create archive based on format
        match format {
            "tar.gz" | "tgz" => {
                create_tar_gz(&sources, &output_path, &self.server_dir).await?;
            }
            "zip" => {
                create_zip(&sources, &output_path, &self.server_dir).await?;
            }
            _ => {
                return Err(NodeError::InvalidInput(format!(
                    "Unsupported archive format: {}. Use tar.gz or zip",
                    format
                )));
            }
        }

        self.claim(&output_path);
        let metadata = fs::metadata(&output_path).await?;

        info!(
            "Compressed {} files to {} ({} bytes) in container {}",
            paths.len(),
            output,
            metadata.len(),
            self.container_id
        );

        Ok((output.to_string(), metadata.len()))
    }

    /// Decompress an archive
    pub async fn decompress(
        &self,
        archive: &str,
        output_dir: &str,
        overwrite: bool,
    ) -> Result<u32> {
        let archive_path = self.sanitize_path(archive)?;
        let output_path = self.sanitize_path(output_dir)?;

        if !archive_path.exists() {
            return Err(NodeError::InvalidInput(format!(
                "Archive does not exist: {}",
                archive
            )));
        }

        // Create output directory
        fs::create_dir_all(&output_path).await?;

        // Determine format from extension
        let archive_str = archive.to_lowercase();
        let count = if archive_str.ends_with(".tar.gz") || archive_str.ends_with(".tgz") {
            extract_tar_gz(&archive_path, &output_path, overwrite).await?
        } else if archive_str.ends_with(".zip") {
            extract_zip(&archive_path, &output_path, overwrite).await?
        } else {
            return Err(NodeError::InvalidInput(
                "Unsupported archive format. Use .tar.gz, .tgz, or .zip".to_string(),
            ));
        };

        self.claim(&output_path);

        info!(
            "Extracted {} files from {} to {} in container {}",
            count, archive, output_dir, self.container_id
        );

        Ok(count)
    }

    /// Get server directory path
    pub fn server_dir(&self) -> &Path {
        &self.server_dir
    }

    /// Calculate SHA256 checksum of a file
    pub async fn checksum(&self, path: &str) -> Result<String> {
        let file_path = self.sanitize_path(path)?;
        let mut file = fs::File::open(&file_path).await?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; CHUNK_SIZE];

        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }
}

/// Recursively copy a directory
/// Copy a directory tree. Symlinks are recreated as symlinks, never
/// followed: a link the game planted pointing outside its directory must not
/// become a copy of what it points at.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).await?;

    let mut entries = fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let meta = fs::symlink_metadata(&src_path).await?;

        if meta.file_type().is_symlink() {
            let target = fs::read_link(&src_path).await?;
            let _ = fs::remove_file(&dst_path).await;
            fs::symlink(&target, &dst_path).await?;
        } else if meta.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            fs::copy(&src_path, &dst_path).await?;
        }
    }

    Ok(())
}

/// Create a tar.gz archive
async fn create_tar_gz(sources: &[PathBuf], output: &Path, base_dir: &Path) -> Result<()> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tar::Builder;

    let file = std::fs::File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);

    for source in sources {
        let rel_path = source.strip_prefix(base_dir).unwrap_or(source);
        if source.is_dir() {
            archive.append_dir_all(rel_path, source)?;
        } else {
            archive.append_path_with_name(source, rel_path)?;
        }
    }

    archive.finish()?;
    Ok(())
}

/// Create a zip archive
async fn create_zip(sources: &[PathBuf], output: &Path, base_dir: &Path) -> Result<()> {
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let file = std::fs::File::create(output)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for source in sources {
        add_to_zip(&mut zip, source, base_dir, options)?;
    }

    zip.finish()?;
    Ok(())
}

/// Recursively add files to zip
fn add_to_zip<W: std::io::Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    path: &Path,
    base_dir: &Path,
    options: zip::write::SimpleFileOptions,
) -> Result<()> {
    let rel_path = path.strip_prefix(base_dir).unwrap_or(path);
    let rel_str = rel_path.to_string_lossy();

    if path.is_dir() {
        zip.add_directory(format!("{}/", rel_str), options)?;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            add_to_zip(zip, &entry.path(), base_dir, options)?;
        }
    } else {
        zip.start_file(rel_str.to_string(), options)?;
        let mut file = std::fs::File::open(path)?;
        std::io::copy(&mut file, zip)?;
    }

    Ok(())
}

/// Extract a tar.gz archive
async fn extract_tar_gz(archive: &Path, output: &Path, overwrite: bool) -> Result<u32> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = std::fs::File::open(archive)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut count = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = entry.path()?.into_owned();
        let path = output.join(&relative);

        if path.exists() && !overwrite {
            continue;
        }

        // `unpack_in` refuses entries that would land outside `output`
        // (`../`, absolute paths, links through a symlinked parent) rather
        // than writing wherever the archive says. A refused entry is simply
        // not extracted.
        if entry.unpack_in(output)? {
            count += 1;
        } else {
            warn!(
                "Skipped archive entry {:?}: it would escape the extraction directory",
                relative
            );
        }
    }

    Ok(count)
}

/// Extract a zip archive
async fn extract_zip(archive: &Path, output: &Path, overwrite: bool) -> Result<u32> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;

    let mut count = 0;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        // `enclosed_name` is the entry's name with traversal, absolute paths
        // and drive letters rejected; `name()` is whatever the archive says.
        let Some(relative) = file.enclosed_name() else {
            warn!(
                "Skipped zip entry {:?}: it would escape the extraction directory",
                file.name()
            );
            continue;
        };
        let path = output.join(relative);

        if path.exists() && !overwrite {
            continue;
        }

        if file.is_dir() {
            std::fs::create_dir_all(&path)?;
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut outfile = std::fs::File::create(&path)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_sanitize_path() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("test-container", temp.path());

        // Create the server directory
        fs::create_dir_all(&manager.server_dir).await.unwrap();

        // Valid path
        let result = manager.sanitize_path("config.yaml");
        assert!(result.is_ok());

        // Path traversal should fail
        let result = manager.sanitize_path("../../../etc/passwd");
        assert!(result.is_err());

        // Traversal must also be rejected when the target does NOT exist yet
        // (this is the write / mkdir / rename-destination case that a
        // canonicalize-or-fall-back check would let through).
        assert!(manager.sanitize_path("../../../../etc/cron.d/pwned").is_err());
        assert!(manager.sanitize_path("subdir/../../escape.txt").is_err());
        assert!(manager.sanitize_path("/../../escape.txt").is_err());

        // A nested path under the jail that does not exist yet is still allowed.
        assert!(manager.sanitize_path("logs/latest/server.log").is_ok());
    }

    #[tokio::test]
    async fn test_write_rejects_traversal_to_nonexistent_target() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("test-container", temp.path());
        fs::create_dir_all(&manager.server_dir).await.unwrap();

        // Attempt to write outside the jail via a non-existent target path.
        let result = manager
            .write_file("../../../../tmp/nexus-escape-test.txt", b"pwned", true)
            .await;
        assert!(result.is_err(), "traversal write should be rejected");

        // And mkdir must be rejected too.
        let result = manager.create_directory("../../escape-dir", true).await;
        assert!(result.is_err(), "traversal mkdir should be rejected");
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("test-container", temp.path());

        // Create the server directory
        fs::create_dir_all(&manager.server_dir).await.unwrap();

        // Write a file
        let content = b"Hello, World!";
        manager.write_file("test.txt", content, true).await.unwrap();

        // Read it back
        let (data, size, mime) = manager.read_file("test.txt", None, None).await.unwrap();
        assert_eq!(data, content);
        assert_eq!(size, content.len() as u64);
        assert_eq!(mime, "text/plain");
    }

    #[tokio::test]
    async fn test_list_files() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("test-container", temp.path());

        // Create the server directory and some files
        fs::create_dir_all(&manager.server_dir).await.unwrap();
        fs::write(manager.server_dir.join("file1.txt"), "test").await.unwrap();
        fs::write(manager.server_dir.join("file2.txt"), "test").await.unwrap();
        fs::create_dir(manager.server_dir.join("subdir")).await.unwrap();

        // List files
        let files = manager.list_files("/").await.unwrap();
        assert_eq!(files.len(), 3);

        // Directory should be first
        assert!(files[0].is_directory);
        assert_eq!(files[0].name, "subdir");
    }

    #[tokio::test]
    async fn test_create_directory() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("test-container", temp.path());

        // Create the server directory
        fs::create_dir_all(&manager.server_dir).await.unwrap();

        // Create nested directory
        manager.create_directory("path/to/nested", true).await.unwrap();

        assert!(manager.server_dir.join("path/to/nested").exists());
    }

    /// An archive naming `../` must not write outside the server directory.
    #[tokio::test]
    async fn tar_slip_is_refused() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("srv", temp.path());
        fs::create_dir_all(&manager.server_dir).await.unwrap();

        // Build a tar.gz with one honest entry and one escaping entry.
        let archive_path = manager.server_dir.join("evil.tar.gz");
        {
            use flate2::write::GzEncoder;
            let file = std::fs::File::create(&archive_path).unwrap();
            let enc = GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, "ok.txt", &b"hi"[..]).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            // append_data would refuse ".."; write the raw path via a GNU long
            // name the way a hostile archive does.
            let mut path_header = tar::Header::new_gnu();
            path_header.set_entry_type(tar::EntryType::GNULongName);
            let name = b"../../escaped.txt\0";
            path_header.set_size(name.len() as u64);
            path_header.set_cksum();
            builder.append(&path_header, &name[..]).unwrap();
            builder.append(&header, &b"evil"[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let count = manager.decompress("evil.tar.gz", "out", true).await.unwrap();
        assert_eq!(count, 1, "only the honest entry is extracted");
        assert!(manager.server_dir.join("out/ok.txt").exists());
        assert!(!temp.path().join("escaped.txt").exists());
        assert!(!temp.path().parent().unwrap().join("escaped.txt").exists());
    }

    #[tokio::test]
    async fn zip_slip_is_refused() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("srv", temp.path());
        fs::create_dir_all(&manager.server_dir).await.unwrap();

        let archive_path = manager.server_dir.join("evil.zip");
        {
            use std::io::Write;
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("ok.txt", opts).unwrap();
            zip.write_all(b"hi").unwrap();
            zip.start_file("../../escaped.txt", opts).unwrap();
            zip.write_all(b"evil").unwrap();
            zip.start_file("/etc/absolute.txt", opts).unwrap();
            zip.write_all(b"evil").unwrap();
            zip.finish().unwrap();
        }

        let count = manager.decompress("evil.zip", "out", true).await.unwrap();
        assert_eq!(count, 1);
        assert!(manager.server_dir.join("out/ok.txt").exists());
        assert!(!temp.path().join("escaped.txt").exists());
        assert!(!std::path::Path::new("/etc/absolute.txt").exists());
    }

    #[tokio::test]
    async fn copying_a_directory_keeps_symlinks_as_symlinks() {
        let temp = TempDir::new().unwrap();
        let manager = FileManager::new("srv", temp.path());
        fs::create_dir_all(manager.server_dir.join("src")).await.unwrap();
        // A link pointing outside the jail.
        std::os::unix::fs::symlink("/etc/hostname", manager.server_dir.join("src/leak")).unwrap();
        fs::write(manager.server_dir.join("src/real.txt"), b"x").await.unwrap();

        manager.copy("src", "dst", false).await.unwrap();
        let meta = fs::symlink_metadata(manager.server_dir.join("dst/leak")).await.unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "not dereferenced into a copy"
        );
        assert!(manager.server_dir.join("dst/real.txt").exists());
    }
}
