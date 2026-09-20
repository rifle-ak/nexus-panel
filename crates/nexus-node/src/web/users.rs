//! Accounts, API keys and the audit trail: the Users page.
//!
//! Everything here is for admins, except changing one's own password.
//! The scope middleware already refuses non-admin sessions at these paths.

use std::collections::BTreeMap;

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use super::{actor_for, audit, client_ip, err_json, node_err_status, ApiError, S};
use crate::audit::{AuditEvent, AuditEventType, AuditTarget, TargetType};
use crate::subuser::Permission;
use crate::users::{preset, ApiKey, Grants, User};
use crate::web::auth::SessionScope;

type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

#[derive(Serialize)]
pub struct UserJson {
    pub id: String,
    pub username: String,
    pub admin: bool,
    pub disabled: bool,
    /// `server id -> [permission.name, …]`.
    pub grants: BTreeMap<String, Vec<&'static str>>,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
}

fn user_json(u: User) -> UserJson {
    UserJson {
        id: u.id,
        username: u.username,
        admin: u.admin,
        disabled: u.disabled,
        grants: u
            .grants
            .iter()
            .map(|(id, ps)| (id.clone(), ps.iter().map(|p| p.as_str()).collect()))
            .collect(),
        created_at: u.created_at,
        last_login_at: u.last_login_at,
    }
}

/// Grants as the API takes them: per server, either a preset name or a
/// list of permission names.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum GrantSpec {
    Preset(String),
    List(Vec<String>),
}

fn parse_grants(spec: BTreeMap<String, GrantSpec>) -> ApiResult<Grants> {
    let mut grants = Grants::new();
    for (server, g) in spec {
        if server.trim().is_empty() {
            return Err(err_json(
                StatusCode::BAD_REQUEST,
                "empty server id in grants",
            ));
        }
        let set = match g {
            GrantSpec::Preset(name) => preset(&name).ok_or_else(|| {
                err_json(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "unknown preset {:?}; use read_only, default, operator or full",
                        name
                    ),
                )
            })?,
            GrantSpec::List(names) => names
                .iter()
                .map(|n| {
                    Permission::parse_permission(n).ok_or_else(|| {
                        err_json(
                            StatusCode::BAD_REQUEST,
                            format!("unknown permission {:?}", n),
                        )
                    })
                })
                .collect::<Result<_, _>>()?,
        };
        grants.insert(server, set);
    }
    Ok(grants)
}

fn user_target(id: &str, name: &str) -> AuditTarget {
    AuditTarget {
        target_type: TargetType::User,
        id: id.to_string(),
        name: Some(name.to_string()),
        attributes: Default::default(),
    }
}

fn key_target(key: &ApiKey) -> AuditTarget {
    AuditTarget {
        target_type: TargetType::ApiKey,
        id: key.id.clone(),
        name: Some(key.name.clone()),
        attributes: Default::default(),
    }
}

// ── Users ───────────────────────────────────────────────────────────

pub(super) async fn api_list_users(State(s): State<S>) -> Json<Vec<UserJson>> {
    Json(s.users.list_users().await.into_iter().map(user_json).collect())
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub grants: BTreeMap<String, GrantSpec>,
}

pub(super) async fn api_create_user(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(req): Json<CreateUserReq>,
) -> ApiResult<(StatusCode, Json<UserJson>)> {
    let grants = parse_grants(req.grants)?;
    let user = s
        .users
        .create_user(&req.username, &req.password, req.admin, grants)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "user.create")
            .with_actor(actor_for(&scope))
            .with_target(user_target(&user.id, &user.username))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .with_context("admin", user.admin)
            .success(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(user_json(user))))
}

#[derive(Deserialize)]
pub struct UpdateUserReq {
    pub password: Option<String>,
    pub admin: Option<bool>,
    pub grants: Option<BTreeMap<String, GrantSpec>>,
    pub disabled: Option<bool>,
}

pub(super) async fn api_update_user(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> ApiResult<Json<UserJson>> {
    let existing = s
        .users
        .get_user(&user_id)
        .await
        .ok_or_else(|| err_json(StatusCode::NOT_FOUND, "no such user"))?;
    // An admin cannot lock themselves out from their own session.
    if scope.subject() == Some(existing.username.as_str())
        && (req.disabled == Some(true) || req.admin == Some(false))
    {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "you cannot disable or demote your own account",
        ));
    }
    let grants = match req.grants {
        Some(g) => Some(parse_grants(g)?),
        None => None,
    };
    let user = s
        .users
        .update_user(
            &user_id,
            req.password.as_deref(),
            req.admin,
            grants,
            req.disabled,
        )
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "user.update")
            .with_actor(actor_for(&scope))
            .with_target(user_target(&user.id, &user.username))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .with_context("password_changed", req.password.is_some())
            .with_context("admin", user.admin)
            .with_context("disabled", user.disabled)
            .success(),
    )
    .await;
    Ok(Json(user_json(user)))
}

pub(super) async fn api_delete_user(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let existing = s
        .users
        .get_user(&user_id)
        .await
        .ok_or_else(|| err_json(StatusCode::NOT_FOUND, "no such user"))?;
    if scope.subject() == Some(existing.username.as_str()) {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "you cannot delete your own account",
        ));
    }
    s.users
        .delete_user(&user_id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ConfigurationChanged, "user.delete")
            .with_actor(actor_for(&scope))
            .with_target(user_target(&existing.id, &existing.username))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .success(),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct PermissionsResp {
    /// Every permission name a grant may carry.
    pub permissions: Vec<&'static str>,
    /// The presets, expanded.
    pub presets: BTreeMap<&'static str, Vec<&'static str>>,
}

/// The vocabulary, for the Users page.
pub(super) async fn api_permissions() -> Json<PermissionsResp> {
    let mut permissions: Vec<&'static str> =
        preset("full").unwrap_or_default().iter().map(|p| p.as_str()).collect();
    permissions.sort();
    let presets = ["read_only", "default", "operator", "full"]
        .into_iter()
        .map(|name| {
            let mut ps: Vec<&'static str> =
                preset(name).unwrap_or_default().iter().map(|p| p.as_str()).collect();
            ps.sort();
            (name, ps)
        })
        .collect();
    Json(PermissionsResp {
        permissions,
        presets,
    })
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub current_password: String,
    pub new_password: String,
}

/// Change the calling account's own password.
pub(super) async fn api_change_password(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(req): Json<ChangePasswordReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(username) = scope.subject().filter(|u| !u.starts_with("key:")) else {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "this session is not a panel account; the operator password is set in the node's environment",
        ));
    };
    let user = s
        .users
        .find_by_username(username)
        .await
        .ok_or_else(|| err_json(StatusCode::NOT_FOUND, "account no longer exists"))?;
    let result = s
        .users
        .change_password(&user.id, &req.current_password, &req.new_password)
        .await;
    let event = AuditEvent::new(AuditEventType::ConfigurationChanged, "user.password")
        .with_actor(actor_for(&scope))
        .with_target(user_target(&user.id, &user.username))
        .with_source_ip(client_ip(peer.map(|c| c.0), &headers));
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ── API keys ────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ApiKeyJson {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: i64,
    pub created_by: String,
    pub last_used_at: Option<i64>,
    /// The key itself, present only in the reply that created it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

fn key_json(k: ApiKey, secret: Option<String>) -> ApiKeyJson {
    ApiKeyJson {
        id: k.id,
        name: k.name,
        prefix: k.prefix,
        created_at: k.created_at,
        created_by: k.created_by,
        last_used_at: k.last_used_at,
        key: secret,
    }
}

pub(super) async fn api_list_keys(State(s): State<S>) -> Json<Vec<ApiKeyJson>> {
    Json(s.users.list_api_keys().await.into_iter().map(|k| key_json(k, None)).collect())
}

#[derive(Deserialize)]
pub struct CreateKeyReq {
    pub name: String,
}

pub(super) async fn api_create_key(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(req): Json<CreateKeyReq>,
) -> ApiResult<(StatusCode, Json<ApiKeyJson>)> {
    let by = scope.subject().unwrap_or("admin").to_string();
    let (key, secret) = s
        .users
        .create_api_key(&req.name, &by)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ApiKeyCreated, "apikey.create")
            .with_actor(actor_for(&scope))
            .with_target(key_target(&key))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .success(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(key_json(key, Some(secret)))))
}

pub(super) async fn api_revoke_key(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(key_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let key = s
        .users
        .revoke_api_key(&key_id)
        .await
        .map_err(|e| err_json(StatusCode::NOT_FOUND, e.to_string()))?;
    audit(
        &s,
        AuditEvent::new(AuditEventType::ApiKeyRevoked, "apikey.revoke")
            .with_actor(actor_for(&scope))
            .with_target(key_target(&key))
            .with_source_ip(client_ip(peer.map(|c| c.0), &headers))
            .success(),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ── Audit trail ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<usize>,
    /// Substring matched against actor, action, target and error.
    pub q: Option<String>,
    /// Only events on this server.
    pub server: Option<String>,
    /// Only failures.
    pub failures: Option<bool>,
}

#[derive(Serialize)]
pub struct AuditResp {
    /// Whether audit logging is on at all.
    pub enabled: bool,
    pub events: Vec<AuditEvent>,
}

/// Recent audit events, newest first.
pub(super) async fn api_audit(State(s): State<S>, Query(q): Query<AuditQuery>) -> Json<AuditResp> {
    let Some(logger) = &s.audit else {
        return Json(AuditResp {
            enabled: false,
            events: Vec::new(),
        });
    };
    let limit = q.limit.unwrap_or(200).clamp(1, crate::audit::RECENT_EVENTS);
    let needle = q.q.as_deref().map(|s| s.to_lowercase()).filter(|s| !s.is_empty());
    let events = logger.recent(limit, |e| {
        if let Some(server) = &q.server {
            if e.target.as_ref().map(|t| &t.id) != Some(server) {
                return false;
            }
        }
        if q.failures == Some(true) && e.outcome == crate::audit::AuditOutcome::Success {
            return false;
        }
        if let Some(n) = &needle {
            let hay = format!(
                "{} {} {} {}",
                e.actor.id,
                e.action,
                e.target.as_ref().map(|t| t.id.as_str()).unwrap_or(""),
                e.error_message.as_deref().unwrap_or("")
            )
            .to_lowercase();
            if !hay.contains(n.as_str()) {
                return false;
            }
        }
        true
    });
    Json(AuditResp {
        enabled: true,
        events,
    })
}
