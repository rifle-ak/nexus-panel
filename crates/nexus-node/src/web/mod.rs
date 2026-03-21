//! Embedded web panel for Nexus Node
//!
//! Serves the admin panel UI and REST API on a configurable HTTP port.
//! All static assets are compiled into the binary via include_str!().

use crate::backup::BackupManager;
use crate::container::ContainerManager;
use crate::error::NodeError;
use crate::files::FileManager;
use crate::health::HealthChecker;
use crate::metrics::Metrics;
use crate::schedule::{ScheduleManager, ScheduleTask, ScheduleTaskType};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{error, info};

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
    pub node_id: String,
    pub data_dir: String,
    pub start_time: SystemTime,
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

    let app = Router::new()
        // ── Static assets ────────────────────────────────────────────
        .route("/", get(serve_index))
        .route("/css/style.css", get(serve_css))
        .route("/js/app.js", get(serve_js))
        // ── Node API ─────────────────────────────────────────────────
        .route("/api/v1/node/info", get(api_node_info))
        .route("/api/v1/node/health", get(api_node_health))
        .route("/api/v1/node/metrics", get(api_node_metrics))
        // ── Container CRUD ───────────────────────────────────────────
        .route(
            "/api/v1/containers",
            get(api_list_containers).post(api_create_container),
        )
        .route("/api/v1/containers/{id}", get(api_get_container).delete(api_delete_container))
        .route("/api/v1/containers/{id}/start", post(api_start_container))
        .route("/api/v1/containers/{id}/stop", post(api_stop_container))
        .route("/api/v1/containers/{id}/restart", post(api_restart_container))
        .route("/api/v1/containers/{id}/kill", post(api_kill_container))
        .route("/api/v1/containers/{id}/suspend", post(api_suspend_container))
        .route("/api/v1/containers/{id}/unsuspend", post(api_unsuspend_container))
        .route("/api/v1/containers/{id}/command", post(api_send_command))
        // ── Files ────────────────────────────────────────────────────
        .route("/api/v1/containers/{id}/files", get(api_list_files))
        .route("/api/v1/containers/{id}/files/read", get(api_read_file))
        .route("/api/v1/containers/{id}/files/write", post(api_write_file))
        .route("/api/v1/containers/{id}/files/delete", post(api_delete_files))
        .route("/api/v1/containers/{id}/files/rename", post(api_rename_file))
        .route("/api/v1/containers/{id}/files/mkdir", post(api_create_dir))
        // ── Backups ──────────────────────────────────────────────────
        .route(
            "/api/v1/containers/{id}/backups",
            get(api_list_backups).post(api_create_backup),
        )
        .route("/api/v1/containers/{id}/backups/{backup_id}/restore", post(api_restore_backup))
        .route("/api/v1/containers/{id}/backups/{backup_id}", delete(api_delete_backup))
        // ── Schedules ────────────────────────────────────────────────
        .route(
            "/api/v1/containers/{id}/schedules",
            get(api_list_schedules).post(api_create_schedule),
        )
        .route(
            "/api/v1/containers/{id}/schedules/{schedule_id}",
            put(api_update_schedule).delete(api_delete_schedule),
        )
        .route("/api/v1/containers/{id}/schedules/{schedule_id}/trigger", post(api_trigger_schedule))
        .with_state(shared);

    let addr: std::net::SocketAddr = bind_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;

    info!("Web panel listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
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
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
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
        (t + d.total_space(), u + (d.total_space() - d.available_space()))
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
    let out: Vec<ContainerJson> = containers.iter().map(|c| container_to_json(c)).collect();
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((StatusCode::CREATED, Json(backup_to_json(info))))
}

async fn api_restore_backup(
    State(s): State<S>,
    Path((id, backup_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.backup_manager
        .restore_backup(&id, &backup_id, false)
        .await
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_delete_backup(
    State(s): State<S>,
    Path((id, backup_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.backup_manager
        .delete_backup(&id, &backup_id)
        .await
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(schedule_to_json(info)))
}

async fn api_delete_schedule(
    State(s): State<S>,
    Path((id, schedule_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    s.schedule_manager
        .delete_schedule(&id, &schedule_id)
        .await
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn api_trigger_schedule(
    State(s): State<S>,
    Path((id, schedule_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    // Create a no-op callback for manual triggers via the web API.
    // In production, the schedule runner provides the real callback that
    // dispatches commands to the container manager.
    let noop: crate::schedule::ScheduleCallback = Arc::new(|_container_id, _task| {
        Box::pin(async { Ok(()) })
    });
    s.schedule_manager
        .trigger_schedule(&id, &schedule_id, &noop)
        .await
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
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
