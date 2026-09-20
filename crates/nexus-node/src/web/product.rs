//! Branding, notification and off-node backup settings: the operator's, and
//! a server owner's own webhook.

use std::collections::HashMap;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use serde::Serialize;

use super::{actor_for, audit, client_ip, container_event, err_json, node_err_status, ApiError, S};
use crate::audit::{AuditEvent, AuditEventType};
use crate::branding::Brand;
use crate::notify::{ChannelStatus, OperatorChannels, ServerHook, EVENT_KINDS};
use crate::remote_backup::{ProbeResult, RemoteSettingsUpdate, RemoteSettingsView, RemoteStatus};
use crate::web::auth::SessionScope;

type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

// ── Branding ────────────────────────────────────────────────────────

pub(super) async fn api_get_branding(State(s): State<S>) -> Json<Brand> {
    Json(s.brand.get().await)
}

pub(super) async fn api_set_branding(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(brand): Json<Brand>,
) -> ApiResult<Json<Brand>> {
    let saved = s
        .brand
        .set(brand)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "branding.update")
            .with_actor(actor_for(&scope))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .with_context("name", &saved.name)
            .success(),
    )
    .await;
    Ok(Json(saved))
}

pub(super) async fn api_reset_branding(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
) -> Json<Brand> {
    let brand = s.brand.reset().await;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "branding.reset")
            .with_actor(actor_for(&scope))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .success(),
    )
    .await;
    Json(brand)
}

// ── Operator notifications ──────────────────────────────────────────

#[derive(Serialize)]
pub struct NotificationsResp {
    #[serde(flatten)]
    pub channels: OperatorChannels,
    /// Delivery outcome per channel.
    pub status: HashMap<String, ChannelStatus>,
    /// The event kinds a server hook may subscribe to.
    pub event_kinds: &'static [&'static str],
}

pub(super) async fn api_get_notifications(State(s): State<S>) -> Json<NotificationsResp> {
    Json(NotificationsResp {
        channels: s.notifier.operator().await,
        status: s.notifier.status().await,
        event_kinds: EVENT_KINDS,
    })
}

pub(super) async fn api_set_notifications(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(channels): Json<OperatorChannels>,
) -> ApiResult<Json<NotificationsResp>> {
    s.notifier
        .set_operator(channels)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let saved = s.notifier.operator().await;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "notifications.update")
            .with_actor(actor_for(&scope))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .with_context("webhooks", saved.webhooks.len())
            .with_context("emails", saved.emails.len())
            .with_context("smtp", saved.smtp_url.is_some())
            .success(),
    )
    .await;
    Ok(Json(NotificationsResp {
        channels: saved,
        status: s.notifier.status().await,
        event_kinds: EVENT_KINDS,
    }))
}

/// Send a test message to every operator channel and report what happened.
pub(super) async fn api_test_notifications(
    State(s): State<S>,
) -> Json<HashMap<String, ChannelStatus>> {
    Json(s.notifier.test().await)
}

// ── A server's own webhook ──────────────────────────────────────────

#[derive(Serialize)]
pub struct ServerHookResp {
    #[serde(flatten)]
    pub hook: ServerHook,
    pub event_kinds: &'static [&'static str],
}

pub(super) async fn api_get_server_hook(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<Json<ServerHookResp>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(ServerHookResp {
        hook: s.notifier.server_hook(&id).await,
        event_kinds: EVENT_KINDS,
    }))
}

pub(super) async fn api_set_server_hook(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(hook): Json<ServerHook>,
) -> ApiResult<Json<ServerHookResp>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    s.notifier
        .set_server_hook(&id, hook)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let saved = s.notifier.server_hook(&id).await;
    audit(
        &s,
        container_event(
            AuditEventType::ConfigurationChanged,
            "notifications.server",
            &scope,
            &client_ip(peer.map(|c| c.0), &headers),
            &id,
        )
        .with_context("has_webhook", saved.webhook_url.is_some())
        .with_context("events", &saved.events)
        .success(),
    )
    .await;
    Ok(Json(ServerHookResp {
        hook: saved,
        event_kinds: EVENT_KINDS,
    }))
}

// ── Off-node backups ────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RemoteBackupsResp {
    #[serde(flatten)]
    pub settings: RemoteSettingsView,
    pub status: RemoteStatus,
}

async fn remote_resp(s: &S) -> RemoteBackupsResp {
    RemoteBackupsResp {
        settings: s.remote_backups.view().await,
        status: s.remote_backups.status().await,
    }
}

pub(super) async fn api_get_remote_backups(State(s): State<S>) -> Json<RemoteBackupsResp> {
    Json(remote_resp(&s).await)
}

pub(super) async fn api_set_remote_backups(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(update): Json<RemoteSettingsUpdate>,
) -> ApiResult<Json<RemoteBackupsResp>> {
    let saved = s
        .remote_backups
        .update(update)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(
            AuditEventType::ConfigurationChanged,
            "backups.remote.update",
        )
        .with_actor(actor_for(&scope))
        .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
        .with_context("enabled", saved.enabled)
        .with_context("bucket", &saved.bucket)
        .with_context("endpoint", saved.endpoint.clone().unwrap_or_default())
        .with_context("keep_local", saved.keep_local)
        .success(),
    )
    .await;
    Ok(Json(remote_resp(&s).await))
}

pub(super) async fn api_reset_remote_backups(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
) -> ApiResult<Json<RemoteBackupsResp>> {
    s.remote_backups
        .reset()
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "backups.remote.reset")
            .with_actor(actor_for(&scope))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .success(),
    )
    .await;
    Ok(Json(remote_resp(&s).await))
}

/// Write, read back and delete a small object in the bucket.
pub(super) async fn api_test_remote_backups(State(s): State<S>) -> Json<ProbeResult> {
    Json(s.remote_backups.probe().await)
}

/// Adopt records in the bucket this node does not have.
pub(super) async fn api_sync_remote_backups(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let adopted = s
        .backup_manager
        .sync_from_remote()
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "backups.remote.sync")
            .with_actor(actor_for(&scope))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .with_context("adopted", adopted)
            .success(),
    )
    .await;
    Ok(Json(serde_json::json!({ "adopted": adopted })))
}
