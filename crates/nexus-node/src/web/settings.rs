//! A server's editable settings: its name, its blueprint variables, and,
//! for the operator, its resources.
//!
//! Changes go through the same override path as a billing package change,
//! so the blueprint's validation rules apply and the container is rebuilt
//! with the new values. Ports are the node's to assign and cannot be
//! changed here.

use std::collections::BTreeMap;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use tracing::error;

use super::{audit, client_ip, container_event, err_json, node_err_status, ApiError, S};
use crate::provision::{apply_overrides, resolve_ports, Overrides};
use crate::web::auth::SessionScope;

type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

#[derive(Serialize)]
pub struct VariableJson {
    pub name: String,
    pub description: String,
    /// The current value, or `null` when it is a secret this session may
    /// not see.
    pub value: Option<String>,
    /// Whether this session may change it.
    pub editable: bool,
    pub secret: bool,
    pub required: bool,
    pub placeholder: Option<String>,
    pub category: Option<String>,
    pub rules: Vec<serde_json::Value>,
    /// Set for a port the node assigned; it cannot be edited.
    pub port: bool,
}

#[derive(Serialize)]
pub struct ResourcesJson {
    pub memory_mb: u32,
    pub cpu_millicores: u32,
    pub disk_mb: u32,
}

#[derive(Serialize)]
pub struct SettingsResp {
    pub name: String,
    pub blueprint: String,
    pub game: String,
    pub version: String,
    pub image: String,
    pub variables: Vec<VariableJson>,
    pub resources: ResourcesJson,
    /// Whether this session may change the resources.
    pub resources_editable: bool,
    pub ports: Vec<crate::provision::AssignedPort>,
}

fn size_to_mb(s: &str) -> u32 {
    crate::container::manager::parse_size(s)
        .map(|b| (b / (1024 * 1024)) as u32)
        .unwrap_or(0)
}

async fn describe(s: &S, id: &str, scope: &SessionScope) -> ApiResult<SettingsResp> {
    let config = s
        .manager
        .load_blueprint(id)
        .await
        .ok_or_else(|| err_json(StatusCode::CONFLICT, "this server has no stored blueprint"))?;
    let ports = resolve_ports(&config);
    let port_vars: Vec<&str> = ports.iter().filter_map(|p| p.variable.as_deref()).collect();
    let admin = scope.is_admin();
    let variables = config
        .variables
        .iter()
        .filter(|v| admin || v.user_viewable || v.user_editable)
        .map(|v| {
            let is_port = port_vars.contains(&v.name.as_str());
            let editable = !is_port && (admin || v.user_editable);
            VariableJson {
                name: v.name.clone(),
                description: v.description.clone(),
                value: (admin || !v.secret || v.user_editable).then(|| v.default.clone()),
                editable,
                secret: v.secret,
                required: v.required,
                placeholder: v.placeholder.clone(),
                category: v.category.clone(),
                rules: v
                    .rules
                    .as_ref()
                    .map(|rs| rs.iter().filter_map(|r| serde_json::to_value(r).ok()).collect())
                    .unwrap_or_default(),
                port: is_port,
            }
        })
        .collect();
    Ok(SettingsResp {
        name: config.metadata.name.clone(),
        blueprint: config.metadata.id.clone(),
        game: config.metadata.game.clone(),
        version: config.metadata.version.clone(),
        image: config.container.image.clone(),
        variables,
        resources: ResourcesJson {
            memory_mb: size_to_mb(&config.resources.memory.max),
            cpu_millicores: config.resources.cpu.max,
            disk_mb: size_to_mb(&config.resources.disk.min),
        },
        resources_editable: admin,
        ports,
    })
}

pub(super) async fn api_get_settings(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    Path(id): Path<String>,
) -> ApiResult<Json<SettingsResp>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(describe(&s, &id, &scope).await?))
}

#[derive(Deserialize)]
pub struct UpdateSettingsReq {
    pub name: Option<String>,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    pub memory_mb: Option<u32>,
    pub cpu_millicores: Option<u32>,
    pub disk_mb: Option<u32>,
    /// Start the server again afterwards if it was running. Default true.
    pub restart: Option<bool>,
}

/// Apply new settings and rebuild the container so they take effect. The
/// server is stopped for the rebuild and started again if it was running.
pub(super) async fn api_update_settings(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<UpdateSettingsReq>,
) -> ApiResult<Json<SettingsResp>> {
    s.manager
        .get_state(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let mut config = s
        .manager
        .load_blueprint(&id)
        .await
        .ok_or_else(|| err_json(StatusCode::CONFLICT, "this server has no stored blueprint"))?;

    let admin = scope.is_admin();
    if !admin && (req.memory_mb.is_some() || req.cpu_millicores.is_some() || req.disk_mb.is_some())
    {
        return Err(err_json(
            StatusCode::FORBIDDEN,
            "resources are set by the operator or the billing system",
        ));
    }
    let ports = resolve_ports(&config);
    for name in req.variables.keys() {
        if ports.iter().any(|p| p.variable.as_deref() == Some(name)) {
            return Err(err_json(
                StatusCode::BAD_REQUEST,
                format!(
                    "{} is a port assigned by this node and cannot be changed",
                    name
                ),
            ));
        }
        let var = config.variables.iter().find(|v| &v.name == name);
        match var {
            Some(v) if admin || v.user_editable => {}
            Some(_) => {
                return Err(err_json(
                    StatusCode::FORBIDDEN,
                    format!("{} is not editable on this server", name),
                ))
            }
            None if admin => {}
            None => {
                return Err(err_json(
                    StatusCode::BAD_REQUEST,
                    format!("{} is not a variable of this server", name),
                ))
            }
        }
    }

    let overrides = Overrides {
        name: req.name.clone(),
        memory_mb: req.memory_mb,
        cpu_millicores: req.cpu_millicores,
        disk_mb: req.disk_mb,
        variables: req.variables.clone(),
    };
    apply_overrides(&mut config, &overrides)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e.to_string()))?;
    config.validate().map_err(|e| {
        err_json(
            StatusCode::BAD_REQUEST,
            format!("blueprint is not valid with these settings: {}", e),
        )
    })?;

    let ip = client_ip(peer.map(|c| c.0), &headers);
    let result = s.manager.reconfigure_container(&id, &config).await;
    let event = container_event(
        crate::audit::AuditEventType::ConfigurationChanged,
        "settings.update",
        &scope,
        &ip,
        &id,
    )
    .with_context("variables", req.variables.keys().collect::<Vec<_>>())
    .with_context("name", &req.name)
    .with_context("memory_mb", req.memory_mb)
    .with_context("cpu_millicores", req.cpu_millicores)
    .with_context("disk_mb", req.disk_mb);
    match &result {
        Ok(_) => audit(&s, event.success()).await,
        Err(e) => audit(&s, event.failure(e.to_string())).await,
    }
    let was_running = result.map_err(|e| err_json(node_err_status(&e), e.to_string()))?;

    if was_running && req.restart.unwrap_or(true) {
        if let Err(e) = s.manager.start_container(&id).await {
            error!(
                "Server {} did not come back after a settings change: {}",
                id, e
            );
        }
    }

    // Keep the billing record in step, so a later package change re-applies
    // what was set here rather than the values from order time.
    if let Some(mut record) = s.provision.get(&id).await {
        record.name = config.metadata.name.clone();
        record
            .variables
            .extend(req.variables.iter().map(|(k, v)| (k.clone(), v.clone())));
        record.resources = crate::provision::ProvisionResources {
            memory_mb: size_to_mb(&config.resources.memory.max),
            cpu_millicores: config.resources.cpu.max,
            disk_mb: size_to_mb(&config.resources.disk.min),
        };
        record.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Err(e) = s.provision.save(record).await {
            error!("Provisioning record for {} was not updated: {}", id, e);
        }
    }

    Ok(Json(describe(&s, &id, &scope).await?))
}
