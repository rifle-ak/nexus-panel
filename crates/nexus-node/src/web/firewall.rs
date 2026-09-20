//! Firewall endpoints: node-wide status and blocklist for the operator, and
//! each server's own rules for whoever owns it.

use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::{audit, container_event, err_json, node_err_status, ApiError, S};
use crate::firewall::{FirewallStatus, ServerStatus};
use crate::web::auth::SessionScope;

type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

#[derive(Deserialize)]
pub struct BlockReq {
    pub cidr: String,
    /// Seconds until the block lifts; omitted means permanent.
    pub ttl_secs: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
pub struct CidrReq {
    pub cidr: String,
}

#[derive(Deserialize)]
pub struct RulesReq {
    pub rules: Vec<nexus_config::FirewallRule>,
}

#[derive(Deserialize)]
pub struct ServerBlockReq {
    pub cidr: String,
    pub name: Option<String>,
}

/// One server's firewall as its owner sees it.
#[derive(Serialize)]
pub struct ServerFirewallResp {
    /// Whether the node's firewall is on at all.
    pub enabled: bool,
    /// The rules in the server's blueprint (what will apply on next start).
    pub rules: Vec<nexus_config::FirewallRule>,
    /// What is applied right now, with counters; `None` while stopped.
    pub applied: Option<ServerStatus>,
}

/// Node-wide status: protections, counters, blocklist, servers.
pub(super) async fn api_firewall_status(State(s): State<S>) -> Json<FirewallStatus> {
    Json(s.firewall.status().await)
}

pub(super) async fn api_firewall_block(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    Json(req): Json<BlockReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let ttl = req.ttl_secs.filter(|t| *t > 0).map(Duration::from_secs);
    s.firewall
        .block(&req.cidr, ttl, req.reason.as_deref())
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    info!("Blocked {} ({:?})", req.cidr, req.reason);
    audit(
        &s,
        crate::audit::AuditEvent::new(
            crate::audit::AuditEventType::SecurityPolicyViolation,
            "firewall.block",
        )
        .with_actor(super::actor_for(&scope))
        .with_context("cidr", &req.cidr)
        .with_context("ttl_secs", req.ttl_secs)
        .with_context("reason", &req.reason)
        .success(),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn api_firewall_unblock(
    State(s): State<S>,
    Json(req): Json<CidrReq>,
) -> ApiResult<Json<serde_json::Value>> {
    s.firewall
        .unblock(&req.cidr)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn api_firewall_trust(
    State(s): State<S>,
    Json(req): Json<CidrReq>,
) -> ApiResult<Json<serde_json::Value>> {
    s.firewall
        .trust(&req.cidr)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn api_firewall_untrust(
    State(s): State<S>,
    Json(req): Json<CidrReq>,
) -> ApiResult<Json<serde_json::Value>> {
    s.firewall
        .untrust(&req.cidr)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn server_firewall(s: &S, id: &str) -> ApiResult<ServerFirewallResp> {
    s.manager
        .get_state(id)
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    let rules = s
        .manager
        .load_blueprint(id)
        .await
        .map(|c| c.security.firewall_rules)
        .unwrap_or_default();
    Ok(ServerFirewallResp {
        enabled: s.firewall.is_enabled(),
        rules,
        applied: s.firewall.server_status(id).await,
    })
}

/// A server's firewall rules and live counters.
pub(super) async fn api_server_firewall(
    State(s): State<S>,
    Path(id): Path<String>,
) -> ApiResult<Json<ServerFirewallResp>> {
    Ok(Json(server_firewall(&s, &id).await?))
}

/// Replace a server's rules. They are stored in its blueprint and, if the
/// server is running, applied at once.
pub(super) async fn api_set_server_firewall(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    Path(id): Path<String>,
    Json(req): Json<RulesReq>,
) -> ApiResult<Json<ServerFirewallResp>> {
    if req.rules.len() > 200 {
        return Err(err_json(
            StatusCode::BAD_REQUEST,
            "at most 200 firewall rules per server",
        ));
    }
    s.manager
        .set_firewall_rules(&id, req.rules.clone())
        .await
        .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    audit(
        &s,
        container_event(
            crate::audit::AuditEventType::ConfigurationChanged,
            "firewall.rules",
            &scope,
            "",
            &id,
        )
        .with_context("rule_count", req.rules.len())
        .success(),
    )
    .await;
    Ok(Json(server_firewall(&s, &id).await?))
}

/// Add a blocked address to a server's rules: the one-click "ban this IP".
pub(super) async fn api_server_firewall_block(
    State(s): State<S>,
    Extension(scope): Extension<SessionScope>,
    Path(id): Path<String>,
    Json(req): Json<ServerBlockReq>,
) -> ApiResult<Json<ServerFirewallResp>> {
    let cidr = crate::firewall::validate_cidr(&req.cidr)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e))?;
    let mut rules = s
        .manager
        .load_blueprint(&id)
        .await
        .map(|c| c.security.firewall_rules)
        .unwrap_or_default();
    let already = rules.iter().any(|r| {
        matches!(r, nexus_config::FirewallRule::BlockCidr { cidr: c, .. } if crate::firewall::validate_cidr(c).ok().as_deref() == Some(cidr.as_str()))
    });
    if !already {
        rules.push(nexus_config::FirewallRule::BlockCidr {
            name: req.name.unwrap_or_else(|| format!("block {}", cidr)),
            cidr: cidr.clone(),
        });
        s.manager
            .set_firewall_rules(&id, rules)
            .await
            .map_err(|e| err_json(node_err_status(&e), e.to_string()))?;
    }
    audit(
        &s,
        container_event(
            crate::audit::AuditEventType::SecurityPolicyViolation,
            "firewall.server_block",
            &scope,
            "",
            &id,
        )
        .with_context("cidr", &cidr)
        .success(),
    )
    .await;
    Ok(Json(server_firewall(&s, &id).await?))
}
