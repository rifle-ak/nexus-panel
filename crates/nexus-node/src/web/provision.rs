//! Provisioning and single sign-on endpoints for billing systems.
//!
//! These are what the WHMCS module (`whmcs/`) talks to. They are admin-only:
//! a billing system holds a node API key, and the scoped sessions it mints
//! for customers cannot reach anything under `/api/v1/provision`.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use super::{err_json, node_err_status, spawn_install, ApiError, S, SHIPPED_BLUEPRINTS};
use crate::provision::{
    allocate_ports, apply_overrides, now_epoch, resolve_ports, validate_external_id, AssignedPort,
    Overrides, ProvisionError, ProvisionRecord, ProvisionResources,
};
use crate::web::auth::{SSO_TOKEN_DEFAULT_TTL, SSO_TOKEN_MAX_TTL};

type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

/// A request to create a server for a billing service.
#[derive(Deserialize)]
pub struct ProvisionCreateReq {
    /// The billing system's id for the service. Creating twice with the same
    /// id returns the server made the first time.
    pub external_id: String,
    pub name: String,
    /// A shipped blueprint id (`minecraft-paper`, `rust`, …).
    pub blueprint: Option<String>,
    /// Or a complete blueprint, for products the shipped set does not cover.
    pub blueprint_yaml: Option<String>,
    pub memory_mb: Option<u32>,
    pub cpu_millicores: Option<u32>,
    pub disk_mb: Option<u32>,
    /// Blueprint variables to set (`MAX_PLAYERS`, `SERVER_NAME`, …).
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Pin the primary port instead of taking the next free one.
    pub port: Option<u16>,
    /// Start the server once its game files are installed. Default true: a
    /// paid-for server should be up without the customer pressing anything.
    pub auto_start: Option<bool>,
    /// Opaque owner reference for the operator's benefit.
    pub owner: Option<String>,
}

/// A request to change what a provisioned server is sold with.
#[derive(Deserialize)]
pub struct ChangePackageReq {
    pub name: Option<String>,
    pub memory_mb: Option<u32>,
    pub cpu_millicores: Option<u32>,
    pub disk_mb: Option<u32>,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Start the server again afterwards if it was running. Default true.
    pub restart: Option<bool>,
}

#[derive(Deserialize)]
pub struct LookupQuery {
    pub external_id: Option<String>,
}

/// A provisioned server as the billing system sees it: its record plus the
/// live state that decides what the customer's overview page shows.
#[derive(Serialize)]
pub struct ProvisionedServer {
    pub id: String,
    pub external_id: String,
    pub name: String,
    pub blueprint: String,
    pub game: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub resources: ProvisionResources,
    pub ports: Vec<AssignedPort>,
    /// The port customers connect to.
    pub primary_port: Option<u16>,
    /// The node's public address, when the operator configured one.
    pub ip: Option<String>,
    pub variables: BTreeMap<String, String>,
    pub status: String,
    pub install_state: crate::install::InstallState,
    pub installing: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Serialize)]
pub struct UsageResp {
    pub id: String,
    pub external_id: String,
    pub status: String,
    pub disk_used_bytes: u64,
    pub disk_limit_bytes: u64,
    pub memory_limit_bytes: u64,
}

#[derive(Deserialize)]
pub struct SsoReq {
    pub server_id: String,
    /// Who this is, for the audit log (a client id or email).
    pub subject: Option<String>,
    /// How long the link stays valid (default 60 s, max 300 s).
    pub ttl_secs: Option<u64>,
    /// How long the resulting panel session lasts; capped by the node's
    /// `WEB_SESSION_TTL_SECS`.
    pub session_ttl_secs: Option<u64>,
}

#[derive(Serialize)]
pub struct SsoResp {
    pub token: String,
    /// Path on the panel that redeems the token; the caller prefixes the
    /// panel's public URL.
    pub path: String,
    pub expires_in_secs: u64,
}

fn provision_err(e: ProvisionError) -> (StatusCode, Json<ApiError>) {
    let status = match e {
        ProvisionError::Invalid(_) => StatusCode::BAD_REQUEST,
        ProvisionError::PortConflict(_) => StatusCode::CONFLICT,
        ProvisionError::NoPortsAvailable(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    err_json(status, e.to_string())
}

/// Parse a blueprint memory/disk size (`4Gi`, `512Mi`) into MiB.
fn size_to_mb(size: &str) -> u32 {
    let s = size.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("Gi") {
        (n, 1024.0)
    } else if let Some(n) = s.strip_suffix("Mi") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix("Ti") {
        (n, 1024.0 * 1024.0)
    } else {
        (s, 1.0 / (1024.0 * 1024.0))
    };
    num.trim().parse::<f64>().map(|v| (v * mult).round() as u32).unwrap_or(0)
}

fn resources_of(config: &nexus_config::GameConfig) -> ProvisionResources {
    ProvisionResources {
        memory_mb: size_to_mb(&config.resources.memory.max),
        cpu_millicores: config.resources.cpu.max,
        disk_mb: size_to_mb(&config.resources.disk.min),
    }
}

/// Every port any server on this node will bind, from their stored
/// blueprints. Servers without a stored blueprint predate persistence and
/// contribute nothing — the operator created them by hand and owns any
/// clash.
async fn ports_in_use(s: &S) -> HashSet<u16> {
    let mut used = HashSet::new();
    for state in s.manager.list_containers().await {
        if let Some(config) = s.manager.load_blueprint(&state.id).await {
            used.extend(resolve_ports(&config).into_iter().map(|p| p.port));
        }
    }
    used
}

async fn describe(s: &S, record: ProvisionRecord) -> ApiResult<ProvisionedServer> {
    let state = match s.manager.get_state(&record.container_id).await {
        Ok(state) => state,
        Err(crate::error::NodeError::ContainerNotFound(_)) => {
            // The server went away under the record (deleted from the panel
            // by hand). Drop the record so the billing system's next create
            // makes a fresh one instead of pointing at nothing forever.
            s.provision.remove(&record.container_id).await;
            return Err(err_json(
                StatusCode::NOT_FOUND,
                format!(
                    "server {} no longer exists on this node",
                    record.container_id
                ),
            ));
        }
        Err(e) => return Err(err_json(node_err_status(&e), e.to_string())),
    };
    let installing = matches!(state.install_state, crate::install::InstallState::Running);
    Ok(ProvisionedServer {
        id: record.container_id,
        external_id: record.external_id,
        name: state.name.clone(),
        blueprint: record.blueprint,
        game: record.game,
        owner: record.owner,
        resources: record.resources,
        primary_port: record.ports.first().map(|p| p.port),
        ports: record.ports,
        ip: s.provision_settings.public_ip.clone(),
        variables: record.variables,
        status: format!("{:?}", state.status).to_lowercase(),
        install_state: state.install_state,
        installing,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

/// Load the blueprint a request names, or the one it carries.
fn load_blueprint(req: &ProvisionCreateReq) -> ApiResult<(nexus_config::GameConfig, String)> {
    if let Some(yaml) = req.blueprint_yaml.as_deref().filter(|y| !y.trim().is_empty()) {
        let config = nexus_config::GameConfig::from_yaml(yaml)
            .map_err(|e| err_json(StatusCode::BAD_REQUEST, format!("Invalid blueprint: {}", e)))?;
        return Ok((config, "custom".to_string()));
    }
    let id =
        req.blueprint
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .ok_or_else(|| {
                err_json(
                    StatusCode::BAD_REQUEST,
                    "one of blueprint (a shipped blueprint id) or blueprint_yaml is required",
                )
            })?;
    let yaml = SHIPPED_BLUEPRINTS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, yaml)| *yaml)
        .ok_or_else(|| {
            err_json(
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown blueprint {:?}; this node ships: {}",
                    id,
                    SHIPPED_BLUEPRINTS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
                ),
            )
        })?;
    let config = nexus_config::GameConfig::from_yaml(yaml).map_err(|e| {
        err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("shipped blueprint {} does not parse: {}", id, e),
        )
    })?;
    Ok((config, id.to_string()))
}

/// Create a server for a billing service.
///
/// Idempotent on `external_id`: WHMCS retries a create that timed out, and
/// the second call must find the first call's server rather than allocate
/// another. Returns 201 when a server was made, 200 when it already existed.
pub(super) async fn api_provision_create(
    State(s): State<S>,
    Json(req): Json<ProvisionCreateReq>,
) -> ApiResult<(StatusCode, Json<ProvisionedServer>)> {
    validate_external_id(&req.external_id).map_err(provision_err)?;

    // Hold the lock across "is there one already?" and "make one", so two
    // concurrent creates for the same service cannot both make a server.
    let _guard = s.provision.lock.lock().await;

    if let Some(existing) = s.provision.find_by_external_id(&req.external_id).await {
        match describe(&s, existing).await {
            Ok(server) => {
                info!(
                    "Provisioning for {} found existing server {}",
                    req.external_id, server.id
                );
                return Ok((StatusCode::OK, Json(server)));
            }
            // Gone: the record was just dropped; fall through and create.
            Err((StatusCode::NOT_FOUND, _)) => {}
            Err(e) => return Err(e),
        }
    }

    let (mut config, blueprint) = load_blueprint(&req)?;

    let overrides = Overrides {
        name: Some(req.name.clone()),
        memory_mb: req.memory_mb,
        cpu_millicores: req.cpu_millicores,
        disk_mb: req.disk_mb,
        variables: req.variables.clone(),
    };
    apply_overrides(&mut config, &overrides).map_err(provision_err)?;

    let used = ports_in_use(&s).await;
    let ports = allocate_ports(
        &mut config,
        &used,
        s.provision_settings.port_range,
        req.port,
    )
    .map_err(provision_err)?;

    config.validate().map_err(|e| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("blueprint is not valid after applying the product: {}", e),
        )
    })?;

    let id = s
        .manager
        .create_container(&config, None)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    // Ports are the node's to assign. A billing system that sent SERVER_PORT
    // as a variable was overruled by the allocator above, and must stay
    // overruled when its variables are re-applied on a package change.
    let port_variables: Vec<&str> = ports.iter().filter_map(|p| p.variable.as_deref()).collect();
    let variables: BTreeMap<String, String> = req
        .variables
        .into_iter()
        .filter(|(k, _)| !port_variables.contains(&k.as_str()))
        .collect();

    let now = now_epoch();
    let record = ProvisionRecord {
        container_id: id.clone(),
        external_id: req.external_id.clone(),
        name: config.metadata.name.clone(),
        blueprint,
        game: config.metadata.game.clone(),
        owner: req.owner.clone(),
        resources: resources_of(&config),
        ports,
        variables,
        created_at: now,
        updated_at: now,
    };

    if let Err(e) = s.provision.save(record.clone()).await {
        // Without the record the billing system's retry would make a second
        // server; better to have made none.
        error!(
            "Failed to write provisioning record for {}: {}; removing the server",
            id, e
        );
        let _ = s.manager.delete_container(&id, true).await;
        return Err(err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not persist provisioning record: {}", e),
        ));
    }

    let auto_start = req.auto_start.unwrap_or(true);
    let installing = spawn_install(&s, &id, auto_start).await.is_some();
    if auto_start && !installing {
        if let Err(e) = s.manager.start_container(&id).await {
            error!("Provisioned server {} did not start: {}", id, e);
        }
    }

    info!(
        "Provisioned server {} for {} ({} on {:?})",
        id,
        record.external_id,
        record.game,
        record.ports.iter().map(|p| p.port).collect::<Vec<_>>()
    );

    let server = describe(&s, record).await?;
    Ok((StatusCode::CREATED, Json(server)))
}

/// List provisioned servers, or look one up by the billing system's id.
pub(super) async fn api_provision_list(
    State(s): State<S>,
    Query(q): Query<LookupQuery>,
) -> ApiResult<Json<Vec<ProvisionedServer>>> {
    if let Some(external_id) = q.external_id.as_deref().filter(|e| !e.is_empty()) {
        return match s.provision.find_by_external_id(external_id).await {
            Some(record) => Ok(Json(vec![describe(&s, record).await?])),
            None => Ok(Json(Vec::new())),
        };
    }
    let mut out = Vec::new();
    for record in s.provision.list().await {
        if let Ok(server) = describe(&s, record).await {
            out.push(server);
        }
    }
    Ok(Json(out))
}

pub(super) async fn api_provision_get(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<Json<ProvisionedServer>> {
    let record = s.provision.get(&id).await.ok_or_else(|| {
        err_json(
            StatusCode::NOT_FOUND,
            "no provisioned server with that id on this node",
        )
    })?;
    Ok(Json(describe(&s, record).await?))
}

/// Apply a package change: new limits and variables, then rebuild the
/// container so they take effect. Ports are kept — a customer's address does
/// not change because they bought more memory.
pub(super) async fn api_provision_change_package(
    State(s): State<S>,
    Path(id): Path<String>,
    Json(req): Json<ChangePackageReq>,
) -> ApiResult<Json<ProvisionedServer>> {
    let mut record = s.provision.get(&id).await.ok_or_else(|| {
        err_json(
            StatusCode::NOT_FOUND,
            "no provisioned server with that id on this node",
        )
    })?;

    let mut config = s.manager.load_blueprint(&id).await.ok_or_else(|| {
        err_json(
            StatusCode::CONFLICT,
            "this server has no stored blueprint to change; it predates blueprint persistence",
        )
    })?;

    let port_variables: Vec<&str> =
        record.ports.iter().filter_map(|p| p.variable.as_deref()).collect();
    if let Some(var) = req.variables.keys().find(|v| port_variables.contains(&v.as_str())) {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            format!(
                "{} is a port allocated by this node and cannot be changed through a package change",
                var
            ),
        ));
    }

    // Re-apply everything the billing system ever set, not just this
    // request: a MEMORY the customer chose at order time must survive a
    // memory bump that would otherwise derive a new one.
    let mut variables = record.variables.clone();
    variables.extend(req.variables.iter().map(|(k, v)| (k.clone(), v.clone())));

    let overrides = Overrides {
        name: req.name.clone(),
        memory_mb: req.memory_mb,
        cpu_millicores: req.cpu_millicores,
        disk_mb: req.disk_mb,
        variables: variables.clone(),
    };
    apply_overrides(&mut config, &overrides).map_err(provision_err)?;
    config.validate().map_err(|e| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("blueprint is not valid after applying the package: {}", e),
        )
    })?;

    let was_running = s
        .manager
        .reconfigure_container(&id, &config)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    if was_running && req.restart.unwrap_or(true) {
        if let Err(e) = s.manager.start_container(&id).await {
            error!(
                "Server {} did not come back after a package change: {}",
                id, e
            );
        }
    }

    record.name = config.metadata.name.clone();
    record.resources = resources_of(&config);
    record.variables = variables;
    record.updated_at = now_epoch();
    s.provision.save(record.clone()).await.map_err(|e| {
        err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not persist provisioning record: {}", e),
        )
    })?;

    info!("Package changed for provisioned server {}", id);
    Ok(Json(describe(&s, record).await?))
}

/// Terminate a provisioned server: the container, its files and its record.
///
/// Idempotent, since a billing system retries terminations too: a server
/// that is already gone is reported as such rather than as an error.
pub(super) async fn api_provision_delete(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let had_record = s.provision.remove(&id).await.is_some();
    let existed = match s.manager.delete_container(&id, true).await {
        Ok(()) => true,
        Err(crate::error::NodeError::ContainerNotFound(_)) => false,
        Err(e) => return Err(err_json(node_err_status(&e), e.to_string())),
    };
    super::forget_server(&s, &id).await;
    if existed {
        info!("Terminated provisioned server {}", id);
    }
    Ok(Json(serde_json::json!({
        "ok": true,
        "existed": existed || had_record,
    })))
}

/// Disk used by a provisioned server, for the billing system's usage stats.
pub(super) async fn api_provision_usage(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<Json<UsageResp>> {
    let record = s.provision.get(&id).await.ok_or_else(|| {
        err_json(
            StatusCode::NOT_FOUND,
            "no provisioned server with that id on this node",
        )
    })?;
    let state = s
        .manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    let dir = std::path::Path::new(&s.data_dir).join(&id);
    let disk_used_bytes = tokio::task::spawn_blocking(move || crate::provision::dir_size(&dir))
        .await
        .unwrap_or(0);

    Ok(Json(UsageResp {
        id,
        external_id: record.external_id,
        status: format!("{:?}", state.status).to_lowercase(),
        disk_used_bytes,
        disk_limit_bytes: record.resources.disk_mb as u64 * 1024 * 1024,
        memory_limit_bytes: record.resources.memory_mb as u64 * 1024 * 1024,
    }))
}

/// Mint a one-time sign-in link for a customer to reach their server.
pub(super) async fn api_provision_sso(
    State(s): State<S>,
    Json(req): Json<SsoReq>,
) -> ApiResult<Json<SsoResp>> {
    // The token must name a server that exists; a link to nothing would
    // still mint a session.
    s.manager
        .get_state(&req.server_id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    let ttl = req
        .ttl_secs
        .map(Duration::from_secs)
        .unwrap_or(SSO_TOKEN_DEFAULT_TTL)
        .clamp(Duration::from_secs(1), SSO_TOKEN_MAX_TTL);
    let session_ttl = req.session_ttl_secs.filter(|t| *t > 0).map(Duration::from_secs);

    let token = s.sso.issue(&req.server_id, req.subject.clone(), ttl, session_ttl).await;

    info!(
        "Issued SSO token for server {} (subject {:?}, valid {}s)",
        req.server_id,
        req.subject,
        ttl.as_secs()
    );

    Ok(Json(SsoResp {
        path: format!("/sso/{}", token),
        token,
        expires_in_secs: ttl.as_secs(),
    }))
}

#[cfg(test)]
mod tests {
    use super::size_to_mb;

    #[test]
    fn sizes_convert_to_mib() {
        assert_eq!(size_to_mb("4Gi"), 4096);
        assert_eq!(size_to_mb("512Mi"), 512);
        assert_eq!(size_to_mb("1.5Gi"), 1536);
        assert_eq!(size_to_mb("1Ti"), 1_048_576);
        assert_eq!(size_to_mb("1048576"), 1);
        assert_eq!(size_to_mb("junk"), 0);
    }
}
