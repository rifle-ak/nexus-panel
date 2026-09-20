//! End-to-end tests of the panel's HTTP surface, driven through the real
//! router with the mock container runtime: provisioning, single sign-on, and
//! what a customer's scoped session can and cannot reach.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use tokio::sync::RwLock;
use tower::ServiceExt;

use super::auth::{sha256_hex, SessionScope, SessionStore, SsoTokenStore, WebAuthConfig};
use super::{build_router, scope_allows, AppState};

const API_KEY: &str = "test-node-key";

/// A blueprint with no install step, so a provisioned server is startable
/// at once and the test does not race a background install job.
const SIMPLE_BLUEPRINT: &str = r#"
metadata:
  id: simple
  name: Simple
  version: "1"
  game: simple
  author: test
container:
  image: example/simple:latest
resources:
  cpu: { min: 500, max: 1000, shares: 1024 }
  memory: { min: 512Mi, max: 1Gi }
  disk: { min: 1Gi }
startup:
  command: /bin/true
  working_dir: /home/container
variables:
  - { name: SERVER_PORT, description: p, default: "7777" }
  - { name: QUERY_PORT, description: q, default: "7778" }
networking:
  ports:
    - { name: game, internal: "{{SERVER_PORT}}", protocol: udp }
    - { name: query, internal: "{{QUERY_PORT}}", protocol: udp }
security:
  capabilities: { drop: [], add: [] }
"#;

struct Harness {
    app: Router,
    _dir: tempfile::TempDir,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let firewall = Arc::new(crate::firewall::Firewall::disabled("test"));
    let manager = Arc::new(
        crate::container::ContainerManager::new(data_dir.clone()).with_firewall(firewall.clone()),
    );
    let auth = WebAuthConfig {
        enabled: true,
        password: None,
        api_key_hashes: vec![sha256_hex(API_KEY)],
        session_ttl: Duration::from_secs(3600),
    };
    let state = AppState {
        manager,
        backup_manager: Arc::new(crate::backup::BackupManager::new(&data_dir)),
        schedule_manager: Arc::new(crate::schedule::ScheduleManager::with_data_dir(
            data_dir.clone(),
        )),
        health_checker: Arc::new(RwLock::new(crate::health::HealthChecker::new(
            "/nonexistent.sock".into(),
            data_dir.to_string_lossy().to_string(),
            0,
            0,
        ))),
        metrics: Arc::new(crate::metrics::Metrics::new().unwrap()),
        marketplace: Arc::new(nexus_marketplace::MarketplaceManager::new()),
        node_id: "test-node".into(),
        data_dir: data_dir.to_string_lossy().to_string(),
        start_time: std::time::SystemTime::now(),
        sessions: Arc::new(SessionStore::new(auth.session_ttl)),
        auth: Arc::new(auth),
        update_jobs: Arc::new(crate::update::UpdateJobStore::new()),
        mod_jobs: Arc::new(crate::mods::ModInstallJobStore::new()),
        install_jobs: Arc::new(crate::install::InstallJobStore::new()),
        updater: crate::selfupdate::SelfUpdater::new(&data_dir),
        http: reqwest::Client::new(),
        provision: Arc::new(crate::provision::ProvisionStore::new(&data_dir)),
        provision_settings: crate::provision::ProvisionSettings {
            port_range: (30000, 30099),
            public_ip: Some("203.0.113.10".into()),
        },
        sso: Arc::new(SsoTokenStore::new()),
        login_throttle: Arc::new(super::auth::LoginThrottle::new()),
        audit: None,
        firewall,
    };
    Harness {
        app: build_router(Arc::new(state)),
        _dir: dir,
    }
}

async fn send(
    app: &Router,
    req: Request<Body>,
) -> (StatusCode, serde_json::Value, header::HeaderMap) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, headers)
}

fn admin(method: Method, path: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let mut req = Request::builder().method(method).uri(path).header("x-api-key", API_KEY);
    match body {
        Some(json) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            req.body(Body::from(json.to_string())).unwrap()
        }
        None => req.body(Body::empty()).unwrap(),
    }
}

fn with_cookie(method: Method, path: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn create_body(external_id: &str) -> serde_json::Value {
    serde_json::json!({
        "external_id": external_id,
        "name": "Customer server",
        "blueprint_yaml": SIMPLE_BLUEPRINT,
        "memory_mb": 2048,
        "cpu_millicores": 1500,
        "disk_mb": 8192,
        "variables": { "MAX_PLAYERS": "16" },
        "auto_start": false,
        "owner": "client-42"
    })
}

#[tokio::test]
async fn provisioning_is_idempotent_on_external_id() {
    let h = harness();

    let (status, first, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(create_body("whmcs-1")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", first);
    let id = first["id"].as_str().unwrap().to_string();
    assert_eq!(first["external_id"], "whmcs-1");
    assert_eq!(first["ip"], "203.0.113.10");
    assert_eq!(first["primary_port"], 30000);
    assert_eq!(first["ports"][1]["port"], 30001);
    assert_eq!(first["resources"]["memory_mb"], 2048);
    assert_eq!(first["resources"]["cpu_millicores"], 1500);
    assert_eq!(first["variables"]["MAX_PLAYERS"], "16");
    assert_eq!(first["install_state"], "not_required");

    // The retry a billing system makes after a timeout gets the same server.
    let (status, again, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(create_body("whmcs-1")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["id"], id);

    // A different service gets different ports.
    let (status, second, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(create_body("whmcs-2")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(second["id"], id);
    assert_eq!(second["primary_port"], 30002);

    // Lookup by external id, and the plain list.
    let (status, found, _) = send(
        &h.app,
        admin(
            Method::GET,
            "/api/v1/provision/servers?external_id=whmcs-2",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found[0]["id"], second["id"]);
    let (_, all, _) = send(
        &h.app,
        admin(Method::GET, "/api/v1/provision/servers", None),
    )
    .await;
    assert_eq!(all.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn provisioning_rejects_bad_requests() {
    let h = harness();
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(serde_json::json!({ "external_id": "bad id", "name": "x", "blueprint": "rust" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);

    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(serde_json::json!({ "external_id": "ok", "name": "x", "blueprint": "no-such-game" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("minecraft-paper"));

    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(serde_json::json!({ "external_id": "ok", "name": "x" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Without a credential, nothing.
    let (status, _, _) = send(
        &h.app,
        Request::builder()
            .method(Method::GET)
            .uri("/api/v1/provision/servers")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn shipped_blueprints_provision_with_allocated_ports() {
    let h = harness();
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(serde_json::json!({
                "external_id": "whmcs-mc",
                "name": "Paper",
                "blueprint": "minecraft-paper",
                "memory_mb": 4096,
                "auto_start": false
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body);
    assert_eq!(body["blueprint"], "minecraft-paper");
    assert_eq!(body["game"], "minecraft");
    let ports: Vec<u64> = body["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["port"].as_u64().unwrap())
        .collect();
    assert!(
        ports.iter().all(|p| (30000..=30099).contains(p)),
        "{:?}",
        ports
    );
}

#[tokio::test]
async fn package_change_and_termination() {
    let h = harness();
    let (_, created, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(create_body("whmcs-9")),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    // Start it so the package change has to stop and restart it.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/start", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, changed, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/provision/servers/{}/package", id),
            Some(serde_json::json!({
                "memory_mb": 4096,
                "variables": { "MAX_PLAYERS": "32" }
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", changed);
    assert_eq!(changed["resources"]["memory_mb"], 4096);
    assert_eq!(changed["resources"]["cpu_millicores"], 1500, "untouched");
    assert_eq!(changed["variables"]["MAX_PLAYERS"], "32");
    assert_eq!(
        changed["primary_port"], 30000,
        "ports survive a package change"
    );
    assert_eq!(changed["status"], "running", "restarted after the change");

    // Ports are the node's to assign, not the billing system's to change.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/provision/servers/{}/package", id),
            Some(serde_json::json!({ "variables": { "SERVER_PORT": "1" } })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, usage, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/provision/servers/{}/usage", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(usage["disk_limit_bytes"], 8192u64 * 1024 * 1024);

    let (status, gone, _) = send(
        &h.app,
        admin(
            Method::DELETE,
            &format!("/api/v1/provision/servers/{}", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gone["existed"], true);

    // Terminating twice is not an error: the billing system retries these.
    let (status, gone, _) = send(
        &h.app,
        admin(
            Method::DELETE,
            &format!("/api/v1/provision/servers/{}", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(gone["existed"], false);

    let (status, _, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/provision/servers/{}", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sso_lands_a_customer_in_a_scoped_session() {
    let h = harness();
    let (_, mine, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(create_body("whmcs-mine")),
        ),
    )
    .await;
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let (_, other, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(create_body("whmcs-other")),
        ),
    )
    .await;
    let other_id = other["id"].as_str().unwrap().to_string();

    // The billing system mints a link…
    let (status, sso, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/sso",
            Some(serde_json::json!({ "server_id": mine_id, "subject": "client-42" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", sso);
    let path = sso["path"].as_str().unwrap().to_string();
    assert!(path.starts_with("/sso/"));

    // …the customer's browser follows it (over https, per the proxy)…
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&path)
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get(header::LOCATION).unwrap(),
        &format!("/#/servers/{}", mine_id)
    );
    let set_cookie = resp.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
    assert!(set_cookie.starts_with("nexus_session="));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("Secure"));
    let cookie = set_cookie.split(';').next().unwrap().to_string();

    // …and the link is dead afterwards.
    let resp = h
        .app
        .clone()
        .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // The session knows what it is.
    let (status, me, _) = send(&h.app, with_cookie(Method::GET, "/api/v1/auth/me", &cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["scope"], "servers");
    assert_eq!(me["server_ids"][0], mine_id);

    // It sees only its own server in the list…
    let (status, list, _) = send(
        &h.app,
        with_cookie(Method::GET, "/api/v1/containers", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], mine_id);

    // …can act on it…
    let (status, _, _) = send(
        &h.app,
        with_cookie(
            Method::GET,
            &format!("/api/v1/containers/{}", mine_id),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        with_cookie(
            Method::POST,
            &format!("/api/v1/containers/{}/start", mine_id),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // …but not delete it, touch another customer's server, or the node.
    let (status, _, _) = send(
        &h.app,
        with_cookie(
            Method::DELETE,
            &format!("/api/v1/containers/{}", mine_id),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &h.app,
        with_cookie(
            Method::GET,
            &format!("/api/v1/containers/{}", other_id),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &h.app,
        with_cookie(Method::GET, "/api/v1/node/info", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &h.app,
        with_cookie(Method::GET, "/api/v1/provision/servers", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Logging out kills the cookie session.
    let (status, _, headers) = send(
        &h.app,
        with_cookie(Method::POST, "/api/v1/auth/logout", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::SET_COOKIE).unwrap().to_str().unwrap().contains("Max-Age=0"));
    let (status, _, _) = send(&h.app, with_cookie(Method::GET, "/api/v1/auth/me", &cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sso_refuses_unknown_servers_and_admin_sees_admin_scope() {
    let h = harness();
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/sso",
            Some(serde_json::json!({ "server_id": "nope" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, me, _) = send(&h.app, admin(Method::GET, "/api/v1/auth/me", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["scope"], "admin");
}

#[test]
fn scope_rules() {
    let scoped = SessionScope::Servers(vec!["a".into()]);
    let allow = |m: Method, p: &str| scope_allows(&scoped, &m, p);

    assert!(allow(Method::GET, "/api/v1/auth/me"));
    assert!(allow(Method::POST, "/api/v1/auth/logout"));
    assert!(allow(Method::GET, "/api/v1/containers"));
    assert!(!allow(Method::POST, "/api/v1/containers"));
    assert!(allow(Method::GET, "/api/v1/containers/a"));
    assert!(allow(Method::POST, "/api/v1/containers/a/files/write"));
    assert!(allow(Method::POST, "/api/v1/containers/a/mods/install"));
    assert!(!allow(Method::DELETE, "/api/v1/containers/a"));
    assert!(allow(Method::DELETE, "/api/v1/containers/a/backups/b1"));
    assert!(!allow(Method::GET, "/api/v1/containers/b"));
    assert!(!allow(Method::GET, "/api/v1/containers/"));
    assert!(!allow(Method::GET, "/api/v1/containers/a-longer-id"));
    assert!(allow(Method::GET, "/api/v1/marketplace/search?q=x"));
    assert!(!allow(Method::GET, "/api/v1/node/info"));
    assert!(!allow(Method::POST, "/api/v1/node/update"));
    assert!(!allow(Method::GET, "/api/v1/blueprints/rust"));
    assert!(!allow(Method::POST, "/api/v1/provision/servers"));

    let admin = SessionScope::Admin;
    assert!(scope_allows(
        &admin,
        &Method::DELETE,
        "/api/v1/containers/a"
    ));
    assert!(scope_allows(&admin, &Method::POST, "/api/v1/node/update"));
}

#[tokio::test]
async fn shipped_blueprints_are_listed() {
    let h = harness();
    let (status, list, _) = send(&h.app, admin(Method::GET, "/api/v1/blueprints", None)).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> =
        list.as_array().unwrap().iter().map(|b| b["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), super::SHIPPED_BLUEPRINTS.len());
    assert!(ids.contains(&"minecraft-paper"));
    let mc = list.as_array().unwrap().iter().find(|b| b["id"] == "minecraft-paper").unwrap();
    assert_eq!(mc["game"], "minecraft");
    assert!(!mc["name"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn package_change_keeps_variables_and_never_reapplies_ports() {
    let h = harness();
    let mut body = create_body("whmcs-vars");
    // A billing system that thinks it can pick the port is overruled.
    body["variables"] = serde_json::json!({ "SERVER_PORT": "9999", "SERVER_NAME": "Acme" });
    let (status, created, _) = send(
        &h.app,
        admin(Method::POST, "/api/v1/provision/servers", Some(body)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", created);
    assert_eq!(created["primary_port"], 30000);
    assert!(
        created["variables"].get("SERVER_PORT").is_none(),
        "port vars are not recorded"
    );
    assert_eq!(created["variables"]["SERVER_NAME"], "Acme");
    let id = created["id"].as_str().unwrap().to_string();

    let (status, changed, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/provision/servers/{}/package", id),
            Some(serde_json::json!({ "memory_mb": 3072, "variables": { "VIEW_DISTANCE": "6" } })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", changed);
    assert_eq!(changed["primary_port"], 30000);
    assert_eq!(
        changed["variables"]["SERVER_NAME"], "Acme",
        "earlier variables survive"
    );
    assert_eq!(changed["variables"]["VIEW_DISTANCE"], "6");

    // The stored blueprint still binds the allocated port, not 9999.
    let (_, container, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/provision/servers/{}", id),
            None,
        ),
    )
    .await;
    assert_eq!(container["ports"][0]["port"], 30000);
}

#[tokio::test]
async fn login_is_throttled_after_repeated_failures() {
    let h = harness();
    let attempt = |password: &str| {
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "password": password }).to_string(),
            ))
            .unwrap()
    };
    for _ in 0..super::auth::LOGIN_MAX_FAILURES_PER_IP {
        let (status, _, _) = send(&h.app, attempt("wrong")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body, headers) = send(&h.app, attempt("wrong")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{}", body);
    assert!(headers.get(header::RETRY_AFTER).is_some());
    // Even the right credential is refused while locked out; the API key
    // path (used by the billing system) is a login too.
    let (status, _, _) = send(
        &h.app,
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "api_key": API_KEY }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn firewall_status_and_per_server_rules() {
    let h = harness();
    // Node-wide status is admin-only and honest about being disabled here.
    let (status, fw, _) = send(&h.app, admin(Method::GET, "/api/v1/firewall", None)).await;
    assert_eq!(status, StatusCode::OK, "{}", fw);
    assert_eq!(fw["enabled"], false);
    assert!(fw["disabled_reason"].is_string());
    assert!(fw["base_rules"].as_array().unwrap().len() >= 8);

    // Input is validated even with the firewall off.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/firewall/blocks",
            Some(serde_json::json!({ "cidr": "not an address" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/firewall/blocks",
            Some(
                serde_json::json!({ "cidr": "203.0.113.0/24", "ttl_secs": 600, "reason": "abuse" }),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A server's rules come from its blueprint and are editable.
    let (_, created, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(create_body("whmcs-fw")),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let (status, sfw, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/firewall", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", sfw);
    assert_eq!(sfw["rules"].as_array().unwrap().len(), 0);
    assert!(
        sfw["applied"].is_null(),
        "stopped server has nothing applied"
    );

    let (status, sfw, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/firewall", id),
            Some(serde_json::json!({ "rules": [
                { "type": "connection_rate", "name": "r", "limit": "100/s", "action": "drop" },
                { "type": "block_cidr", "name": "b", "cidr": "203.0.113.5" }
            ] })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", sfw);
    assert_eq!(sfw["rules"].as_array().unwrap().len(), 2);

    // A bad rule is refused with the reason.
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/firewall", id),
            Some(serde_json::json!({ "rules": [
                { "type": "connection_rate", "name": "r", "limit": "fast", "action": "drop" }
            ] })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);

    // One-click block appends a rule, once.
    for _ in 0..2 {
        let (status, sfw, _) = send(
            &h.app,
            admin(
                Method::POST,
                &format!("/api/v1/containers/{}/firewall/blocks", id),
                Some(serde_json::json!({ "cidr": "198.51.100.9" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", sfw);
        assert_eq!(sfw["rules"].as_array().unwrap().len(), 3);
    }

    // Rules persist in the stored blueprint and apply when it starts.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/start", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, sfw, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/firewall", id),
            None,
        ),
    )
    .await;
    assert_eq!(sfw["applied"]["rules"].as_array().unwrap().len(), 3);
    assert_eq!(sfw["applied"]["ports"][0]["port"], 30000);

    // A customer session reaches its own server's firewall, not the node's.
    let (_, sso, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/sso",
            Some(serde_json::json!({ "server_id": id })),
        ),
    )
    .await;
    let resp = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(sso["path"].as_str().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let (status, _, _) = send(
        &h.app,
        with_cookie(
            Method::GET,
            &format!("/api/v1/containers/{}/firewall", id),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        with_cookie(Method::GET, "/api/v1/firewall", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
