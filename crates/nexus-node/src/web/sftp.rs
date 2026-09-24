//! A server's SFTP access: where to connect, what username to use, and the
//! server's own SFTP password.

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use serde::Deserialize;

use super::{audit, client_ip, container_event, err_json, node_err_status, ApiError, S};
use crate::audit::AuditEventType;
use crate::sftp::SftpInfo;
use crate::web::auth::SessionScope;

type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

/// Where and how this session connects to the server over SFTP.
pub(super) async fn api_sftp_info(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    Path(id): Path<String>,
) -> ApiResult<Json<SftpInfo>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    // A panel account logs in as itself; a customer uses the server's own
    // password. API keys have no SFTP login of their own.
    let account = scope.subject().filter(|u| !u.starts_with("key:"));
    Ok(Json(s.sftp.info(&id, account).await))
}

#[derive(Deserialize)]
pub struct SftpPasswordReq {
    pub password: String,
}

/// Set the server's SFTP password (what a customer logs in with).
pub(super) async fn api_set_sftp_password(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SftpPasswordReq>,
) -> ApiResult<Json<SftpInfo>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let result = s.sftp.credentials.set(&id, &req.password).await;
    let event = container_event(
        AuditEventType::ConfigurationChanged,
        "sftp.password",
        &scope,
        &client_ip(peer.map(|c| c.0), &headers),
        &id,
    );
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let account = scope.subject().filter(|u| !u.starts_with("key:"));
    Ok(Json(s.sftp.info(&id, account).await))
}

/// Remove the server's SFTP password; account logins still work.
pub(super) async fn api_clear_sftp_password(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<SftpInfo>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    s.sftp
        .credentials
        .clear(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        container_event(
            AuditEventType::ConfigurationChanged,
            "sftp.password_cleared",
            &scope,
            &client_ip(peer.map(|c| c.0), &headers),
            &id,
        )
        .success(),
    )
    .await;
    let account = scope.subject().filter(|u| !u.starts_with("key:"));
    Ok(Json(s.sftp.info(&id, account).await))
}
