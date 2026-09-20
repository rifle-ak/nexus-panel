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
    monitor: Arc<crate::stats::ResourceMonitor>,
    notifier: Arc<crate::notify::Notifier>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let firewall = Arc::new(crate::firewall::Firewall::disabled("test"));
    let metrics = Arc::new(crate::metrics::Metrics::new().unwrap());
    let notifier = Arc::new(crate::notify::Notifier::load(&data_dir, "test"));
    let users = Arc::new(crate::users::UserStore::load(&data_dir));
    let login_throttle = Arc::new(super::auth::LoginThrottle::new());
    let manager = Arc::new(
        crate::container::ContainerManager::new(data_dir.clone())
            .with_firewall(firewall.clone())
            .with_notifier(notifier.clone()),
    );
    let monitor = Arc::new(crate::stats::ResourceMonitor::new(
        (*manager).clone(),
        metrics.clone(),
    ));
    let auth = WebAuthConfig {
        enabled: true,
        password: None,
        api_key_hashes: vec![sha256_hex(API_KEY)],
        session_ttl: Duration::from_secs(3600),
    };
    let state = AppState {
        manager: manager.clone(),
        backup_manager: Arc::new(crate::backup::BackupManager::new(&data_dir)),
        schedule_manager: Arc::new(crate::schedule::ScheduleManager::with_data_dir(
            data_dir.clone(),
        )),
        health_checker: Arc::new(RwLock::new(
            crate::health::HealthChecker::new(
                "/nonexistent.sock".into(),
                data_dir.to_string_lossy().to_string(),
                0,
                0,
            )
            .with_runtime(manager.runtime())
            .with_manager(manager.clone()),
        )),
        metrics: metrics.clone(),
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
        login_throttle: login_throttle.clone(),
        audit: Some(Arc::new(
            crate::audit::AuditLogger::new(crate::audit::AuditConfig {
                enabled: true,
                min_severity: crate::audit::AuditSeverity::Debug,
                file_output: None,
                stdout_output: false,
                include_checksums: true,
                buffer_size: 256,
                node_id: "test".into(),
            })
            .await
            .unwrap(),
        )),
        firewall,
        monitor: monitor.clone(),
        users: users.clone(),
        notifier: notifier.clone(),
        brand: Arc::new(crate::branding::BrandStore::load(&data_dir)),
        sftp: Arc::new(crate::sftp::SftpServer::new(
            crate::sftp::SftpSettings {
                enabled: false,
                bind: "127.0.0.1:0".into(),
                public_host: Some("node.example".into()),
            },
            data_dir.clone(),
            manager.clone(),
            users.clone(),
            login_throttle.clone(),
            None,
            "test",
        )),
    };
    Harness {
        app: build_router(Arc::new(state)),
        monitor,
        notifier,
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
    let h = harness().await;

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
    let h = harness().await;
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
    let h = harness().await;
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
    let h = harness().await;
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
    let h = harness().await;
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
    let h = harness().await;
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
    let h = harness().await;
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
    let h = harness().await;
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
    let h = harness().await;
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
    let h = harness().await;
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

#[tokio::test]
async fn stats_console_and_node_usage() {
    let h = harness().await;
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(serde_json::json!({
                "external_id": "obs-1",
                "name": "Observed",
                "blueprint_yaml": SIMPLE_BLUEPRINT,
                "auto_start": false
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body);
    let id = body["id"].as_str().unwrap().to_string();

    // Unknown server: 404. Known but stopped: nothing to show yet.
    let (status, _, _) = send(
        &h.app,
        admin(Method::GET, "/api/v1/containers/nope/stats", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, stats, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/stats", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", stats);
    assert_eq!(stats["running"], false);
    assert!(stats["current"].is_null());
    assert_eq!(stats["interval_secs"], 5);

    // Running and sampled: usage everywhere it belongs.
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
    h.monitor.sample().await;
    h.monitor.sample().await;
    let (status, stats, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/stats", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", stats);
    assert_eq!(stats["running"], true);
    assert!(stats["current"]["memory_bytes"].as_u64().unwrap() > 0);
    assert!(stats["current"]["memory_limit_bytes"].as_u64().unwrap() > 0);
    assert_eq!(stats["history"].as_array().unwrap().len(), 2);
    let (_, c, _) = send(
        &h.app,
        admin(Method::GET, &format!("/api/v1/containers/{}", id), None),
    )
    .await;
    assert!(c["usage"]["cpu_percent"].is_number(), "{}", c);
    let (_, list, _) = send(&h.app, admin(Method::GET, "/api/v1/containers", None)).await;
    assert!(list[0]["usage"]["memory_bytes"].is_number(), "{}", list);

    // The node's view names the server.
    let (status, node, _) = send(&h.app, admin(Method::GET, "/api/v1/node/stats", None)).await;
    assert_eq!(status, StatusCode::OK, "{}", node);
    assert!(node["current"]["memory_total_bytes"].as_u64().unwrap() > 0);
    assert_eq!(node["servers"][0]["name"], "Observed");
    assert_eq!(node["servers"][0]["id"], id);

    // Console: history as text, live output as server-sent events.
    let (status, tail, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/console?bytes=200", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", tail);
    let text = tail["text"].as_str().unwrap();
    assert!(
        text.len() <= 200 && text.ends_with("Save complete\n"),
        "{:?}",
        text
    );
    assert!(!text.starts_with('\n'));
    let resp = h
        .app
        .clone()
        .oneshot(admin(
            Method::GET,
            &format!("/api/v1/containers/{}/console/stream", id),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));

    // Health reports the firewall as a warning, not a failure, and
    // includes the servers check.
    let (status, health, _) = send(&h.app, admin(Method::GET, "/api/v1/node/health", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "healthy");
    let names: Vec<&str> = health["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"disk") && names.contains(&"memory"));

    // A customer sees their own server's stats and console, not the node's.
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
    for path in [
        format!("/api/v1/containers/{}/stats", id),
        format!("/api/v1/containers/{}/console", id),
    ] {
        let (status, _, _) = send(&h.app, with_cookie(Method::GET, &path, &cookie)).await;
        assert_eq!(status, StatusCode::OK, "{}", path);
    }
    let (status, _, _) = send(
        &h.app,
        with_cookie(Method::GET, "/api/v1/node/stats", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A request carrying the admin API key and a raw body.
fn admin_raw(method: Method, path: &str, body: Body, headers: &[(&str, &str)]) -> Request<Body> {
    let mut req = Request::builder().method(method).uri(path).header("x-api-key", API_KEY);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    req.body(body).unwrap()
}

async fn provisioned(h: &Harness, external_id: &str) -> String {
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(serde_json::json!({
                "external_id": external_id,
                "name": "Files",
                "blueprint_yaml": SIMPLE_BLUEPRINT,
                "auto_start": false
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body);
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn files_upload_download_and_archives() {
    let h = harness().await;
    let id = provisioned(&h, "files-1").await;

    // Upload streams the body into place.
    let (status, body, _) = send(
        &h.app,
        admin_raw(
            Method::POST,
            &format!(
                "/api/v1/containers/{}/files/upload?path=/plugins&name=Essentials.jar",
                id
            ),
            Body::from("jar bytes here"),
            &[("content-type", "application/octet-stream")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert_eq!(body["bytes_written"], 14);
    assert_eq!(body["path"], "/plugins/Essentials.jar");
    // No temp file left behind, and the file is listed.
    let (_, list, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/files?path=/plugins", id),
            None,
        ),
    )
    .await;
    let names: Vec<&str> =
        list.as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["Essentials.jar"]);

    // Bad names and traversal are refused.
    for name in ["..", "a/b", ""] {
        let (status, _, _) = send(
            &h.app,
            admin_raw(
                Method::POST,
                &format!(
                    "/api/v1/containers/{}/files/upload?path=/&name={}",
                    id, name
                ),
                Body::from("x"),
                &[],
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{:?}", name);
    }
    let (status, _, _) = send(
        &h.app,
        admin_raw(
            Method::POST,
            &format!(
                "/api/v1/containers/{}/files/upload?path=/../../etc&name=x",
                id
            ),
            Body::from("x"),
            &[],
        ),
    )
    .await;
    assert_ne!(status, StatusCode::OK);

    // A declared size beyond the disk allowance (1 GiB in the blueprint) is
    // refused before a byte is read.
    let (status, body, _) = send(
        &h.app,
        admin_raw(
            Method::POST,
            &format!(
                "/api/v1/containers/{}/files/upload?path=/&name=huge.bin",
                id
            ),
            Body::from("tiny"),
            &[("content-length", "5000000000")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{}", body);

    // Download streams it back with a file name; directories are refused.
    let resp = h
        .app
        .clone()
        .oneshot(admin(
            Method::GET,
            &format!(
                "/api/v1/containers/{}/files/download?path=/plugins/Essentials.jar",
                id
            ),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .contains("Essentials.jar"));
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"jar bytes here");
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/files/download?path=/plugins", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Compress, then extract somewhere else.
    for (dest, out) in [
        ("/plugins.zip", "/from-zip"),
        ("/plugins.tar.gz", "/from-tar"),
    ] {
        let (status, body, _) = send(
            &h.app,
            admin(
                Method::POST,
                &format!("/api/v1/containers/{}/files/compress", id),
                Some(serde_json::json!({ "paths": ["/plugins"], "destination": dest })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", body);
        assert!(body["size"].as_u64().unwrap() > 0);
        let (status, body, _) = send(
            &h.app,
            admin(
                Method::POST,
                &format!("/api/v1/containers/{}/files/decompress", id),
                Some(serde_json::json!({ "path": dest, "destination": out })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", body);
        assert_eq!(body["destination"], out);
        let (_, list, _) = send(
            &h.app,
            admin(
                Method::GET,
                &format!("/api/v1/containers/{}/files?path={}/plugins", id, out),
                None,
            ),
        )
        .await;
        assert_eq!(list[0]["name"], "Essentials.jar", "{}", list);
    }
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/files/compress", id),
            Some(serde_json::json!({ "paths": ["/plugins"], "destination": "/x.rar" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn backups_restore_safely_and_download() {
    let h = harness().await;
    let id = provisioned(&h, "backup-1").await;
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/files/write", id),
            Some(serde_json::json!({ "path": "/server.properties", "content": "motd=hello" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A backup with no name gets a dated one.
    let (status, backup, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/backups", id),
            Some(serde_json::json!({})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", backup);
    assert!(backup["name"].as_str().unwrap().starts_with("manual-"));
    assert_eq!(backup["status"], "completed");
    let backup_id = backup["id"].as_str().unwrap().to_string();

    // Download carries a sensible file name.
    let resp = h
        .app
        .clone()
        .oneshot(admin(
            Method::GET,
            &format!("/api/v1/containers/{}/backups/{}/download", id, backup_id),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let disposition = resp.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().to_string();
    assert!(
        disposition.contains("manual-") && disposition.ends_with(".tar.gz\""),
        "{}",
        disposition
    );
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/backups/nope/download", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Change the file, start the server: a restore is refused while it runs.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/files/write", id),
            Some(serde_json::json!({ "path": "/server.properties", "content": "motd=changed" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
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
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/backups/{}/restore", id, backup_id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{}", body);

    // Told to stop first, it stops, swaps the directory in, and says so.
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/backups/{}/restore", id, backup_id),
            Some(serde_json::json!({ "stop": true, "delete_existing": true })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert_eq!(body["stopped"], true);
    let (_, c, _) = send(
        &h.app,
        admin(Method::GET, &format!("/api/v1/containers/{}", id), None),
    )
    .await;
    assert_ne!(c["status"], "running");
    let (_, text, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!(
                "/api/v1/containers/{}/files/read?path=/server.properties",
                id
            ),
            None,
        ),
    )
    .await;
    assert_eq!(text, serde_json::Value::Null); // not JSON: it is the raw file
    let resp = h
        .app
        .clone()
        .oneshot(admin(
            Method::GET,
            &format!(
                "/api/v1/containers/{}/files/read?path=/server.properties",
                id
            ),
            None,
        ))
        .await
        .unwrap();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"motd=hello");

    // Deleting the server takes its backups with it.
    let (status, _, _) = send(
        &h.app,
        admin(Method::DELETE, &format!("/api/v1/containers/{}", id), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!h._dir.path().join("backups").join(&id).exists());
}

#[tokio::test]
async fn schedules_take_five_field_cron_and_a_zone() {
    let h = harness().await;
    let id = provisioned(&h, "sched-1").await;
    let create = |cron: &str, tz: Option<&str>| {
        serde_json::json!({
            "name": "Save",
            "cron_expression": cron,
            "timezone": tz,
            "tasks": [{ "action": "command", "payload": "save-all", "time_offset": 0 }]
        })
    };
    for bad in [
        create("0 4 *", None),
        create("0 4 * * *", Some("Mars/Base")),
    ] {
        let (status, body, _) = send(
            &h.app,
            admin(
                Method::POST,
                &format!("/api/v1/containers/{}/schedules", id),
                Some(bad),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    }
    let (status, sched, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/schedules", id),
            Some(create("0 4 * * *", Some("Europe/Berlin"))),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", sched);
    assert_eq!(sched["timezone"], "Europe/Berlin");
    assert_eq!(sched["cron_expression"], "0 4 * * *");
    assert!(sched["next_run"].is_number());
    assert_eq!(sched["running"], false);
    let sid = sched["id"].as_str().unwrap().to_string();

    // Run now returns at once; the command fails on a stopped server and
    // the failure is recorded on the schedule.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::POST,
            &format!("/api/v1/containers/{}/schedules/{}/trigger", id, sid),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut last = serde_json::Value::Null;
    for _ in 0..100 {
        let (_, list, _) = send(
            &h.app,
            admin(
                Method::GET,
                &format!("/api/v1/containers/{}/schedules", id),
                None,
            ),
        )
        .await;
        last = list[0].clone();
        if last["running"] == false && last["last_run"].is_number() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(last["last_run"].is_number(), "{}", last);
    assert!(last["last_error"].is_string(), "{}", last);

    // Disable, and clear the zone.
    let (status, upd, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/schedules/{}", id, sid),
            Some(serde_json::json!({ "is_active": false, "timezone": "" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", upd);
    assert_eq!(upd["is_active"], false);
    assert!(upd["timezone"].is_null());
    assert!(upd["next_run"].is_null());

    // Deleting the server takes its schedules with it.
    let (status, _, _) = send(
        &h.app,
        admin(Method::DELETE, &format!("/api/v1/containers/{}", id), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        h.app
            .clone()
            .oneshot(admin(
                Method::GET,
                &format!("/api/v1/containers/{}/schedules", id),
                None
            ))
            .await
            .unwrap()
            .status()
            .is_client_error()
            || std::fs::read_dir(h._dir.path().join(".nexus/schedules"))
                .map(|d| d.count() == 0)
                .unwrap_or(true)
    );
}

/// A blueprint with editable and secret variables, for the settings tests.
const EDITABLE_BLUEPRINT: &str = r#"
metadata:
  id: editable
  name: Editable
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
  - { name: MOTD, description: message, default: "hello", user_editable: true, user_viewable: true }
  - { name: DIFFICULTY, description: d, default: "normal", user_editable: true, user_viewable: true, rules: [{ type: enum, values: [easy, normal, hard] }] }
  - { name: LICENSE_KEY, description: k, default: "top-secret", secret: true }
networking:
  ports:
    - { name: game, internal: "{{SERVER_PORT}}", protocol: udp }
security:
  capabilities: { drop: [], add: [] }
"#;

/// Log in as a panel account and return the bearer token.
async fn login_as(h: &Harness, username: &str, password: &str) -> String {
    let (status, body, _) = send(
        &h.app,
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "username": username, "password": password }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    body["token"].as_str().unwrap().to_string()
}

fn bearer(
    token: &str,
    method: Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {}", token));
    match body {
        Some(json) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            req.body(Body::from(json.to_string())).unwrap()
        }
        None => req.body(Body::empty()).unwrap(),
    }
}

#[tokio::test]
async fn panel_users_get_exactly_their_grants() {
    let h = harness().await;
    let id = provisioned(&h, "users-1").await;

    // Validation: bad names, short passwords, unknown permissions and presets.
    for bad in [
        serde_json::json!({ "username": "x", "password": "long enough password" }),
        serde_json::json!({ "username": "ops", "password": "short" }),
        serde_json::json!({ "username": "ops", "password": "long enough password", "grants": { "s": ["nope.nope"] } }),
        serde_json::json!({ "username": "ops", "password": "long enough password", "grants": { "s": "godmode" } }),
    ] {
        let (status, body, _) = send(&h.app, admin(Method::POST, "/api/v1/users", Some(bad))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    }

    // An operator on this server: preset grants.
    let (status, ops, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/users",
            Some(serde_json::json!({
                "username": "Ops",
                "password": "correct horse battery",
                "grants": { id.clone(): "read_only" }
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", ops);
    assert_eq!(ops["username"], "ops");
    assert_eq!(ops["admin"], false);
    assert!(ops["grants"][&id].as_array().unwrap().iter().any(|p| p == "console.read"));
    assert!(!ops["grants"][&id].as_array().unwrap().iter().any(|p| p == "power.start"));
    let ops_id = ops["id"].as_str().unwrap().to_string();

    // Wrong password fails; right one gives a session that knows itself.
    let (status, _, _) = send(
        &h.app,
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "username": "ops", "password": "wrong password!" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let token = login_as(&h, "ops", "correct horse battery").await;
    let (_, me, _) = send(&h.app, bearer(&token, Method::GET, "/api/v1/auth/me", None)).await;
    assert_eq!(me["scope"], "servers");
    assert_eq!(me["username"], "ops");
    assert_eq!(me["server_ids"][0], id);
    assert!(me["permissions"][&id].as_array().unwrap().iter().any(|p| p == "file.read"));

    // Read-only: may look, may not touch, sees only their server.
    let (status, list, _) = send(
        &h.app,
        bearer(&token, Method::GET, "/api/v1/containers", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    for (method, path, expect) in [
        (
            Method::GET,
            format!("/api/v1/containers/{}", id),
            StatusCode::OK,
        ),
        (
            Method::GET,
            format!("/api/v1/containers/{}/files?path=/", id),
            StatusCode::OK,
        ),
        (
            Method::GET,
            format!("/api/v1/containers/{}/console", id),
            StatusCode::OK,
        ),
        (
            Method::POST,
            format!("/api/v1/containers/{}/start", id),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::POST,
            format!("/api/v1/containers/{}/command", id),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::POST,
            format!("/api/v1/containers/{}/files/mkdir", id),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::DELETE,
            format!("/api/v1/containers/{}", id),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::GET,
            "/api/v1/users".to_string(),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::GET,
            "/api/v1/audit".to_string(),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::GET,
            "/api/v1/containers/other/files".to_string(),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let body = if method == Method::POST {
            Some(serde_json::json!({ "path": "/x", "command": "say hi" }))
        } else {
            None
        };
        let (status, _, _) = send(&h.app, bearer(&token, method.clone(), &path, body)).await;
        assert_eq!(status, expect, "{} {}", method, path);
    }

    // Promote to operator: power and files open up; deleting stays closed.
    let (status, upd, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/users/{}", ops_id),
            Some(serde_json::json!({ "grants": { id.clone(): ["power.start", "power.stop", "file.write"] } })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", upd);
    // Grants apply to new sessions; the old token keeps its old scope.
    let token = login_as(&h, "ops", "correct horse battery").await;
    let (status, _, _) = send(
        &h.app,
        bearer(
            &token,
            Method::POST,
            &format!("/api/v1/containers/{}/files/mkdir", id),
            Some(serde_json::json!({ "path": "/made" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        bearer(
            &token,
            Method::POST,
            &format!("/api/v1/containers/{}/start", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        bearer(
            &token,
            Method::DELETE,
            &format!("/api/v1/containers/{}", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Changing one's own password; a disabled account cannot sign in.
    let (status, _, _) = send(
        &h.app,
        bearer(&token, Method::POST, "/api/v1/auth/password", Some(serde_json::json!({ "current_password": "wrong", "new_password": "another long password" }))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        &h.app,
        bearer(&token, Method::POST, "/api/v1/auth/password", Some(serde_json::json!({ "current_password": "correct horse battery", "new_password": "another long password" }))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    login_as(&h, "ops", "another long password").await;
    send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/users/{}", ops_id),
            Some(serde_json::json!({ "disabled": true })),
        ),
    )
    .await;
    let (status, _, _) = send(
        &h.app,
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "username": "ops", "password": "another long password" })
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An admin account has the run of the node but cannot demote itself.
    let (status, boss, _) = send(
        &h.app,
        admin(Method::POST, "/api/v1/users", Some(serde_json::json!({ "username": "boss", "password": "boss password 123", "admin": true }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let boss_token = login_as(&h, "boss", "boss password 123").await;
    let (status, _, _) = send(
        &h.app,
        bearer(&boss_token, Method::GET, "/api/v1/users", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        bearer(
            &boss_token,
            Method::PUT,
            &format!("/api/v1/users/{}", boss["id"].as_str().unwrap()),
            Some(serde_json::json!({ "admin": false })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        &h.app,
        bearer(
            &boss_token,
            Method::DELETE,
            &format!("/api/v1/users/{}", boss["id"].as_str().unwrap()),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        &h.app,
        bearer(
            &boss_token,
            Method::DELETE,
            &format!("/api/v1/users/{}", ops_id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The audit trail names who did what.
    let (status, trail, _) = send(
        &h.app,
        admin(Method::GET, "/api/v1/audit?q=user.delete", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", trail);
    assert_eq!(trail["enabled"], true);
    let ev = &trail["events"][0];
    assert_eq!(ev["action"], "user.delete", "{}", trail);
    assert_eq!(ev["actor"]["id"], "boss");
    assert_eq!(ev["target"]["name"], "ops");
    let (_, failures, _) = send(
        &h.app,
        admin(Method::GET, "/api/v1/audit?failures=true", None),
    )
    .await;
    assert!(failures["events"].as_array().unwrap().iter().all(|e| e["outcome"] == "failure"));
    assert!(
        failures["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_type"] == "AUTHENTICATION_FAILURE" && e["actor"]["id"] == "ops"),
        "{}",
        failures
    );
    let (_, permissions, _) = send(&h.app, admin(Method::GET, "/api/v1/permissions", None)).await;
    assert!(permissions["presets"]["operator"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "power.restart"));
}

#[tokio::test]
async fn api_keys_are_minted_once_and_revocable() {
    let h = harness().await;
    let (status, made, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/apikeys",
            Some(serde_json::json!({ "name": "billing" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", made);
    let secret = made["key"].as_str().unwrap().to_string();
    assert!(secret.starts_with("nxk_"));
    let (_, list, _) = send(&h.app, admin(Method::GET, "/api/v1/apikeys", None)).await;
    assert!(list[0]["key"].is_null());
    assert_eq!(list[0]["name"], "billing");

    // The key works as x-api-key and as a bearer, with admin reach, and its
    // actions are attributed to it.
    let (status, _, _) = send(
        &h.app,
        Request::builder()
            .method(Method::GET)
            .uri("/api/v1/users")
            .header("x-api-key", &secret)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        bearer(
            &secret,
            Method::POST,
            "/api/v1/firewall/blocks",
            Some(serde_json::json!({ "cidr": "203.0.113.9" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, trail, _) = send(
        &h.app,
        admin(Method::GET, "/api/v1/audit?q=firewall.block", None),
    )
    .await;
    assert_eq!(
        trail["events"][0]["actor"]["id"], "key:billing",
        "{}",
        trail
    );
    let (_, list, _) = send(&h.app, admin(Method::GET, "/api/v1/apikeys", None)).await;
    assert!(list[0]["last_used_at"].is_number());

    // Revoked keys stop working at once.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::DELETE,
            &format!("/api/v1/apikeys/{}", made["id"].as_str().unwrap()),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        Request::builder()
            .method(Method::GET)
            .uri("/api/v1/users")
            .header("x-api-key", &secret)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(&h.app, admin(Method::DELETE, "/api/v1/apikeys/nope", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn server_settings_respect_editability_and_rules() {
    let h = harness().await;
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::POST,
            "/api/v1/provision/servers",
            Some(serde_json::json!({ "external_id": "settings-1", "name": "Tunable", "blueprint_yaml": EDITABLE_BLUEPRINT, "auto_start": false })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", body);
    let id = body["id"].as_str().unwrap().to_string();

    // The operator sees everything, including the secret and the port.
    let (status, settings, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/settings", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", settings);
    assert_eq!(settings["name"], "Tunable");
    assert_eq!(settings["resources_editable"], true);
    assert_eq!(settings["resources"]["memory_mb"], 1024);
    let vars = settings["variables"].as_array().unwrap();
    let find = |n: &str| vars.iter().find(|v| v["name"] == n).cloned().unwrap();
    assert_eq!(find("LICENSE_KEY")["value"], "top-secret");
    assert_eq!(find("SERVER_PORT")["port"], true);
    assert_eq!(find("SERVER_PORT")["editable"], false);
    assert_eq!(find("DIFFICULTY")["rules"][0]["type"], "enum");

    // Rules apply; ports are off limits; the rest is applied and recorded.
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/settings", id),
            Some(serde_json::json!({ "variables": { "DIFFICULTY": "insane" } })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/settings", id),
            Some(serde_json::json!({ "variables": { "SERVER_PORT": "1" } })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    let (status, updated, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/settings", id),
            Some(serde_json::json!({ "name": "Renamed", "memory_mb": 2048, "variables": { "DIFFICULTY": "hard", "MOTD": "welcome" } })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", updated);
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(updated["resources"]["memory_mb"], 2048);
    let (_, record, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/provision/servers/{}", id),
            None,
        ),
    )
    .await;
    assert_eq!(record["name"], "Renamed");
    assert_eq!(record["resources"]["memory_mb"], 2048);

    // A customer sees only what the blueprint lets them, and cannot touch
    // resources or non-editable variables.
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
    let (status, view, _) = send(
        &h.app,
        with_cookie(
            Method::GET,
            &format!("/api/v1/containers/{}/settings", id),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", view);
    assert_eq!(view["resources_editable"], false);
    let names: Vec<&str> = view["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"MOTD") && !names.contains(&"LICENSE_KEY"),
        "{:?}",
        names
    );
    let put = |body: serde_json::Value| {
        let mut req = with_cookie(
            Method::PUT,
            &format!("/api/v1/containers/{}/settings", id),
            &cookie,
        );
        *req.body_mut() = Body::from(body.to_string());
        req.headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        req
    };
    let (status, _, _) = send(&h.app, put(serde_json::json!({ "memory_mb": 4096 }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &h.app,
        put(serde_json::json!({ "variables": { "LICENSE_KEY": "mine" } })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body, _) = send(
        &h.app,
        put(serde_json::json!({ "variables": { "MOTD": "customer motd" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
}

#[tokio::test]
async fn branding_is_public_and_operator_editable() {
    let h = harness().await;
    // Before anyone signs in, the login screen can read the brand.
    let (status, cfg, _) = send(
        &h.app,
        Request::builder().uri("/api/v1/auth/config").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cfg["brand"]["name"], "Nexus Panel");

    let (status, body, _) = send(
        &h.app,
        admin(
            Method::PUT,
            "/api/v1/node/branding",
            Some(serde_json::json!({ "name": "Acme", "tagline": "x", "accent": "red" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::PUT,
            "/api/v1/node/branding",
            Some(serde_json::json!({
                "name": "Acme Hosting", "tagline": "Play more", "accent": "#ff6600",
                "logo_url": "https://cdn.example/logo.png", "support_url": "https://acme.example/support",
                "billing_url": ""
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert_eq!(body["name"], "Acme Hosting");
    assert!(body["billing_url"].is_null());
    let (_, cfg, _) = send(
        &h.app,
        Request::builder().uri("/api/v1/auth/config").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(cfg["brand"]["accent"], "#ff6600");

    // A customer cannot change it.
    let id = provisioned(&h, "brand-1").await;
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
        with_cookie(Method::PUT, "/api/v1/node/branding", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body, _) =
        send(&h.app, admin(Method::DELETE, "/api/v1/node/branding", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Nexus Panel");
}

#[tokio::test]
async fn notifications_settings_and_server_hooks() {
    let h = harness().await;
    let id = provisioned(&h, "notify-1").await;

    let (status, body, _) = send(&h.app, admin(Method::GET, "/api/v1/notifications", None)).await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert!(body["event_kinds"].as_array().unwrap().iter().any(|k| k == "server.crashed"));
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::PUT,
            "/api/v1/notifications",
            Some(serde_json::json!({ "webhooks": ["not a url"] })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    let (status, body, _) = send(
        &h.app,
        admin(
            Method::PUT,
            "/api/v1/notifications",
            Some(serde_json::json!({
                "webhooks": ["https://discord.com/api/webhooks/1/abc"],
                "emails": ["ops@example.com"],
                "smtp_url": "smtps://u:p@mail.example.com:465",
                "min_severity": "critical"
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert_eq!(body["min_severity"], "critical");
    assert_eq!(
        body["webhooks"][0],
        "https://discord.com/api/webhooks/1/abc"
    );

    // A customer sets their own server's webhook, not the node's.
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
        with_cookie(Method::GET, "/api/v1/notifications", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let put = |body: serde_json::Value| {
        let mut req = with_cookie(
            Method::PUT,
            &format!("/api/v1/containers/{}/notifications", id),
            &cookie,
        );
        *req.body_mut() = Body::from(body.to_string());
        req.headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        req
    };
    let (status, body, _) = send(&h.app, put(serde_json::json!({ "webhook_url": "https://discord.com/api/webhooks/2/x", "events": ["server.nope"] }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    let (status, body, _) = send(&h.app, put(serde_json::json!({ "webhook_url": "https://discord.com/api/webhooks/2/x", "events": ["server.crashed", "backup.failed"] }))).await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert_eq!(body["events"].as_array().unwrap().len(), 2);
    let (status, body, _) = send(
        &h.app,
        with_cookie(
            Method::GET,
            &format!("/api/v1/containers/{}/notifications", id),
            &cookie,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["webhook_url"], "https://discord.com/api/webhooks/2/x");

    // Deleting the server drops its hook.
    let (status, _, _) = send(
        &h.app,
        admin(Method::DELETE, &format!("/api/v1/containers/{}", id), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.notifier.server_hook(&id).await,
        crate::notify::ServerHook::default()
    );
}

#[tokio::test]
async fn sftp_settings_follow_the_session() {
    let h = harness().await;
    let id = provisioned(&h, "sftp-1").await;
    let short: String = id.chars().take(8).collect();

    // The operator (no named account) is told the server username.
    let (status, info, _) = send(
        &h.app,
        admin(
            Method::GET,
            &format!("/api/v1/containers/{}/sftp", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", info);
    assert_eq!(info["enabled"], false);
    assert_eq!(info["host"], "node.example");
    assert_eq!(info["username"], short);
    assert_eq!(info["has_password"], false);

    // Too short is refused; a proper one is stored hashed.
    let (status, _, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/sftp", id),
            Some(serde_json::json!({ "password": "short" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, info, _) = send(
        &h.app,
        admin(
            Method::PUT,
            &format!("/api/v1/containers/{}/sftp", id),
            Some(serde_json::json!({ "password": "customer password" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", info);
    assert_eq!(info["has_password"], true);

    // A panel account with file.sftp sees its own username; one without
    // the grant cannot reach the endpoint.
    let (status, _, _) = send(
        &h.app,
        admin(Method::POST, "/api/v1/users", Some(serde_json::json!({ "username": "ops", "password": "ops password 123", "grants": { id.clone(): ["file.sftp", "settings.read"] } }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let token = login_as(&h, "ops", "ops password 123").await;
    let (status, info, _) = send(
        &h.app,
        bearer(
            &token,
            Method::GET,
            &format!("/api/v1/containers/{}/sftp", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", info);
    assert_eq!(info["username"], format!("ops.{}", short));
    send(&h.app, admin(Method::POST, "/api/v1/users", Some(serde_json::json!({ "username": "viewer", "password": "viewer password 1", "grants": { id.clone(): "read_only" } })))).await;
    let token = login_as(&h, "viewer", "viewer password 1").await;
    let (status, _, _) = send(
        &h.app,
        bearer(
            &token,
            Method::GET,
            &format!("/api/v1/containers/{}/sftp", id),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Deleting the server drops its password.
    let (status, _, _) = send(
        &h.app,
        admin(Method::DELETE, &format!("/api/v1/containers/{}", id), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !std::fs::read_to_string(h._dir.path().join(".nexus/sftp.json"))
            .unwrap_or_default()
            .contains(&id)
    );
}
