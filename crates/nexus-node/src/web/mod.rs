//! Embedded web panel for Nexus Node
//!
//! Serves the admin panel UI and REST API on a configurable HTTP port.
//! All static assets are compiled into the binary via include_str!().

pub mod auth;
pub mod firewall;
pub mod observe;
pub mod provision;
#[cfg(test)]
mod router_tests;
pub mod storage;

use crate::backup::BackupManager;
use crate::container::ContainerManager;
use crate::error::NodeError;
use crate::files::FileManager;
use crate::health::HealthChecker;
use crate::metrics::Metrics;
use crate::schedule::{ScheduleManager, ScheduleTask, ScheduleTaskType};
use axum::{
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post, put},
    Extension, Json, Router,
};
use nexus_marketplace::{MarketplaceManager, SearchQuery, SortOrder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Static assets (compiled into binary)
// ---------------------------------------------------------------------------

const INDEX_HTML: &str = include_str!("../../static/index.html");
const STYLE_CSS: &str = include_str!("../../static/css/style.css");
const APP_JS: &str = include_str!("../../static/js/app.js");

/// The blueprints shipped in `/blueprints`, compiled in so the panel serves the
/// same YAML the repo ships and `nexus-config`'s blueprint test validates.
///
/// The UI used to carry its own hand-copied YAML for each game. Those copies
/// drifted out of the schema and every one of them failed to parse when
/// deployed, so the duplicate is gone: this table is the only source the
/// Blueprints page reads from. Keys are the file stems, which are also the ids
/// the UI's blueprint cards use.
const SHIPPED_BLUEPRINTS: &[(&str, &str)] = &[
    ("cs2", include_str!("../../../../blueprints/cs2.yaml")),
    ("dayz", include_str!("../../../../blueprints/dayz.yaml")),
    (
        "minecraft-paper",
        include_str!("../../../../blueprints/minecraft-paper.yaml"),
    ),
    (
        "palworld",
        include_str!("../../../../blueprints/palworld.yaml"),
    ),
    ("rust", include_str!("../../../../blueprints/rust.yaml")),
    (
        "rust-carbon",
        include_str!("../../../../blueprints/rust-carbon.yaml"),
    ),
    (
        "valheim",
        include_str!("../../../../blueprints/valheim.yaml"),
    ),
];

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub manager: Arc<ContainerManager>,
    pub backup_manager: Arc<BackupManager>,
    pub schedule_manager: Arc<ScheduleManager>,
    pub health_checker: Arc<RwLock<HealthChecker>>,
    pub metrics: Arc<Metrics>,
    pub marketplace: Arc<MarketplaceManager>,
    pub node_id: String,
    pub data_dir: String,
    pub start_time: SystemTime,
    pub auth: Arc<auth::WebAuthConfig>,
    pub sessions: Arc<auth::SessionStore>,
    pub update_jobs: crate::update::SharedUpdateJobStore,
    pub mod_jobs: crate::mods::SharedModInstallJobStore,
    pub install_jobs: crate::install::SharedInstallJobStore,
    /// Applies updates to the node itself.
    pub updater: crate::selfupdate::SelfUpdater,
    /// Shared outbound HTTP client, so an update check does not build a new
    /// TLS stack per request.
    pub http: reqwest::Client,
    /// Servers created for a billing system, keyed by its service ids.
    pub provision: Arc<crate::provision::ProvisionStore>,
    pub provision_settings: crate::provision::ProvisionSettings,
    /// One-time sign-in tokens minted for customers.
    pub sso: Arc<auth::SsoTokenStore>,
    /// Brute-force protection for the login endpoint.
    pub login_throttle: Arc<auth::LoginThrottle>,
    /// Where security-relevant actions are recorded, when audit logging is
    /// enabled (`AUDIT_ENABLED`).
    pub audit: Option<Arc<crate::audit::AuditLogger>>,
    /// The node's firewall (may be disabled, in which case it answers
    /// honestly and does nothing).
    pub firewall: Arc<crate::firewall::Firewall>,
    /// Resource usage per server and for the node, sampled on a schedule.
    pub monitor: Arc<crate::stats::ResourceMonitor>,
}

// ---------------------------------------------------------------------------
// Audit helpers
// ---------------------------------------------------------------------------

/// Record an audit event, if audit logging is on. Cheap when it is not.
async fn audit(s: &AppState, event: crate::audit::AuditEvent) {
    if let Some(logger) = &s.audit {
        logger.log(event.with_node_id(s.node_id.clone())).await;
    }
}

/// The audit actor for a session scope.
fn actor_for(scope: &auth::SessionScope) -> crate::audit::AuditActor {
    use crate::audit::{ActorType, AuditActor};
    match scope {
        auth::SessionScope::Admin => AuditActor {
            actor_type: ActorType::User,
            id: "admin".to_string(),
            name: None,
            auth_method: Some("session".to_string()),
            roles: vec!["admin".to_string()],
            tenant_id: None,
        },
        auth::SessionScope::Servers(ids) => AuditActor {
            actor_type: ActorType::User,
            id: format!("customer:{}", ids.join(",")),
            name: None,
            auth_method: Some("sso".to_string()),
            roles: vec!["customer".to_string()],
            tenant_id: None,
        },
    }
}

fn container_target(id: &str) -> crate::audit::AuditTarget {
    crate::audit::AuditTarget {
        target_type: crate::audit::TargetType::Container,
        id: id.to_string(),
        name: None,
        attributes: std::collections::HashMap::new(),
    }
}

/// An audit event for an action on a container by the calling session.
fn container_event(
    event_type: crate::audit::AuditEventType,
    action: &str,
    scope: &auth::SessionScope,
    ip: &str,
    container_id: &str,
) -> crate::audit::AuditEvent {
    crate::audit::AuditEvent::new(event_type, action)
        .with_actor(actor_for(scope))
        .with_target(container_target(container_id))
        .with_source_ip(ip.to_string())
}

/// The address a request came from: the peer, or the first hop of
/// `X-Forwarded-For` when the peer is the local reverse proxy. A forwarded
/// header from anywhere else is not trusted, since anyone can send one.
fn client_ip(peer: Option<std::net::SocketAddr>, headers: &HeaderMap) -> String {
    let peer_ip = peer.map(|p| p.ip());
    let from_proxy = peer_ip
        .map(|ip| match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
            std::net::IpAddr::V6(v6) => v6.is_loopback(),
        })
        .unwrap_or(false);
    if from_proxy {
        for name in ["x-forwarded-for", "x-real-ip"] {
            if let Some(forwarded) = headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                return forwarded.to_string();
            }
        }
    }
    peer_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unknown".to_string())
}

type S = Arc<AppState>;

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

pub async fn start_web_server(
    state: AppState,
    bind_addr: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let shared = Arc::new(state);

    if shared.auth.enabled && !shared.auth.has_credentials() {
        error!(
            "AUTH_ENABLED=true but no AUTH_PASSWORD or AUTH_API_KEYS configured — \
             the panel will reject every API request until a credential is set"
        );
    }
    if !shared.auth.enabled {
        warn!(
            "Web panel authentication is DISABLED (AUTH_ENABLED is not true). \
             Do not expose this port to untrusted networks."
        );
    }

    let app = build_router(shared);

    let addr: std::net::SocketAddr = bind_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!("Web panel listening on http://{}", addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// The panel's routes. Separate from [`start_web_server`] so tests can drive
/// the router without binding a port.
pub fn build_router(shared: S) -> Router {
    // Protected API routes — everything that can read or mutate node state.
    // The auth middleware runs for every route registered before `route_layer`.
    let protected = Router::new()
        // ── Node API ─────────────────────────────────────────────────
        .route("/api/v1/auth/me", get(api_auth_me))
        .route("/api/v1/node/info", get(api_node_info))
        .route("/api/v1/node/health", get(api_node_health))
        .route("/api/v1/node/metrics", get(api_node_metrics))
        .route("/api/v1/node/stats", get(observe::api_node_stats))
        .route("/api/v1/node/update-check", get(api_update_check))
        .route(
            "/api/v1/node/update",
            get(api_self_update_status).post(api_apply_update),
        )
        // ── Container CRUD ───────────────────────────────────────────
        .route(
            "/api/v1/containers",
            get(api_list_containers).post(api_create_container),
        )
        .route(
            "/api/v1/containers/:id",
            get(api_get_container).delete(api_delete_container),
        )
        .route("/api/v1/containers/:id/start", post(api_start_container))
        .route("/api/v1/containers/:id/stop", post(api_stop_container))
        .route(
            "/api/v1/containers/:id/restart",
            post(api_restart_container),
        )
        .route("/api/v1/containers/:id/kill", post(api_kill_container))
        .route(
            "/api/v1/containers/:id/suspend",
            post(api_suspend_container),
        )
        .route(
            "/api/v1/containers/:id/unsuspend",
            post(api_unsuspend_container),
        )
        .route("/api/v1/containers/:id/command", post(api_send_command))
        .route(
            "/api/v1/containers/:id/stats",
            get(observe::api_container_stats),
        )
        .route(
            "/api/v1/containers/:id/console",
            get(observe::api_console_tail),
        )
        .route(
            "/api/v1/containers/:id/console/stream",
            get(observe::api_console_stream),
        )
        .route("/api/v1/containers/:id/exec", post(api_exec))
        // ── Files ────────────────────────────────────────────────────
        .route("/api/v1/containers/:id/files", get(api_list_files))
        .route("/api/v1/containers/:id/files/read", get(api_read_file))
        .route("/api/v1/containers/:id/files/write", post(api_write_file))
        .route(
            "/api/v1/containers/:id/files/delete",
            post(api_delete_files),
        )
        .route("/api/v1/containers/:id/files/rename", post(api_rename_file))
        .route("/api/v1/containers/:id/files/mkdir", post(api_create_dir))
        .route(
            "/api/v1/containers/:id/files/upload",
            post(storage::api_upload_file).route_layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route(
            "/api/v1/containers/:id/files/download",
            get(storage::api_download_file),
        )
        .route(
            "/api/v1/containers/:id/files/compress",
            post(storage::api_compress),
        )
        .route(
            "/api/v1/containers/:id/files/decompress",
            post(storage::api_decompress),
        )
        // ── Backups ──────────────────────────────────────────────────
        .route(
            "/api/v1/containers/:id/backups",
            get(api_list_backups).post(api_create_backup),
        )
        .route(
            "/api/v1/containers/:id/backups/:backup_id/restore",
            post(api_restore_backup),
        )
        .route(
            "/api/v1/containers/:id/backups/:backup_id/download",
            get(storage::api_download_backup),
        )
        .route(
            "/api/v1/containers/:id/backups/:backup_id",
            delete(api_delete_backup),
        )
        // ── Schedules ────────────────────────────────────────────────
        .route(
            "/api/v1/containers/:id/schedules",
            get(api_list_schedules).post(api_create_schedule),
        )
        .route(
            "/api/v1/containers/:id/schedules/:schedule_id",
            put(api_update_schedule).delete(api_delete_schedule),
        )
        .route(
            "/api/v1/containers/:id/schedules/:schedule_id/trigger",
            post(api_trigger_schedule),
        )
        // ── Mods (install to a server) ───────────────────────────────
        .route(
            "/api/v1/containers/:id/mods/install",
            get(api_install_mod_status).post(api_install_mod),
        )
        // ── Game-file install (runs once, before first start) ────────
        .route(
            "/api/v1/containers/:id/install",
            get(api_install_status).post(api_start_install),
        )
        // ── Game-file update (SteamCMD / DepotDownloader) ────────────
        .route(
            "/api/v1/containers/:id/update",
            get(api_update_status).post(api_start_update),
        )
        .route(
            "/api/v1/containers/:id/update-config",
            get(api_update_config),
        )
        // ── Blueprints ───────────────────────────────────────────────
        .route("/api/v1/blueprints", get(api_list_blueprints))
        .route("/api/v1/blueprints/import-egg", post(api_import_egg))
        .route("/api/v1/blueprints/:id", get(api_get_blueprint))
        // ── Marketplace ─────────────────────────────────────────────
        .route("/api/v1/marketplace/search", get(api_marketplace_search))
        .route(
            "/api/v1/marketplace/mods/:provider/:mod_id",
            get(api_marketplace_get_mod),
        )
        // ── Provisioning (billing systems; admin only) ───────────────
        .route(
            "/api/v1/provision/servers",
            get(provision::api_provision_list).post(provision::api_provision_create),
        )
        .route(
            "/api/v1/provision/servers/:id",
            get(provision::api_provision_get).delete(provision::api_provision_delete),
        )
        .route(
            "/api/v1/provision/servers/:id/package",
            post(provision::api_provision_change_package),
        )
        .route(
            "/api/v1/provision/servers/:id/usage",
            get(provision::api_provision_usage),
        )
        .route("/api/v1/provision/sso", post(provision::api_provision_sso))
        // ── Firewall ─────────────────────────────────────────────────
        .route("/api/v1/firewall", get(firewall::api_firewall_status))
        .route(
            "/api/v1/firewall/blocks",
            post(firewall::api_firewall_block),
        )
        .route(
            "/api/v1/firewall/unblock",
            post(firewall::api_firewall_unblock),
        )
        .route(
            "/api/v1/firewall/trusted",
            post(firewall::api_firewall_trust),
        )
        .route(
            "/api/v1/firewall/untrust",
            post(firewall::api_firewall_untrust),
        )
        .route(
            "/api/v1/containers/:id/firewall",
            get(firewall::api_server_firewall).put(firewall::api_set_server_firewall),
        )
        .route(
            "/api/v1/containers/:id/firewall/blocks",
            post(firewall::api_server_firewall_block),
        )
        .route_layer(middleware::from_fn_with_state(shared.clone(), require_auth));

    // Public routes — static assets and the auth endpoints needed to log in.
    Router::new()
        .route("/", get(serve_index))
        .route("/css/style.css", get(serve_css))
        .route("/js/app.js", get(serve_js))
        .route("/api/v1/auth/config", get(api_auth_config))
        .route("/api/v1/auth/login", post(api_login))
        .route("/api/v1/auth/logout", post(api_logout))
        .route("/sso/:token", get(sso_redeem))
        .merge(protected)
        .with_state(shared)
}

// ---------------------------------------------------------------------------
// Authentication middleware + handlers
// ---------------------------------------------------------------------------

/// Middleware that gates protected routes behind a valid session token or key.
///
/// On success the caller's [`auth::SessionScope`] is attached to the request
/// so handlers that answer differently per scope (the container list, `me`)
/// can read it.
async fn require_auth(State(s): State<S>, mut req: Request, next: Next) -> Response {
    // Auth disabled: allow through (intended only for loopback/dev use).
    if !s.auth.enabled {
        req.extensions_mut().insert(auth::SessionScope::Admin);
        return next.run(req).await;
    }

    // Fail closed if enabled without any configured credential.
    if !s.auth.has_credentials() {
        return err_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication is enabled but no credential is configured on the node",
        )
        .into_response();
    }

    let headers = req.headers().clone();
    let Some(scope) = resolve_scope(&s, &headers).await else {
        return err_json(StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    };

    if !scope_allows(&scope, req.method(), req.uri().path()) {
        return err_json(
            StatusCode::FORBIDDEN,
            "This session is limited to its own servers",
        )
        .into_response();
    }

    req.extensions_mut().insert(scope);
    next.run(req).await
}

/// Work out who is calling from a bearer session token, an API key (as a
/// bearer or `x-api-key`), or the session cookie. A stale bearer token does
/// not veto a valid cookie: a customer arriving through SSO may still have an
/// old admin token in the browser's storage.
async fn resolve_scope(s: &S, headers: &header::HeaderMap) -> Option<auth::SessionScope> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::bearer_from_header);

    if let Some(token) = bearer {
        if let Some(scope) = s.sessions.scope_of(token).await {
            return Some(scope);
        }
        if s.auth.verify_api_key(token) {
            return Some(auth::SessionScope::Admin);
        }
    }

    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::session_from_cookie)
    {
        if let Some(scope) = s.sessions.scope_of(token).await {
            return Some(scope);
        }
    }

    if let Some(key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        if s.auth.verify_api_key(key) {
            return Some(auth::SessionScope::Admin);
        }
    }

    None
}

/// What a session may reach. Admin sessions: everything. Server-scoped
/// sessions: their own servers, the auth endpoints, the read-only mod
/// catalogue, and nothing at node level.
///
/// Deleting a server is refused even for one the session owns: the server
/// exists because a billing system created it, and only that system's
/// termination should remove it.
fn scope_allows(scope: &auth::SessionScope, method: &Method, path: &str) -> bool {
    let ids = match scope {
        auth::SessionScope::Admin => return true,
        auth::SessionScope::Servers(ids) => ids,
    };

    if path.starts_with("/api/v1/auth/") {
        return true;
    }
    if path == "/api/v1/containers" {
        // The list itself is filtered to the session's servers.
        return method == Method::GET;
    }
    if let Some(rest) = path.strip_prefix("/api/v1/containers/") {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if id.is_empty() || !ids.iter().any(|allowed| allowed == id) {
            return false;
        }
        return !(tail.is_empty() && method == Method::DELETE);
    }
    if path.starts_with("/api/v1/marketplace/") {
        return method == Method::GET;
    }
    false
}

#[derive(Serialize)]
struct AuthConfigResponse {
    auth_required: bool,
}

/// Public endpoint so the UI knows whether to show the login screen.
async fn api_auth_config(State(s): State<S>) -> impl IntoResponse {
    Json(AuthConfigResponse {
        auth_required: s.auth.enabled,
    })
}

#[derive(Deserialize)]
struct LoginReq {
    password: Option<String>,
    api_key: Option<String>,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
    expires_in_secs: u64,
}

/// Exchange a password or API key for a session token.
///
/// Throttled per source address and node-wide: past the limit the caller
/// gets `429` with `Retry-After`, and every failure is audited.
async fn api_login(
    State(s): State<S>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<LoginReq>,
) -> Result<Response, Response> {
    // If auth is disabled there is nothing to log in to; report success with
    // an empty token so the UI can proceed without a login screen.
    if !s.auth.enabled {
        return Ok(Json(LoginResponse {
            token: String::new(),
            expires_in_secs: 0,
        })
        .into_response());
    }

    let ip = client_ip(peer.map(|c| c.0), &headers);

    if let auth::LoginVerdict::Blocked { retry_after_secs } = s.login_throttle.check(&ip).await {
        warn!("Login from {} refused: too many failed attempts", ip);
        audit(
            &s,
            crate::audit::AuditEvent::new(crate::audit::AuditEventType::RateLimitExceeded, "login")
                .with_source_ip(ip.clone())
                .failure("too many failed login attempts"),
        )
        .await;
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry_after_secs.to_string())],
            Json(ApiError {
                error: format!(
                    "Too many failed login attempts; try again in {} seconds",
                    retry_after_secs
                ),
            }),
        )
            .into_response());
    }

    let method = if body.api_key.is_some() {
        "api_key"
    } else {
        "password"
    };
    let ok = body.password.as_deref().map(|p| s.auth.verify_password(p)).unwrap_or(false)
        || body.api_key.as_deref().map(|k| s.auth.verify_api_key(k)).unwrap_or(false);

    if !ok {
        s.login_throttle.record_failure(&ip).await;
        audit(
            &s,
            crate::audit::AuditEvent::authentication_failure("admin", "invalid credentials")
                .with_context("method", method)
                .with_source_ip(ip.clone()),
        )
        .await;
        return Err(err_json(StatusCode::UNAUTHORIZED, "Invalid credentials").into_response());
    }

    s.login_throttle.record_success(&ip).await;
    audit(
        &s,
        crate::audit::AuditEvent::authentication_success("admin", method).with_source_ip(ip),
    )
    .await;

    let token = s.sessions.create().await;
    Ok(Json(LoginResponse {
        token,
        expires_in_secs: s.sessions.ttl_secs(),
    })
    .into_response())
}

/// Revoke the caller's session, whether it arrived as a bearer token or as
/// the cookie an SSO sign-in set. The cookie is cleared either way.
async fn api_logout(State(s): State<S>, req: Request) -> impl IntoResponse {
    if let Some(token) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::bearer_from_header)
    {
        s.sessions.revoke(token).await;
    }
    if let Some(token) = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::session_from_cookie)
    {
        s.sessions.revoke(token).await;
    }
    (
        [(
            header::SET_COOKIE,
            "nexus_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        )],
        Json(serde_json::json!({ "ok": true })),
    )
}

#[derive(Serialize)]
struct MeResponse {
    /// `admin` or `servers`.
    scope: &'static str,
    /// The servers a scoped session may act on; empty for admin.
    server_ids: Vec<String>,
}

/// Who the caller is, so the UI can show a customer only their server.
async fn api_auth_me(Extension(scope): Extension<auth::SessionScope>) -> impl IntoResponse {
    let (scope_name, server_ids) = match scope {
        auth::SessionScope::Admin => ("admin", Vec::new()),
        auth::SessionScope::Servers(ids) => ("servers", ids),
    };
    Json(MeResponse {
        scope: scope_name,
        server_ids,
    })
}

/// Turn a one-time SSO token into a browser session scoped to its server,
/// and land the customer on that server's page.
///
/// The session is delivered as an `HttpOnly` cookie rather than a bearer
/// token in the URL: the URL is the one thing here that gets written to
/// proxy logs and browser history. `Secure` follows the scheme the reverse
/// proxy reports, since the node itself speaks plain HTTP behind it.
async fn sso_redeem(State(s): State<S>, Path(token): Path<String>, req: Request) -> Response {
    // Redeem before the auth check so an expired or reused token is gone
    // regardless; a token must never survive a failed attempt.
    let grant = s.sso.redeem(&token).await;

    if !s.auth.enabled {
        // Nothing to sign in to; the panel is open. Still honour the link.
        let target = grant
            .map(|g| format!("/#/servers/{}", g.server_id))
            .unwrap_or_else(|| "/".to_string());
        return Redirect::to(&target).into_response();
    }

    let Some(grant) = grant else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            SSO_EXPIRED_HTML,
        )
            .into_response();
    };

    let scope = auth::SessionScope::Servers(vec![grant.server_id.clone()]);
    let session = s.sessions.create_scoped(scope, grant.session_ttl).await;
    let max_age = grant
        .session_ttl
        .map(|t| t.as_secs().min(s.sessions.ttl_secs()))
        .unwrap_or_else(|| s.sessions.ttl_secs());

    let https = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    let cookie = format!(
        "nexus_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        session,
        max_age,
        if https { "; Secure" } else { "" }
    );

    info!(
        "SSO sign-in for server {} (subject {:?})",
        grant.server_id, grant.subject
    );
    let ip = client_ip(
        req.extensions().get::<ConnectInfo<std::net::SocketAddr>>().map(|c| c.0),
        req.headers(),
    );
    audit(
        &s,
        crate::audit::AuditEvent::new(crate::audit::AuditEventType::SessionStart, "sso")
            .with_actor(actor_for(&auth::SessionScope::Servers(vec![grant
                .server_id
                .clone()])))
            .with_target(container_target(&grant.server_id))
            .with_context("subject", &grant.subject)
            .with_source_ip(ip)
            .success(),
    )
    .await;

    (
        [(header::SET_COOKIE, cookie)],
        Redirect::to(&format!("/#/servers/{}", grant.server_id)),
    )
        .into_response()
}

const SSO_EXPIRED_HTML: &str = r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>Sign-in link expired</title>
<style>body{font-family:system-ui,sans-serif;background:#0f1117;color:#e6e6e6;display:flex;align-items:center;justify-content:center;height:100vh;margin:0}
main{max-width:28rem;padding:2rem;text-align:center}h1{font-size:1.25rem}p{color:#9aa0a6}</style></head>
<body><main><h1>This sign-in link has expired</h1>
<p>Sign-in links work once and only for a minute. Go back to your billing portal and open the panel again.</p></main></body></html>"#;

// ---------------------------------------------------------------------------
// Static asset handlers
// ---------------------------------------------------------------------------

async fn serve_index() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

async fn serve_css() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
}

async fn serve_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn system_time_to_epoch(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn err_json(status: StatusCode, msg: impl ToString) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            error: msg.to_string(),
        }),
    )
}

fn node_err_status(e: &NodeError) -> StatusCode {
    match e {
        NodeError::ContainerNotFound(_) => StatusCode::NOT_FOUND,
        NodeError::ContainerAlreadyExists(_) => StatusCode::CONFLICT,
        NodeError::InvalidInput(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ApiError {
    error: String,
}

#[derive(Serialize)]
struct NodeInfo {
    node_id: String,
    version: String,
    uptime_secs: u64,
    containers_total: usize,
    containers_running: usize,
    system: SystemInfo,
}

#[derive(Serialize)]
struct SystemInfo {
    cpu_count: usize,
    total_memory_bytes: u64,
    used_memory_bytes: u64,
    total_disk_bytes: u64,
    used_disk_bytes: u64,
}

#[derive(Serialize)]
struct ContainerJson {
    id: String,
    name: String,
    image: String,
    status: String,
    pid: Option<u32>,
    exit_code: Option<i32>,
    restart_count: u32,
    /// Times the process died on its own.
    crash_count: u32,
    /// Bytes the server's files occupy, as of the last measurement.
    disk_used_bytes: u64,
    /// The disk allowance in bytes; zero means unlimited.
    disk_limit_bytes: u64,
    created_at: u64,
    started_at: Option<u64>,
    stopped_at: Option<u64>,
    /// The latest resource sample, while the server runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<crate::stats::ResourceSample>,
}

#[derive(Serialize)]
struct HealthJson {
    status: String,
    checks: Vec<HealthCheckJson>,
}

#[derive(Serialize)]
struct HealthCheckJson {
    name: String,
    status: String,
    message: Option<String>,
}

#[derive(Deserialize)]
struct CreateContainerReq {
    config_yaml: String,
    server_id: Option<String>,
    auto_start: Option<bool>,
}

#[derive(Deserialize)]
struct StopReq {
    timeout: Option<u32>,
}

#[derive(Deserialize)]
struct CommandReq {
    command: String,
}

#[derive(Deserialize)]
struct FileQuery {
    path: Option<String>,
}

#[derive(Serialize)]
struct FileInfoJson {
    name: String,
    path: String,
    is_directory: bool,
    size: u64,
    modified_at: i64,
    mime_type: String,
    is_symlink: bool,
    symlink_target: Option<String>,
}

#[derive(Deserialize)]
struct WriteFileReq {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct DeleteFilesReq {
    paths: Vec<String>,
    recursive: Option<bool>,
}

#[derive(Deserialize)]
struct RenameFileReq {
    old_path: String,
    new_path: String,
}

#[derive(Deserialize)]
struct MkdirReq {
    path: String,
    recursive: Option<bool>,
}

#[derive(Serialize)]
struct BackupJson {
    id: String,
    container_id: String,
    name: String,
    size: u64,
    created_at: i64,
    checksum: String,
    status: String,
    error: Option<String>,
    /// The paths the backup was limited to; empty means everything.
    include: Vec<String>,
}

#[derive(Deserialize)]
struct CreateBackupReq {
    name: Option<String>,
    /// Paths to back up instead of the blueprint's.
    paths: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
struct RestoreBackupReq {
    /// Replace the server directory instead of unpacking over it.
    delete_existing: Option<bool>,
    /// Stop the server first if it is running; otherwise a running server
    /// is refused.
    stop: Option<bool>,
}

#[derive(Serialize)]
struct ScheduleJson {
    id: String,
    container_id: String,
    name: String,
    cron_expression: String,
    /// IANA zone the expression is read in; `null` is UTC.
    timezone: Option<String>,
    is_active: bool,
    tasks: Vec<ScheduleTaskJson>,
    created_at: i64,
    last_run: Option<i64>,
    next_run: Option<i64>,
    /// What went wrong on the last run, if anything did.
    last_error: Option<String>,
    /// Whether it is executing right now.
    running: bool,
}

#[derive(Serialize, Deserialize)]
struct ScheduleTaskJson {
    action: String,
    payload: String,
    time_offset: u32,
}

#[derive(Deserialize)]
struct CreateScheduleReq {
    name: String,
    cron_expression: String,
    timezone: Option<String>,
    is_active: Option<bool>,
    tasks: Vec<ScheduleTaskJson>,
}

#[derive(Deserialize)]
struct UpdateScheduleReq {
    name: Option<String>,
    cron_expression: Option<String>,
    timezone: Option<String>,
    is_active: Option<bool>,
    tasks: Option<Vec<ScheduleTaskJson>>,
}

// ---------------------------------------------------------------------------
// Node API handlers
// ---------------------------------------------------------------------------

async fn api_node_info(State(s): State<S>) -> impl IntoResponse {
    let containers = s.manager.list_containers().await;
    let total = containers.len();
    let running = containers.iter().filter(|c| c.status.is_running()).count();

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();

    let disks = sysinfo::Disks::new_with_refreshed_list();
    let (total_disk, used_disk) = disks.list().iter().fold((0u64, 0u64), |(t, u), d| {
        (
            t + d.total_space(),
            u + (d.total_space() - d.available_space()),
        )
    });

    Json(NodeInfo {
        node_id: s.node_id.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: system_time_to_epoch(SystemTime::now()) - system_time_to_epoch(s.start_time),
        containers_total: total,
        containers_running: running,
        system: SystemInfo {
            cpu_count: sys.cpus().len(),
            total_memory_bytes: sys.total_memory(),
            used_memory_bytes: sys.used_memory(),
            total_disk_bytes: total_disk,
            used_disk_bytes: used_disk,
        },
    })
}

async fn api_node_health(State(s): State<S>) -> impl IntoResponse {
    let mut checker = s.health_checker.write().await;
    let result = checker.check().await;

    let status_str = match result.status {
        crate::health::HealthStatus::Healthy => "healthy",
        crate::health::HealthStatus::Degraded => "degraded",
        crate::health::HealthStatus::Unhealthy => "unhealthy",
    };

    let checks: Vec<HealthCheckJson> = result
        .checks
        .iter()
        .map(|(name, c)| HealthCheckJson {
            name: name.clone(),
            status: match c.status {
                crate::health::HealthStatus::Healthy => "pass",
                crate::health::HealthStatus::Degraded => "warn",
                crate::health::HealthStatus::Unhealthy => "fail",
            }
            .to_string(),
            message: c.error.clone(),
        })
        .collect();

    Json(HealthJson {
        status: status_str.to_string(),
        checks,
    })
}

async fn api_node_metrics(State(s): State<S>) -> impl IntoResponse {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let families = s.metrics.registry().gather();
    let mut buf = Vec::new();
    if encoder.encode(&families, &mut buf).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            "encode error".to_string(),
        );
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        String::from_utf8(buf).unwrap_or_default(),
    )
}

// ---------------------------------------------------------------------------
// Container API handlers
// ---------------------------------------------------------------------------

async fn api_list_containers(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
) -> impl IntoResponse {
    let containers = s.manager.list_containers().await;
    let usage = s.monitor.all_usage().await;
    let out: Vec<ContainerJson> = containers
        .iter()
        .filter(|c| scope.allows_server(&c.id))
        .map(|c| {
            let mut json = container_to_json(c);
            json.usage = usage.get(&c.id).cloned();
            json
        })
        .collect();
    Json(out)
}

async fn api_get_container(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<ContainerJson>, (StatusCode, Json<ApiError>)> {
    let c = s
        .manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let mut json = container_to_json(&c);
    json.usage = s.monitor.usage(&id).await;
    Ok(Json(json))
}

async fn api_create_container(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<CreateContainerReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let config: nexus_config::GameConfig = serde_yaml::from_str(&body.config_yaml)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, format!("Invalid YAML: {}", e)))?;

    let id = s
        .manager
        .create_container(&config, body.server_id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    let ip = client_ip(peer.map(|c| c.0), &headers);
    audit(
        &s,
        container_event(
            crate::audit::AuditEventType::ContainerCreated,
            "create",
            &scope,
            &ip,
            &id,
        )
        .with_context("name", &config.metadata.name)
        .with_context("image", &config.container.image)
        .success(),
    )
    .await;

    // Choosing a game *is* choosing to install it: a server is useless until
    // its game files are there, so the install starts as soon as it is
    // created rather than waiting for the operator to find a button. The
    // client polls `GET .../install` for progress.
    let auto_start = body.auto_start.unwrap_or(false);
    let installing = spawn_install(&s, &id, auto_start).await.is_some();

    if auto_start && !installing {
        let _ = s.manager.start_container(&id).await;
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "installing": installing })),
    ))
}

/// Start a server's install in the background, if it has one to run.
///
/// With `auto_start`, a successful install is followed by starting the server
/// — which is what "auto-start after creation" has to mean for a game whose
/// files did not exist yet at creation time.
///
/// Returns the job it registered, or `None` when this blueprint needs no
/// install or one is already running.
async fn spawn_install(s: &S, id: &str, auto_start: bool) -> Option<crate::install::InstallJob> {
    let plan = s.manager.install_plan(id).await?;

    let job = s.install_jobs.start(id, &plan.image).await.ok()?;

    let manager = s.manager.clone();
    let jobs = s.install_jobs.clone();
    let container_id = id.to_string();
    tokio::spawn(async move {
        // Output is appended as it arrives so a half-hour download shows
        // progress rather than a spinner.
        let sink_jobs = jobs.clone();
        let sink_id = container_id.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let pump = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                sink_jobs.append_log(&sink_id, &line).await;
            }
        });

        let result = manager
            .run_install(&container_id, |line| {
                let _ = tx.send(line);
            })
            .await;

        drop(tx);
        let _ = pump.await;

        match result {
            Ok(code) => {
                jobs.finish(&container_id, Some(code)).await;
                if code == 0 && auto_start {
                    if let Err(e) = manager.start_container(&container_id).await {
                        warn!(
                            "Auto-start after install failed for {}: {}",
                            container_id, e
                        );
                    }
                }
            }
            Err(e) => jobs.fail(&container_id, e.to_string()).await,
        }
    });

    Some(job)
}

/// Kick off (or re-run) a server's game-file install.
///
/// Re-running is the "reinstall" path: the script is the blueprint's own, and
/// SteamCMD-style installs are incremental, so this is also how an operator
/// repairs a server whose files were damaged.
async fn api_start_install(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<crate::install::InstallJob>, (StatusCode, Json<ApiError>)> {
    let state = s
        .manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    // Installing under a running server would rewrite the files it is reading.
    if state.status.is_running() {
        return Err(err_json(
            StatusCode::CONFLICT,
            "stop the server before installing its game files",
        ));
    }

    if s.manager.install_plan(&id).await.is_none() {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "this server's blueprint declares no install step — its image is self-contained",
        ));
    }

    // An explicit install is a repair, not a deployment: leave the server
    // stopped so the operator can look at the result first.
    spawn_install(&s, &id, false).await.map(Json).ok_or_else(|| {
        err_json(
            StatusCode::CONFLICT,
            "an install is already running for this server",
        )
    })
}

/// Return the current/most-recent install job for a server.
async fn api_install_status(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<crate::install::InstallJob>, (StatusCode, Json<ApiError>)> {
    match s.install_jobs.get(&id).await {
        Some(job) => Ok(Json(job)),
        None => Err(err_json(
            StatusCode::NOT_FOUND,
            "no install has been run for this server on this node",
        )),
    }
}

async fn api_start_container(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = s.manager.start_container(&id).await;
    let event = container_event(
        crate::audit::AuditEventType::ContainerStarted,
        "start",
        &scope,
        &ip,
        &id,
    );
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_stop_container(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<StopReq>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let timeout = body.and_then(|b| b.timeout);
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = s.manager.stop_container(&id, timeout).await;
    let event = container_event(
        crate::audit::AuditEventType::ContainerStopped,
        "stop",
        &scope,
        &ip,
        &id,
    );
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_restart_container(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = s.manager.restart_container(&id).await;
    let event = container_event(
        crate::audit::AuditEventType::ContainerRestarted,
        "restart",
        &scope,
        &ip,
        &id,
    );
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_kill_container(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.manager
        .stop_container(&id, Some(0))
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_delete_container(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = s.manager.delete_container(&id, true).await;
    let event = container_event(
        crate::audit::AuditEventType::ContainerDeleted,
        "delete",
        &scope,
        &ip,
        &id,
    );
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    forget_server(&s, &id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_suspend_container(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = s.manager.suspend_container(&id).await;
    let event = container_event(
        crate::audit::AuditEventType::ContainerStopped,
        "suspend",
        &scope,
        &ip,
        &id,
    );
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_unsuspend_container(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = s.manager.unsuspend_container(&id).await;
    let event = container_event(
        crate::audit::AuditEventType::ContainerStarted,
        "unsuspend",
        &scope,
        &ip,
        &id,
    );
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_send_command(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<CommandReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.manager
        .send_command(&id, &body.command)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ExecReq {
    /// The command line to run.
    command: String,
    /// When true, split `command` on whitespace and exec directly instead of
    /// running it through `/bin/sh -c` (no shell features like pipes/globs).
    #[serde(default)]
    raw: bool,
}

#[derive(Serialize)]
struct ExecResp {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}

/// Run a one-shot command as a new process inside a running container (the
/// shell console) and return its captured output. Unlike the game console
/// (`/command`, which writes to the game process's stdin), this can run
/// arbitrary tooling such as `npm` or a shell.
async fn api_exec(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<ExecReq>,
) -> Result<Json<ExecResp>, (StatusCode, Json<ApiError>)> {
    if body.command.trim().is_empty() {
        return Err(err_json(StatusCode::BAD_REQUEST, "Empty command"));
    }

    let argv: Vec<String> = if body.raw {
        body.command.split_whitespace().map(String::from).collect()
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            body.command.clone(),
        ]
    };

    let out = s
        .manager
        .exec_command(&id, &argv, crate::runtime::DEFAULT_EXEC_TIMEOUT)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    Ok(Json(ExecResp {
        stdout: out.stdout,
        stderr: out.stderr,
        exit_code: out.exit_code,
    }))
}

// ---------------------------------------------------------------------------
// File API handlers
// ---------------------------------------------------------------------------

fn file_manager(state: &AppState, container_id: &str) -> FileManager {
    FileManager::new(container_id, std::path::Path::new(&state.data_dir))
        .with_owner(state.manager.game_user())
}

async fn api_list_files(
    State(s): State<S>,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> Result<Json<Vec<FileInfoJson>>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    let path = q.path.as_deref().unwrap_or("/");
    let files = fm
        .list_files(path)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    Ok(Json(
        files
            .into_iter()
            .map(|f| FileInfoJson {
                name: f.name,
                path: f.path,
                is_directory: f.is_directory,
                size: f.size,
                modified_at: f.modified_at,
                mime_type: f.mime_type,
                is_symlink: f.is_symlink,
                symlink_target: f.symlink_target,
            })
            .collect(),
    ))
}

async fn api_read_file(
    State(s): State<S>,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    let path = q.path.as_deref().unwrap_or("");
    let (data, _size, _mime) = fm
        .read_file(path, None, None)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream")],
        data,
    ))
}

async fn api_write_file(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<WriteFileReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    let result = fm.write_file(&body.path, body.content.as_bytes(), true).await;
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let event = container_event(
        crate::audit::AuditEventType::FileWritten,
        "file.write",
        &scope,
        &ip,
        &id,
    )
    .with_context("path", &body.path);
    match &result {
        Ok(_) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    let bytes_written = result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "bytes_written": bytes_written })))
}

async fn api_delete_files(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DeleteFilesReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    let result = fm.delete(&body.paths, body.recursive.unwrap_or(false)).await;
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let event = container_event(
        crate::audit::AuditEventType::FileDeleted,
        "file.delete",
        &scope,
        &ip,
        &id,
    )
    .with_context("paths", &body.paths);
    match &result {
        Ok(_) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    let deleted = result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn api_rename_file(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RenameFileReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    let result = fm.rename(&body.old_path, &body.new_path).await;
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let event = container_event(
        crate::audit::AuditEventType::FileRenamed,
        "file.rename",
        &scope,
        &ip,
        &id,
    )
    .with_context("from", &body.old_path)
    .with_context("to", &body.new_path);
    match &result {
        Ok(_) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_create_dir(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<MkdirReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    fm.create_directory(&body.path, body.recursive.unwrap_or(true))
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Backup API handlers
// ---------------------------------------------------------------------------

async fn api_list_backups(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<Vec<BackupJson>>, (StatusCode, Json<ApiError>)> {
    let backups = s
        .backup_manager
        .list_backups(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(backups.into_iter().map(backup_to_json).collect()))
}

async fn api_create_backup(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<CreateBackupReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let name = body
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| format!("manual-{}", chrono::Utc::now().format("%Y%m%d-%H%M")));
    let info = crate::backup::backup_server(
        &s.manager,
        &s.backup_manager,
        &id,
        &name,
        body.paths,
        crate::schedule::PRE_BACKUP_SETTLE,
    )
    .await
    .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok((StatusCode::CREATED, Json(backup_to_json(info))))
}

/// Restore a backup into a stopped server. Restoring under a running game
/// would have it overwrite the files as they land, so a running server is
/// refused unless the request says to stop it first.
async fn api_restore_backup(
    State(s): State<S>,
    Extension(scope): Extension<auth::SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path((id, backup_id)): Path<(String, String)>,
    body: Option<Json<RestoreBackupReq>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let state = s
        .manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let mut stopped = false;
    if state.status.is_running() {
        if !req.stop.unwrap_or(false) {
            return Err(err_json(
                StatusCode::CONFLICT,
                "the server is running; stop it first, or pass \"stop\": true",
            ));
        }
        s.manager
            .stop_container(&id, None)
            .await
            .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
        stopped = true;
    }
    let delete_existing = req.delete_existing.unwrap_or(false);
    let result = s.backup_manager.restore_backup(&id, &backup_id, delete_existing).await;
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let event = container_event(
        crate::audit::AuditEventType::FileWritten,
        "backup.restore",
        &scope,
        &ip,
        &id,
    )
    .with_context("backup_id", &backup_id)
    .with_context("delete_existing", delete_existing);
    match &result {
        Ok(()) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true, "stopped": stopped })))
}

async fn api_delete_backup(
    State(s): State<S>,
    Path((id, backup_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.backup_manager
        .delete_backup(&id, &backup_id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Schedule API handlers
// ---------------------------------------------------------------------------

async fn api_list_schedules(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ScheduleJson>>, (StatusCode, Json<ApiError>)> {
    let schedules = s
        .schedule_manager
        .list_schedules(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let mut out = Vec::with_capacity(schedules.len());
    for sch in schedules {
        let running = s.schedule_manager.is_running(&sch.id).await;
        out.push(schedule_to_json(sch, running));
    }
    Ok(Json(out))
}

async fn api_create_schedule(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<CreateScheduleReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let tasks: Vec<ScheduleTask> = body.tasks.into_iter().map(task_from_json).collect();
    let info = s
        .schedule_manager
        .create_schedule(
            &id,
            &body.name,
            &body.cron_expression,
            body.timezone.as_deref(),
            body.is_active.unwrap_or(true),
            tasks,
        )
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok((StatusCode::CREATED, Json(schedule_to_json(info, false))))
}

async fn api_update_schedule(
    State(s): State<S>,
    Path((id, schedule_id)): Path<(String, String)>,
    Json(body): Json<UpdateScheduleReq>,
) -> Result<Json<ScheduleJson>, (StatusCode, Json<ApiError>)> {
    let tasks = body.tasks.map(|t| t.into_iter().map(task_from_json).collect());
    let info = s
        .schedule_manager
        .update_schedule(
            &id,
            &schedule_id,
            body.name.as_deref(),
            body.cron_expression.as_deref(),
            body.timezone.as_deref(),
            body.is_active,
            tasks,
        )
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let running = s.schedule_manager.is_running(&schedule_id).await;
    Ok(Json(schedule_to_json(info, running)))
}

async fn api_delete_schedule(
    State(s): State<S>,
    Path((id, schedule_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.schedule_manager
        .delete_schedule(&id, &schedule_id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_trigger_schedule(
    State(s): State<S>,
    Path((id, schedule_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    // Dispatch the schedule's tasks for real (container command / power /
    // backup), the same way the background runner does.
    let callback = crate::schedule::dispatch_callback(s.manager.clone(), s.backup_manager.clone());
    s.schedule_manager
        .trigger_schedule(&id, &schedule_id, &callback)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Mod install handler
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct InstallModReq {
    provider: String,
    mod_id: String,
    /// Specific version to install; defaults to the latest.
    version: Option<String>,
    /// Directory within the server to install into (e.g. "oxide/plugins",
    /// "plugins"). Defaults to a per-framework/provider guess when omitted.
    target_dir: Option<String>,
    /// Rust modding framework the target server runs ("oxide" or "carbon").
    /// Selects `oxide/plugins` vs `carbon/plugins` when `target_dir` is
    /// omitted; ignored when `target_dir` is set explicitly.
    framework: Option<String>,
}

/// Sensible default mods directory for a provider when the caller doesn't
/// specify one. The Rust marketplaces install Oxide plugins.
fn default_mods_dir(provider: &str) -> &'static str {
    match provider {
        "umod" | "codefling" | "lone_design" => "oxide/plugins",
        // Steam Workshop items are whole mod directories that the games
        // themselves expect in the server root (`@Mod` folders for DayZ/Arma,
        // Workshop-id folders elsewhere), not files dropped in a plugins dir.
        "steam_workshop" => ".",
        _ => "plugins",
    }
}

/// Resolve the install directory when the caller didn't set `target_dir`.
/// An explicit `framework` wins (Rust servers can run Oxide *or* Carbon, which
/// use different plugin folders); otherwise fall back to a per-provider guess.
fn mods_dir_for(framework: Option<&str>, provider: &str) -> String {
    // The Oxide/Carbon split is a Rust-plugin concept; a stray framework hint
    // must not divert a Workshop mod into `oxide/plugins`, where the game
    // would never find it.
    if provider == "steam_workshop" {
        return default_mods_dir(provider).to_string();
    }
    match framework.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) if f.eq_ignore_ascii_case("carbon") => "carbon/plugins".to_string(),
        Some(f) if f.eq_ignore_ascii_case("oxide") => "oxide/plugins".to_string(),
        _ => default_mods_dir(provider).to_string(),
    }
}

/// Start installing a marketplace mod into a server's mods directory.
///
/// The install runs as a background job and the client polls
/// `GET .../mods/install` for progress, mirroring the game-file update
/// executor. Inline installs were fine for a 50 KB Oxide plugin, but a Steam
/// Workshop item can be gigabytes — long enough for the request to outlive
/// any proxy between the browser and the node.
async fn api_install_mod(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<InstallModReq>,
) -> Result<Json<crate::mods::ModInstallJob>, (StatusCode, Json<ApiError>)> {
    // Container must exist.
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    let subdir = body
        .target_dir
        .filter(|d| !d.trim().is_empty())
        .unwrap_or_else(|| mods_dir_for(body.framework.as_deref(), &body.provider));

    // Resolve the install directory safely inside the container's files.
    let fm = file_manager(&s, &id);
    let target = fm
        .resolve_path(&subdir)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e.to_string()))?;

    // Register the job (rejects if an install is already running here).
    let job = s
        .mod_jobs
        .start(&id, &body.provider, &body.mod_id, &subdir)
        .await
        .map_err(|e| err_json(StatusCode::CONFLICT, e))?;

    let server_dir = fm.server_dir().to_path_buf();
    tokio::spawn(crate::mods::run_install(
        s.mod_jobs.clone(),
        s.marketplace.clone(),
        id,
        body.provider,
        body.mod_id,
        body.version,
        target,
        server_dir,
    ));

    Ok(Json(job))
}

/// Return the current/most-recent mod-install job for a server.
async fn api_install_mod_status(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<crate::mods::ModInstallJob>, (StatusCode, Json<ApiError>)> {
    match s.mod_jobs.get(&id).await {
        Some(job) => Ok(Json(job)),
        None => Err(err_json(
            StatusCode::NOT_FOUND,
            "no mod install has been run for this server",
        )),
    }
}

// ---------------------------------------------------------------------------
// Game-file update executor
// ---------------------------------------------------------------------------

/// Request to run a game-file update. The body carries an `UpdateApply`
/// strategy (tagged by `type`, e.g. `steam_cmd` / `depot_downloader`) plus an
/// optional install directory.
#[derive(Deserialize, Default)]
struct StartUpdateReq {
    /// Explicit strategy. Omit (or send an empty body) to use the strategy
    /// declared in the server's own blueprint (`updates.apply`).
    #[serde(flatten, default)]
    apply: Option<nexus_config::UpdateApply>,
    /// Directory inside the container to install into. Defaults to the
    /// blueprint's `startup.working_dir`, then `/home/container`.
    install_dir: Option<String>,
}

/// The update strategy a server declares in its blueprint, so the UI can
/// prefill (and one-click) instead of making the operator retype it.
#[derive(Serialize)]
struct UpdateConfigResp {
    apply: nexus_config::UpdateApply,
    install_dir: String,
    /// Whether the blueprint asks for unattended updates.
    auto_update: bool,
}

/// Return the update strategy declared in this server's stored blueprint.
/// 404 when the server has no blueprint on file or declares no `updates` block.
async fn api_update_config(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<UpdateConfigResp>, (StatusCode, Json<ApiError>)> {
    // Container must exist (gives a clean 404 for unknown ids).
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    let config = s.manager.load_blueprint(&id).await.ok_or_else(|| {
        err_json(
            StatusCode::NOT_FOUND,
            "no blueprint stored for this server (it may predate blueprint persistence)",
        )
    })?;

    let updates = config.updates.as_ref().ok_or_else(|| {
        err_json(
            StatusCode::NOT_FOUND,
            "this server's blueprint declares no update strategy",
        )
    })?;

    Ok(Json(UpdateConfigResp {
        apply: updates.apply.clone(),
        install_dir: blueprint_install_dir(&config),
        auto_update: updates.auto_update,
    }))
}

/// The directory a blueprint installs into: its startup working dir, falling
/// back to the conventional container path.
fn blueprint_install_dir(config: &nexus_config::GameConfig) -> String {
    let dir = config.startup.working_dir.trim();
    if dir.is_empty() {
        crate::update::DEFAULT_INSTALL_DIR.to_string()
    } else {
        dir.to_string()
    }
}

/// Kick off a game-file update for a server. The update runs as a background
/// job (downloads can take many minutes); this returns immediately with the
/// job snapshot, and the client polls `GET .../update` for progress.
async fn api_start_update(
    State(s): State<S>,
    Path(id): Path<String>,
    body: Option<Json<StartUpdateReq>>,
) -> Result<Json<crate::update::UpdateJob>, (StatusCode, Json<ApiError>)> {
    // Container must exist and be running to exec into it.
    let state = s
        .manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    if !state.status.is_running() {
        return Err(err_json(
            StatusCode::CONFLICT,
            "server must be running to apply an update",
        ));
    }

    let body = body.map(|Json(b)| b).unwrap_or_default();

    // The blueprint supplies the defaults; an explicit request overrides them.
    let blueprint = s.manager.load_blueprint(&id).await;

    let apply = match body.apply {
        Some(apply) => apply,
        None => blueprint
            .as_ref()
            .and_then(|c| c.updates.as_ref())
            .map(|u| u.apply.clone())
            .ok_or_else(|| {
                err_json(
                    StatusCode::BAD_REQUEST,
                    "no update strategy given and this server's blueprint declares none",
                )
            })?,
    };

    let install_dir = body
        .install_dir
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .or_else(|| blueprint.as_ref().map(blueprint_install_dir))
        .unwrap_or_else(|| crate::update::DEFAULT_INSTALL_DIR.to_string());

    let command = crate::update::build_update_command(&apply, &install_dir)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e))?;

    // Register the job (rejects if one is already running for this server).
    let job = s
        .update_jobs
        .start(&id, command.clone())
        .await
        .map_err(|e| err_json(StatusCode::CONFLICT, e))?;

    // Run the update detached; the client polls for completion.
    let manager = s.manager.clone();
    let jobs = s.update_jobs.clone();
    let container_id = id.clone();
    tokio::spawn(async move {
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), command];
        match manager.exec_command(&container_id, &argv, crate::update::UPDATE_TIMEOUT).await {
            Ok(out) => {
                jobs.finish(&container_id, out.stdout, out.stderr, out.exit_code).await;
            }
            Err(e) => {
                jobs.fail(&container_id, e.to_string()).await;
            }
        }
    });

    Ok(Json(job))
}

/// Return the current/most-recent update job for a server.
async fn api_update_status(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<crate::update::UpdateJob>, (StatusCode, Json<ApiError>)> {
    match s.update_jobs.get(&id).await {
        Some(job) => Ok(Json(job)),
        None => Err(err_json(
            StatusCode::NOT_FOUND,
            "no update has been run for this server",
        )),
    }
}

// ---------------------------------------------------------------------------
// Blueprint import (Pterodactyl eggs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ImportEggReq {
    /// Raw Pterodactyl egg JSON (the file you export from Pterodactyl/Pelican).
    egg_json: String,
    /// Scan the egg's startup/install script for risky patterns. Default true.
    security_scan: Option<bool>,
    /// Add Nexus's default firewall rules to the generated blueprint.
    /// Default true.
    firewall_rules: Option<bool>,
}

#[derive(Serialize)]
struct ImportEggResp {
    /// The converted blueprint, ready to paste into the create form.
    blueprint_yaml: String,
    /// Summary of what was imported, so the UI needn't re-parse the YAML.
    name: String,
    game: String,
    image: String,
    variable_count: usize,
    port_count: usize,
    /// Advisory security findings from scanning the egg (may be empty).
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct BlueprintSummary {
    /// The id `GET /api/v1/blueprints/:id` and provisioning take.
    id: String,
    name: String,
    game: String,
    version: String,
}

/// The blueprints this node ships, so a billing system can offer them as
/// products without a copy of the list.
async fn api_list_blueprints() -> Json<Vec<BlueprintSummary>> {
    Json(
        SHIPPED_BLUEPRINTS
            .iter()
            .filter_map(|(id, yaml)| {
                let bp: nexus_config::Blueprint = serde_yaml::from_str(yaml).ok()?;
                Some(BlueprintSummary {
                    id: id.to_string(),
                    name: bp.metadata.name,
                    game: bp.metadata.game,
                    version: bp.metadata.version,
                })
            })
            .collect(),
    )
}

/// Return the YAML for one of the shipped blueprints, by file stem.
///
/// The Blueprints page fetches this when an operator picks a game, so the
/// create-server form is prefilled with the same blueprint the repo ships
/// rather than a copy maintained separately in the frontend.
async fn api_get_blueprint(
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let yaml = SHIPPED_BLUEPRINTS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, yaml)| *yaml)
        .ok_or_else(|| {
            err_json(
                StatusCode::NOT_FOUND,
                format!("unknown blueprint \"{}\"", id),
            )
        })?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
        yaml,
    ))
}

/// Convert a Pterodactyl egg into a Nexus blueprint.
///
/// This is the migration path off Pterodactyl/Pelican: paste an exported egg
/// and get back an equivalent blueprint, plus any security findings about what
/// the egg's scripts do. Conversion is pure — nothing is deployed here; the
/// operator reviews the result and creates a server from it.
async fn api_import_egg(
    Json(body): Json<ImportEggReq>,
) -> Result<Json<ImportEggResp>, (StatusCode, Json<ApiError>)> {
    if body.egg_json.trim().is_empty() {
        return Err(err_json(StatusCode::BAD_REQUEST, "Empty egg JSON"));
    }

    let egg = egg_importer::PterodactylEgg::from_json(&body.egg_json).map_err(|e| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("Not a valid Pterodactyl egg: {}", e),
        )
    })?;

    let converter = egg_importer::EggConverter::new()
        .security_scan(body.security_scan.unwrap_or(true))
        .add_firewall_rules(body.firewall_rules.unwrap_or(true));

    let (blueprint, warnings) = converter.convert_with_report(&egg).map_err(|e| {
        err_json(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Could not convert egg '{}': {}", egg.name, e),
        )
    })?;

    let blueprint_yaml = blueprint.to_yaml().map_err(|e| {
        err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to render blueprint: {}", e),
        )
    })?;

    Ok(Json(ImportEggResp {
        name: blueprint.metadata.name.clone(),
        game: blueprint.metadata.game.clone(),
        image: blueprint.container.image.clone(),
        variable_count: blueprint.variables.len(),
        port_count: blueprint.networking.ports.len(),
        blueprint_yaml,
        warnings,
    }))
}

// ---------------------------------------------------------------------------
// Marketplace API handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MarketplaceSearchQuery {
    q: String,
    game: Option<String>,
    provider: Option<String>,
    sort: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct ModInfoJson {
    id: String,
    name: String,
    description: String,
    author: String,
    provider: String,
    game: String,
    downloads: u64,
    rating: Option<f32>,
    latest_version: String,
    url: String,
}

async fn api_marketplace_search(
    State(s): State<S>,
    Query(q): Query<MarketplaceSearchQuery>,
) -> Result<Json<Vec<ModInfoJson>>, (StatusCode, Json<ApiError>)> {
    let sort = match q.sort.as_deref() {
        Some("updated") => SortOrder::Updated,
        Some("created") => SortOrder::Created,
        Some("name") => SortOrder::Name,
        Some("rating") => SortOrder::Rating,
        _ => SortOrder::Downloads,
    };
    let mut query = SearchQuery::new(&q.q).with_sort(sort).with_limit(q.limit.unwrap_or(24));
    if let Some(game) = &q.game {
        query = query.with_game(game);
    }

    let results = if let Some(provider) = &q.provider {
        s.marketplace
            .search(provider, &query)
            .await
            .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e.to_string()))?
    } else {
        s.marketplace
            .search_all(&query)
            .await
            .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e.to_string()))?
    };

    Ok(Json(
        results
            .into_iter()
            .map(|m| ModInfoJson {
                id: m.id,
                name: m.name,
                description: m.description,
                author: m.author,
                provider: m.provider,
                game: m.game,
                downloads: m.downloads,
                rating: m.rating,
                latest_version: m.latest_version,
                url: m.url,
            })
            .collect(),
    ))
}

async fn api_marketplace_get_mod(
    State(s): State<S>,
    Path((provider, mod_id)): Path<(String, String)>,
) -> Result<Json<ModInfoJson>, (StatusCode, Json<ApiError>)> {
    let details = s
        .marketplace
        .get_mod(&provider, &mod_id)
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e.to_string()))?;

    Ok(Json(ModInfoJson {
        id: details.info.id,
        name: details.info.name,
        description: details.full_description.unwrap_or(details.info.description),
        author: details.info.author,
        provider: details.info.provider,
        game: details.info.game,
        downloads: details.info.downloads,
        rating: details.info.rating,
        latest_version: details.info.latest_version,
        url: details.info.url,
    }))
}

// ---------------------------------------------------------------------------
// Update check API handler
// ---------------------------------------------------------------------------

/// What this node is running, and what its channel has available.
async fn api_update_check(
    State(s): State<S>,
) -> Result<Json<crate::version::UpdateStatus>, (StatusCode, Json<ApiError>)> {
    let channel = crate::version::Channel::from_env();
    Ok(Json(crate::version::check(&s.http, channel).await))
}

/// Start applying an update to the node itself.
///
/// Returns as soon as the updater is launched. It deliberately outlives this
/// process — finishing the job means restarting the service — so the client
/// polls `GET` for the result, which survives the node going away and coming
/// back.
async fn api_apply_update(
    State(s): State<S>,
) -> Result<Json<crate::selfupdate::SelfUpdateJob>, (StatusCode, Json<ApiError>)> {
    // Updating restarts the node, and a restart mid-install leaves a server
    // with a half-downloaded game. Running game servers are fine: their
    // containerd shims are independent of this process.
    for state in s.manager.list_containers().await {
        if state.install_state == crate::install::InstallState::Running {
            return Err(err_json(
                StatusCode::CONFLICT,
                format!(
                    "server {} is installing its game files — updating now would interrupt it",
                    state.name
                ),
            ));
        }
    }

    let channel = crate::version::Channel::from_env();
    let build = crate::version::BuildInfo::current();

    s.updater.start(channel.as_str(), &build.version).map(Json).map_err(|e| {
        let status = match e {
            crate::selfupdate::StartError::AlreadyRunning => StatusCode::CONFLICT,
            crate::selfupdate::StartError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
            crate::selfupdate::StartError::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        err_json(status, e.to_string())
    })
}

/// The current or most recent self-update, with its log.
async fn api_self_update_status(
    State(s): State<S>,
) -> Result<Json<crate::selfupdate::SelfUpdateJob>, (StatusCode, Json<ApiError>)> {
    match s.updater.current() {
        Some(job) => Ok(Json(job)),
        None => Err(err_json(
            StatusCode::NOT_FOUND,
            "this node has not been updated from the panel",
        )),
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn container_to_json(c: &crate::container::ContainerState) -> ContainerJson {
    ContainerJson {
        id: c.id.clone(),
        name: c.name.clone(),
        image: c.image.clone(),
        status: format!("{:?}", c.status).to_lowercase(),
        pid: c.pid,
        exit_code: c.exit_code,
        restart_count: c.restart_count,
        crash_count: c.crash_count,
        disk_used_bytes: c.disk_used_bytes,
        disk_limit_bytes: c.disk_limit_bytes,
        created_at: system_time_to_epoch(c.created_at),
        started_at: c.started_at.map(system_time_to_epoch),
        stopped_at: c.stopped_at.map(system_time_to_epoch),
        usage: None,
    }
}

/// Drop what the node keeps about a server beyond its container: backups
/// and schedules. Best-effort, after the container itself is gone.
pub(super) async fn forget_server(s: &S, id: &str) {
    if let Err(e) = s.backup_manager.delete_all(id).await {
        warn!("Backups of deleted server {} were not removed: {}", id, e);
    }
    s.schedule_manager.delete_all(id).await;
}

fn backup_to_json(b: crate::backup::BackupInfo) -> BackupJson {
    BackupJson {
        id: b.id,
        container_id: b.container_id,
        name: b.name,
        size: b.size,
        created_at: b.created_at,
        checksum: b.checksum,
        status: format!("{:?}", b.status).to_lowercase(),
        error: b.error,
        include: b.include,
    }
}

fn schedule_to_json(s: crate::schedule::ScheduleInfo, running: bool) -> ScheduleJson {
    ScheduleJson {
        id: s.id,
        container_id: s.container_id,
        name: s.name,
        cron_expression: s.cron_expression,
        timezone: s.timezone,
        is_active: s.is_active,
        last_error: s.last_error,
        running,
        tasks: s
            .tasks
            .into_iter()
            .map(|t| ScheduleTaskJson {
                action: match t.task_type {
                    ScheduleTaskType::Command => "command".to_string(),
                    ScheduleTaskType::Power => "power".to_string(),
                    ScheduleTaskType::Backup => "backup".to_string(),
                },
                payload: t.payload,
                time_offset: t.time_offset,
            })
            .collect(),
        created_at: s.created_at,
        last_run: s.last_run_at,
        next_run: s.next_run_at,
    }
}

fn task_from_json(t: ScheduleTaskJson) -> ScheduleTask {
    ScheduleTask {
        task_type: match t.action.as_str() {
            "power" => ScheduleTaskType::Power,
            "backup" => ScheduleTaskType::Backup,
            _ => ScheduleTaskType::Command,
        },
        payload: t.payload,
        time_offset: t.time_offset,
    }
}

#[cfg(test)]
mod tests {
    use super::{default_mods_dir, mods_dir_for, StartUpdateReq, APP_JS, SHIPPED_BLUEPRINTS};

    /// No two panel features may claim the same `NX.<name>` handler.
    ///
    /// `app.js` is one flat script, so a second `NX.foo = function` silently
    /// replaces the first — the two features look fine in isolation and one of
    /// them is simply dead at runtime. That happened once already: the
    /// game-file install panel and the marketplace mod dialog both defined
    /// `NX.renderInstallJob`, and the install panel's status never rendered.
    #[test]
    fn panel_handlers_are_uniquely_named() {
        let mut seen: Vec<&str> = Vec::new();
        let mut duplicates: Vec<&str> = Vec::new();

        for line in APP_JS.lines() {
            let line = line.trim_start();
            let Some(rest) = line.strip_prefix("NX.") else {
                continue;
            };
            // Only definitions (`NX.name = …`), not call sites.
            let Some((name, tail)) = rest.split_once('=') else {
                continue;
            };
            let name = name.trim();
            // Skip comparisons (`NX.a === b`) and property paths.
            if tail.starts_with('=') || name.contains('.') || name.contains('(') {
                continue;
            }
            // Only function definitions. Plain state (`NX.containers = […]`)
            // is assigned from several places by design; it is redefining a
            // *handler* that silently unhooks a feature.
            let tail = tail.trim_start();
            if !(tail.starts_with("function") || tail.starts_with("async function")) {
                continue;
            }
            if seen.contains(&name) {
                duplicates.push(name);
            } else {
                seen.push(name);
            }
        }

        assert!(
            duplicates.is_empty(),
            "these NX handlers are defined more than once, so all but the last are dead: {:?}",
            duplicates
        );
    }

    /// Every blueprint the panel serves must parse and validate against the
    /// current schema.
    ///
    /// The Blueprints page used to serve YAML hand-copied into `app.js`. It
    /// drifted out of the schema — every game failed with a parse error the
    /// moment an operator clicked it — because nothing here ever parsed what
    /// the UI actually served. `nexus-config` validates the files in
    /// `/blueprints`; this validates what reaches the browser.
    #[test]
    fn every_served_blueprint_parses_and_validates() {
        assert!(
            !SHIPPED_BLUEPRINTS.is_empty(),
            "no blueprints are served to the UI"
        );

        for (id, yaml) in SHIPPED_BLUEPRINTS {
            let blueprint: nexus_config::Blueprint = serde_yaml::from_str(yaml)
                .unwrap_or_else(|e| panic!("blueprint \"{}\" failed to parse: {}", id, e));
            blueprint
                .validate()
                .unwrap_or_else(|e| panic!("blueprint \"{}\" failed validation: {}", id, e));
        }
    }

    /// Each blueprint card in the UI must resolve to a blueprint the node
    /// serves, or clicking it 404s.
    #[test]
    fn every_blueprint_card_in_the_ui_is_served() {
        let app_js = super::APP_JS;
        let start = app_js
            .find("const BLUEPRINTS = [")
            .expect("BLUEPRINTS list not found in app.js");
        let list =
            &app_js[start..app_js[start..].find("];").expect("unterminated BLUEPRINTS") + start];

        let card_ids: Vec<&str> = list
            .match_indices("{ id: '")
            .map(|(i, pat)| {
                let rest = &list[i + pat.len()..];
                &rest[..rest.find('\'').expect("unterminated blueprint id")]
            })
            .collect();

        assert!(
            !card_ids.is_empty(),
            "no blueprint cards parsed out of app.js"
        );

        for id in card_ids {
            assert!(
                SHIPPED_BLUEPRINTS.iter().any(|(name, _)| *name == id),
                "the UI offers blueprint \"{}\" but the node serves no such blueprint",
                id
            );
        }
    }

    #[test]
    fn start_update_body_may_omit_the_strategy() {
        // An empty body (or one carrying only install_dir) means "use the
        // server's blueprint" — the strategy must deserialize as None rather
        // than failing on the missing `type` tag.
        let empty: StartUpdateReq = serde_json::from_str("{}").unwrap();
        assert!(empty.apply.is_none());
        assert!(empty.install_dir.is_none());

        let dir_only: StartUpdateReq =
            serde_json::from_str(r#"{"install_dir":"/srv/game"}"#).unwrap();
        assert!(dir_only.apply.is_none());
        assert_eq!(dir_only.install_dir.as_deref(), Some("/srv/game"));
    }

    #[test]
    fn start_update_body_still_accepts_an_explicit_strategy() {
        // The flattened shape the UI already sends must keep working.
        let body: StartUpdateReq =
            serde_json::from_str(r#"{"type":"steam_cmd","app_id":258550,"install_dir":"/x"}"#)
                .unwrap();
        match body.apply {
            Some(nexus_config::UpdateApply::SteamCmd { app_id, beta }) => {
                assert_eq!(app_id, 258550);
                assert_eq!(beta, None);
            }
            other => panic!("expected SteamCmd, got {:?}", other.is_some()),
        }
        assert_eq!(body.install_dir.as_deref(), Some("/x"));

        let depot: StartUpdateReq =
            serde_json::from_str(r#"{"type":"depot_downloader","app_id":1,"branch":"beta"}"#)
                .unwrap();
        assert!(matches!(
            depot.apply,
            Some(nexus_config::UpdateApply::DepotDownloader { app_id: 1, .. })
        ));
    }

    #[test]
    fn mods_dir_defaults_per_provider() {
        assert_eq!(default_mods_dir("umod"), "oxide/plugins");
        assert_eq!(default_mods_dir("codefling"), "oxide/plugins");
        assert_eq!(default_mods_dir("lone_design"), "oxide/plugins");
        assert_eq!(default_mods_dir("spigot"), "plugins");
        // Workshop items are mod folders the game loads from the server root.
        assert_eq!(default_mods_dir("steam_workshop"), ".");
    }

    #[test]
    fn mods_dir_honors_framework() {
        // Framework selects the plugin folder for Rust servers...
        assert_eq!(mods_dir_for(Some("carbon"), "umod"), "carbon/plugins");
        assert_eq!(mods_dir_for(Some("Carbon"), "umod"), "carbon/plugins");
        assert_eq!(mods_dir_for(Some("oxide"), "umod"), "oxide/plugins");
        // ...and falls back to the provider guess when absent/blank/unknown.
        assert_eq!(mods_dir_for(None, "umod"), "oxide/plugins");
        assert_eq!(mods_dir_for(Some("  "), "codefling"), "oxide/plugins");
        assert_eq!(mods_dir_for(Some("bogus"), "spigot"), "plugins");
        // A Workshop mod ignores the Rust framework hint entirely — dropping
        // an `@Mod` folder into oxide/plugins would leave it unloadable.
        assert_eq!(mods_dir_for(Some("carbon"), "steam_workshop"), ".");
        assert_eq!(mods_dir_for(None, "steam_workshop"), ".");
    }
}
