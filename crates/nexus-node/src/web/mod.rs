//! Embedded web panel for Nexus Node
//!
//! Serves the admin panel UI and REST API on a configurable HTTP port.
//! All static assets are compiled into the binary via include_str!().

pub mod auth;

use crate::backup::BackupManager;
use crate::container::ContainerManager;
use crate::error::NodeError;
use crate::files::FileManager;
use crate::health::HealthChecker;
use crate::metrics::Metrics;
use crate::schedule::{ScheduleManager, ScheduleTask, ScheduleTaskType};
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
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

    // Protected API routes — everything that can read or mutate node state.
    // The auth middleware runs for every route registered before `route_layer`.
    let protected = Router::new()
        // ── Node API ─────────────────────────────────────────────────
        .route("/api/v1/node/info", get(api_node_info))
        .route("/api/v1/node/health", get(api_node_health))
        .route("/api/v1/node/metrics", get(api_node_metrics))
        .route("/api/v1/node/update-check", get(api_update_check))
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
        .route("/api/v1/containers/:id/mods/install", post(api_install_mod))
        // ── Game-file update (SteamCMD / DepotDownloader) ────────────
        .route(
            "/api/v1/containers/:id/update",
            get(api_update_status).post(api_start_update),
        )
        // ── Marketplace ─────────────────────────────────────────────
        .route("/api/v1/marketplace/search", get(api_marketplace_search))
        .route(
            "/api/v1/marketplace/mods/:provider/:mod_id",
            get(api_marketplace_get_mod),
        )
        .route_layer(middleware::from_fn_with_state(shared.clone(), require_auth));

    // Public routes — static assets and the auth endpoints needed to log in.
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/css/style.css", get(serve_css))
        .route("/js/app.js", get(serve_js))
        .route("/api/v1/auth/config", get(api_auth_config))
        .route("/api/v1/auth/login", post(api_login))
        .route("/api/v1/auth/logout", post(api_logout))
        .merge(protected)
        .with_state(shared);

    let addr: std::net::SocketAddr = bind_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!("Web panel listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Authentication middleware + handlers
// ---------------------------------------------------------------------------

/// Middleware that gates protected routes behind a valid session token or key.
async fn require_auth(State(s): State<S>, req: Request, next: Next) -> Response {
    // Auth disabled: allow through (intended only for loopback/dev use).
    if !s.auth.enabled {
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

    let headers = req.headers();

    // Accept a bearer session token, a session cookie, or a raw API key.
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::bearer_from_header);

    if let Some(token) = bearer {
        if s.sessions.validate(token).await || s.auth.verify_api_key(token) {
            return next.run(req).await;
        }
    }

    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::session_from_cookie)
    {
        if s.sessions.validate(token).await {
            return next.run(req).await;
        }
    }

    if let Some(key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        if s.auth.verify_api_key(key) {
            return next.run(req).await;
        }
    }

    err_json(StatusCode::UNAUTHORIZED, "Authentication required").into_response()
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
async fn api_login(
    State(s): State<S>,
    Json(body): Json<LoginReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    // If auth is disabled there is nothing to log in to; report success with
    // an empty token so the UI can proceed without a login screen.
    if !s.auth.enabled {
        return Ok(Json(LoginResponse {
            token: String::new(),
            expires_in_secs: 0,
        }));
    }

    let ok = body.password.as_deref().map(|p| s.auth.verify_password(p)).unwrap_or(false)
        || body.api_key.as_deref().map(|k| s.auth.verify_api_key(k)).unwrap_or(false);

    if !ok {
        return Err(err_json(StatusCode::UNAUTHORIZED, "Invalid credentials"));
    }

    let token = s.sessions.create().await;
    Ok(Json(LoginResponse {
        token,
        expires_in_secs: s.sessions.ttl_secs(),
    }))
}

/// Revoke the caller's session token.
async fn api_logout(State(s): State<S>, req: Request) -> impl IntoResponse {
    if let Some(token) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::bearer_from_header)
    {
        s.sessions.revoke(token).await;
    }
    Json(serde_json::json!({ "ok": true }))
}

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
    created_at: u64,
    started_at: Option<u64>,
    stopped_at: Option<u64>,
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
}

#[derive(Deserialize)]
struct CreateBackupReq {
    name: Option<String>,
    paths: Option<Vec<String>>,
}

#[derive(Serialize)]
struct ScheduleJson {
    id: String,
    container_id: String,
    name: String,
    cron_expression: String,
    is_active: bool,
    tasks: Vec<ScheduleTaskJson>,
    created_at: i64,
    last_run: Option<i64>,
    next_run: Option<i64>,
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
    tasks: Vec<ScheduleTaskJson>,
}

#[derive(Deserialize)]
struct UpdateScheduleReq {
    name: Option<String>,
    cron_expression: Option<String>,
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
                _ => "fail",
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

async fn api_list_containers(State(s): State<S>) -> impl IntoResponse {
    let containers = s.manager.list_containers().await;
    let out: Vec<ContainerJson> = containers.iter().map(container_to_json).collect();
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
    Ok(Json(container_to_json(&c)))
}

async fn api_create_container(
    State(s): State<S>,
    Json(body): Json<CreateContainerReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let config: nexus_config::GameConfig = serde_yaml::from_str(&body.config_yaml)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, format!("Invalid YAML: {}", e)))?;

    let id = s
        .manager
        .create_container(&config, body.server_id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    if body.auto_start.unwrap_or(false) {
        let _ = s.manager.start_container(&id).await;
    }

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

async fn api_start_container(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.manager
        .start_container(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_stop_container(
    State(s): State<S>,
    Path(id): Path<String>,
    body: Option<Json<StopReq>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let timeout = body.and_then(|b| b.timeout);
    s.manager
        .stop_container(&id, timeout)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_restart_container(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.manager
        .restart_container(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
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
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.manager
        .delete_container(&id, true)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_suspend_container(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.manager
        .suspend_container(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_unsuspend_container(
    State(s): State<S>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.manager
        .unsuspend_container(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
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
    Path(id): Path<String>,
    Json(body): Json<WriteFileReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    let bytes_written = fm
        .write_file(&body.path, body.content.as_bytes(), true)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "bytes_written": bytes_written })))
}

async fn api_delete_files(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<DeleteFilesReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    let deleted = fm
        .delete(&body.paths, body.recursive.unwrap_or(false))
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn api_rename_file(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<RenameFileReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let fm = file_manager(&s, &id);
    fm.rename(&body.old_path, &body.new_path)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
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
    let name = body.name.unwrap_or_else(|| "manual-backup".to_string());
    let include = body.paths.unwrap_or_default();
    let info = s
        .backup_manager
        .create_backup(&id, &name, &include, &[])
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok((StatusCode::CREATED, Json(backup_to_json(info))))
}

async fn api_restore_backup(
    State(s): State<S>,
    Path((id, backup_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.backup_manager
        .restore_backup(&id, &backup_id, false)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
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
    Ok(Json(schedules.into_iter().map(schedule_to_json).collect()))
}

async fn api_create_schedule(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<CreateScheduleReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    let tasks: Vec<ScheduleTask> = body.tasks.into_iter().map(task_from_json).collect();
    let info = s
        .schedule_manager
        .create_schedule(&id, &body.name, &body.cron_expression, true, tasks)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok((StatusCode::CREATED, Json(schedule_to_json(info))))
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
            body.is_active,
            tasks,
        )
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(schedule_to_json(info)))
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

#[derive(Serialize)]
struct InstallModResp {
    /// Installed file path, relative to the server root.
    file_path: String,
    file_size: u64,
    checksum: String,
}

/// Sensible default mods directory for a provider when the caller doesn't
/// specify one. The Rust marketplaces install Oxide plugins.
fn default_mods_dir(provider: &str) -> &'static str {
    match provider {
        "umod" | "codefling" | "lone_design" => "oxide/plugins",
        _ => "plugins",
    }
}

/// Resolve the install directory when the caller didn't set `target_dir`.
/// An explicit `framework` wins (Rust servers can run Oxide *or* Carbon, which
/// use different plugin folders); otherwise fall back to a per-provider guess.
fn mods_dir_for(framework: Option<&str>, provider: &str) -> String {
    match framework.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) if f.eq_ignore_ascii_case("carbon") => "carbon/plugins".to_string(),
        Some(f) if f.eq_ignore_ascii_case("oxide") => "oxide/plugins".to_string(),
        _ => default_mods_dir(provider).to_string(),
    }
}

/// Download a marketplace mod and install it into a running server's mods
/// directory. The download itself (fetch + checksum verify) lives in the
/// marketplace adapter; here we resolve a jail-safe target path and stream it in.
async fn api_install_mod(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<InstallModReq>,
) -> Result<Json<InstallModResp>, (StatusCode, Json<ApiError>)> {
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

    let result = s
        .marketplace
        .download_mod(
            &body.provider,
            &body.mod_id,
            body.version.as_deref(),
            &target,
        )
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e.to_string()))?;

    // Report the path relative to the server root so the UI can show it.
    let rel = result
        .file_path
        .strip_prefix(fm.server_dir())
        .unwrap_or(&result.file_path)
        .to_string_lossy()
        .to_string();

    Ok(Json(InstallModResp {
        file_path: rel,
        file_size: result.file_size,
        checksum: result.checksum,
    }))
}

// ---------------------------------------------------------------------------
// Game-file update executor
// ---------------------------------------------------------------------------

/// Request to run a game-file update. The body carries an `UpdateApply`
/// strategy (tagged by `type`, e.g. `steam_cmd` / `depot_downloader`) plus an
/// optional install directory.
#[derive(Deserialize)]
struct StartUpdateReq {
    #[serde(flatten)]
    apply: nexus_config::UpdateApply,
    /// Directory inside the container to install into. Defaults to the
    /// blueprint working dir (`/home/container`).
    install_dir: Option<String>,
}

/// Kick off a game-file update for a server. The update runs as a background
/// job (downloads can take many minutes); this returns immediately with the
/// job snapshot, and the client polls `GET .../update` for progress.
async fn api_start_update(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(body): Json<StartUpdateReq>,
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

    let install_dir = body
        .install_dir
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(crate::update::DEFAULT_INSTALL_DIR)
        .to_string();

    let command = crate::update::build_update_command(&body.apply, &install_dir)
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

#[derive(Serialize)]
struct UpdateCheckResponse {
    current_version: String,
    latest_version: String,
    update_available: bool,
}

async fn api_update_check(
    State(_s): State<S>,
) -> Result<Json<UpdateCheckResponse>, (StatusCode, Json<ApiError>)> {
    let current = env!("CARGO_PKG_VERSION").to_string();

    // Check the latest release from the GitHub API
    let latest = match reqwest::Client::new()
        .get("https://api.github.com/repos/rifle-ak/nexus-panel/releases/latest")
        .header("User-Agent", "nexus-panel")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                body["tag_name"]
                    .as_str()
                    .unwrap_or(&current)
                    .trim_start_matches('v')
                    .to_string()
            } else {
                current.clone()
            }
        }
        Err(_) => current.clone(),
    };

    let update_available = is_newer(&latest, &current);

    Ok(Json(UpdateCheckResponse {
        current_version: current,
        latest_version: latest,
        update_available,
    }))
}

/// Whether `latest` is a strictly newer release than `current`, compared with
/// semantic-version ordering (so 0.10.0 correctly beats 0.9.0). Falls back to a
/// plain string inequality only if either value is not valid semver.
fn is_newer(latest: &str, current: &str) -> bool {
    match (
        semver::Version::parse(latest),
        semver::Version::parse(current),
    ) {
        (Ok(l), Ok(c)) => l > c,
        _ => latest != current,
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
        created_at: system_time_to_epoch(c.created_at),
        started_at: c.started_at.map(system_time_to_epoch),
        stopped_at: c.stopped_at.map(system_time_to_epoch),
    }
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
    }
}

fn schedule_to_json(s: crate::schedule::ScheduleInfo) -> ScheduleJson {
    ScheduleJson {
        id: s.id,
        container_id: s.container_id,
        name: s.name,
        cron_expression: s.cron_expression,
        is_active: s.is_active,
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
    use super::{default_mods_dir, is_newer, mods_dir_for};

    #[test]
    fn mods_dir_defaults_per_provider() {
        assert_eq!(default_mods_dir("umod"), "oxide/plugins");
        assert_eq!(default_mods_dir("codefling"), "oxide/plugins");
        assert_eq!(default_mods_dir("lone_design"), "oxide/plugins");
        assert_eq!(default_mods_dir("spigot"), "plugins");
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
    }

    #[test]
    fn semver_update_comparison() {
        // The bug this replaces: string comparison ranked 0.9.0 above 0.10.0.
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("1.0.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        // Non-semver values fall back to inequality (no false "update").
        assert!(!is_newer("unknown", "unknown"));
    }
}
