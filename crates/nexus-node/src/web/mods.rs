//! Installed mods on a server: what is there, whether anything is newer,
//! one-click updates and uninstalls. Installing in the first place is
//! `POST …/mods/install` next door in the router.

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use serde::{Deserialize, Serialize};

use super::{
    audit, client_ip, container_event, err_json, file_manager, node_err_status, ApiError, S,
};
use crate::audit::AuditEventType;
use crate::mods::{InstallSpec, ModInstallJob, ModRecord};
use crate::web::auth::SessionScope;

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

/// The Mods tab: every recorded mod plus the install job in flight, if any.
#[derive(Serialize)]
pub struct ModsResp {
    pub mods: Vec<ModRecord>,
    /// The current or most recent install/update job on this server.
    pub job: Option<ModInstallJob>,
}

async fn exists(s: &S, id: &str) -> Result<(), (StatusCode, Json<ApiError>)> {
    s.manager
        .get_state(id)
        .await
        .map(|_| ())
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))
}

/// `GET /api/v1/containers/:id/mods`
pub(super) async fn api_list_mods(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<ModsResp> {
    exists(&s, &id).await?;
    let mods = s
        .mods
        .list(&id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(ModsResp {
        mods,
        job: s.mod_jobs.get(&id).await,
    }))
}

/// `POST /api/v1/containers/:id/mods/check` — ask each mod's marketplace for
/// its latest version now, rather than waiting for the periodic check.
pub(super) async fn api_check_mods(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<ModsResp> {
    exists(&s, &id).await?;
    let mods = s
        .mods
        .check(&id, &s.marketplace)
        .await
        .map_err(|e| err_json(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(ModsResp {
        mods,
        job: s.mod_jobs.get(&id).await,
    }))
}

/// `POST /api/v1/containers/:id/mods/:provider/:mod_id/update` — reinstall
/// the mod at its latest version, in the directory it was installed to. Runs
/// as an install job the client polls like any other.
pub(super) async fn api_update_mod(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path((id, provider, mod_id)): Path<(String, String, String)>,
) -> ApiResult<ModInstallJob> {
    exists(&s, &id).await?;
    let record = s
        .mods
        .get(&id, &provider, &mod_id)
        .await
        .map_err(|e| err_json(StatusCode::NOT_FOUND, e.to_string()))?;

    let fm = file_manager(&s, &id);
    let target = fm
        .resolve_path(&record.target_dir)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e.to_string()))?;
    let job = s
        .mod_jobs
        .start(&id, &provider, &mod_id, &record.target_dir)
        .await
        .map_err(|e| err_json(StatusCode::CONFLICT, e))?;

    let ip = client_ip(peer.map(|c| c.0), &headers);
    audit(
        &s,
        container_event(
            AuditEventType::ConfigurationChanged,
            "mods.update",
            &scope,
            &ip,
            &id,
        )
        .with_context("mod", format!("{}/{}", provider, mod_id))
        .with_context("from", record.version.clone())
        .with_context(
            "to",
            record.available_version.clone().unwrap_or_else(|| "latest".into()),
        )
        .success(),
    )
    .await;

    // The version the check found, when there was one; otherwise whatever
    // the provider calls latest right now.
    let spec = InstallSpec {
        container_id: id,
        provider,
        mod_id,
        version: record.available_version.clone(),
        target_dir: record.target_dir.clone(),
        target,
        server_dir: fm.server_dir().to_path_buf(),
    };
    tokio::spawn(async move {
        let _ = crate::mods::run_install(
            s.mod_jobs.clone(),
            s.marketplace.clone(),
            s.mods.clone(),
            spec,
        )
        .await;
    });
    Ok(Json(job))
}

#[derive(Deserialize)]
pub struct ModSettingsReq {
    pub auto_update: bool,
}

/// `PUT /api/v1/containers/:id/mods/:provider/:mod_id` — opt a mod in or out
/// of automatic updates.
pub(super) async fn api_set_mod(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path((id, provider, mod_id)): Path<(String, String, String)>,
    Json(body): Json<ModSettingsReq>,
) -> ApiResult<ModRecord> {
    exists(&s, &id).await?;
    let record = s
        .mods
        .set_auto_update(&id, &provider, &mod_id, body.auto_update)
        .await
        .map_err(|e| err_json(StatusCode::NOT_FOUND, e.to_string()))?;
    let ip = client_ip(peer.map(|c| c.0), &headers);
    audit(
        &s,
        container_event(
            AuditEventType::ConfigurationChanged,
            "mods.auto_update",
            &scope,
            &ip,
            &id,
        )
        .with_context("mod", format!("{}/{}", provider, mod_id))
        .with_context("auto_update", body.auto_update)
        .success(),
    )
    .await;
    Ok(Json(record))
}

/// `DELETE /api/v1/containers/:id/mods/:provider/:mod_id` — remove the mod's
/// files (and any signature keys it brought) and forget it.
pub(super) async fn api_uninstall_mod(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Path((id, provider, mod_id)): Path<(String, String, String)>,
) -> ApiResult<serde_json::Value> {
    exists(&s, &id).await?;
    if s.mod_jobs
        .get(&id)
        .await
        .is_some_and(|j| j.status == crate::mods::InstallStatus::Running)
    {
        return Err(err_json(
            StatusCode::CONFLICT,
            "a mod install is running for this server; wait for it to finish",
        ));
    }
    let record = s
        .mods
        .get(&id, &provider, &mod_id)
        .await
        .map_err(|e| err_json(StatusCode::NOT_FOUND, e.to_string()))?;

    let mut paths = vec![record.path.clone()];
    paths.extend(record.signature_keys.iter().map(|k| format!("keys/{}", k)));
    let fm = file_manager(&s, &id);
    let ip = client_ip(peer.map(|c| c.0), &headers);
    let event = container_event(
        AuditEventType::FileDeleted,
        "mods.uninstall",
        &scope,
        &ip,
        &id,
    )
    .with_context("mod", format!("{}/{}", provider, mod_id))
    .with_context("path", record.path.clone());
    let removed = match fm.delete(&paths, true).await {
        Ok(n) => n,
        Err(e) => {
            audit(&s, event.failure(e.to_string())).await;
            return Err(err_json(node_err_status(&e), e.to_string()));
        }
    };
    s.mods
        .remove(&id, &provider, &mod_id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(&s, event.with_context("removed", removed).success()).await;
    Ok(Json(serde_json::json!({ "ok": true, "removed": removed })))
}
