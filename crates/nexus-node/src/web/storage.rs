//! Moving files in and out of a server: uploads and downloads that stream
//! rather than buffer, archives, and backup downloads.

use axum::{
    body::Body,
    extract::{ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use futures::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use super::{
    audit, client_ip, container_event, err_json, file_manager, node_err_status, ApiError, S,
};
use crate::web::auth::SessionScope;

type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

#[derive(Deserialize)]
pub struct UploadQuery {
    /// The directory to upload into.
    pub path: Option<String>,
    /// The file name.
    pub name: String,
}

/// Upload one file, streamed from the request body straight to disk. The
/// bytes land in a temporary file beside the target and are moved into
/// place when the body ends, so a dropped connection leaves no half file.
/// A server with a disk allowance cannot be pushed over it.
pub(super) async fn api_upload_file(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> ApiResult<Json<serde_json::Value>> {
    if q.name.is_empty() || q.name.contains('/') || q.name == "." || q.name == ".." {
        return Err(err_json(StatusCode::BAD_REQUEST, "invalid file name"));
    }
    let state = s
        .manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    // Refuse up front what the disk allowance cannot take.
    let declared: u64 = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let allowance = if state.disk_limit_bytes > 0 {
        Some(state.disk_limit_bytes.saturating_sub(state.disk_used_bytes))
    } else {
        None
    };
    if let Some(room) = allowance {
        if declared > room {
            return Err(err_json(
                StatusCode::INSUFFICIENT_STORAGE,
                format!(
                    "upload of {} bytes exceeds the server's remaining disk allowance of {} bytes",
                    declared, room
                ),
            ));
        }
    }

    let dir = q.path.as_deref().unwrap_or("/");
    let rel = format!("{}/{}", dir.trim_end_matches('/'), q.name);
    let fm = file_manager(&s, &id);
    let target = fm
        .resolve_for_write(&rel)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let tmp = target.with_file_name(format!(
        ".{}.upload-{}",
        q.name,
        uuid::Uuid::new_v4().simple()
    ));

    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = write_body(body, &tmp, &target, allowance).await;
    let event = container_event(
        crate::audit::AuditEventType::FileWritten,
        "file.upload",
        &scope,
        &ip,
        &id,
    )
    .with_context("path", &rel);
    match &result {
        Ok(size) => {
            fm.claim_path(&target);
            audit(&s, event.with_context("bytes", *size).success()).await;
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            audit(&s, event.failure(e.to_string())).await;
        }
    }
    let size = result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(
        serde_json::json!({ "path": rel, "bytes_written": size }),
    ))
}

async fn write_body(
    body: Body,
    tmp: &std::path::Path,
    target: &std::path::Path,
    allowance: Option<u64>,
) -> crate::error::Result<u64> {
    let mut file = tokio::fs::File::create(tmp).await?;
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| crate::error::NodeError::Internal(format!("upload interrupted: {}", e)))?;
        written += chunk.len() as u64;
        if let Some(room) = allowance {
            if written > room {
                return Err(crate::error::NodeError::InvalidInput(
                    "upload exceeds the server's disk allowance".to_string(),
                ));
            }
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(tmp, target).await?;
    Ok(written)
}

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
}

/// Download one file, streamed. Directories are refused: compress them
/// first.
pub(super) async fn api_download_file(
    State(s): State<S>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Response> {
    let fm = file_manager(&s, &id);
    let path = fm
        .resolve_path(&q.path)
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| err_json(StatusCode::NOT_FOUND, format!("{} not found", q.path)))?;
    if meta.is_dir() {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "cannot download a directory; compress it first",
        ));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    stream_file(&path, &name, meta.len()).await
}

/// Download a backup archive.
pub(super) async fn api_download_backup(
    State(s): State<S>,
    Path((id, backup_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let backup = s
        .backup_manager
        .get_backup(&id, &backup_id)
        .await
        .map_err(|e| err_json(StatusCode::NOT_FOUND, e.to_string()))?;
    if backup.status != crate::backup::BackupStatus::Completed {
        return Err(err_json(StatusCode::CONFLICT, "backup is not complete"));
    }
    let path = s.backup_manager.get_backup_path(&id, &backup_id);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| err_json(StatusCode::NOT_FOUND, "backup file is missing"))?;
    let safe_name: String = backup
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = format!(
        "{}-{}.tar.gz",
        safe_name,
        &backup_id[..8.min(backup_id.len())]
    );
    stream_file(&path, &name, meta.len()).await
}

async fn stream_file(path: &std::path::Path, name: &str, len: u64) -> ApiResult<Response> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mime = mime_guess::from_path(path).first_or_octet_stream().to_string();
    let disposition = format!("attachment; filename=\"{}\"", name.replace('"', ""));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct CompressReq {
    pub paths: Vec<String>,
    /// The archive to write; its extension picks the format (`.zip`,
    /// `.tar.gz`, `.tgz`).
    pub destination: String,
}

/// Compress files into an archive inside the server directory.
pub(super) async fn api_compress(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<CompressReq>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.paths.is_empty() {
        return Err(err_json(StatusCode::BAD_REQUEST, "nothing to compress"));
    }
    let lower = req.destination.to_ascii_lowercase();
    let format = if lower.ends_with(".zip") {
        "zip"
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        "tar.gz"
    } else {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "destination must end in .zip, .tar.gz or .tgz",
        ));
    };
    let fm = file_manager(&s, &id);
    let result = fm.compress(&req.paths, &req.destination, format).await;
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let event = container_event(
        crate::audit::AuditEventType::FileWritten,
        "file.compress",
        &scope,
        &ip,
        &id,
    )
    .with_context("destination", &req.destination)
    .with_context("paths", &req.paths);
    match &result {
        Ok(_) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    let (path, size) = result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "path": path, "size": size })))
}

#[derive(Deserialize)]
pub struct DecompressReq {
    pub path: String,
    /// Where to extract; defaults to the archive's own directory.
    pub destination: Option<String>,
    pub overwrite: Option<bool>,
}

/// Extract an archive inside the server directory. Entries that would land
/// outside it are skipped.
pub(super) async fn api_decompress(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<DecompressReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let destination = req.destination.clone().unwrap_or_else(|| match req.path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => req.path[..i].to_string(),
    });
    let fm = file_manager(&s, &id);
    let result = fm.decompress(&req.path, &destination, req.overwrite.unwrap_or(true)).await;
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let event = container_event(
        crate::audit::AuditEventType::FileWritten,
        "file.decompress",
        &scope,
        &ip,
        &id,
    )
    .with_context("archive", &req.path)
    .with_context("destination", &destination);
    match &result {
        Ok(_) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    let count = result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(
        serde_json::json!({ "extracted": count, "destination": destination }),
    ))
}
