//! Telling people what happened: webhooks and email for the operator, and a
//! webhook per server for its owner.
//!
//! Events come from the parts of the node that notice things (a crash, a
//! server stopped for being over its disk allowance, a failed backup, a
//! health change) and go out to whatever is configured: Discord and Slack
//! webhooks get a formatted message, any other URL gets JSON, and email goes
//! through SMTP. A server's owner can add their own webhook on the server's
//! Settings tab and pick which of its events they want.
//!
//! Delivery is fire and forget with a short timeout; a channel that fails
//! keeps its last error for the Settings page to show. Repeats of the same
//! event on the same server are held back for a cooldown, so a server that
//! stays over its allowance does not page anyone every minute.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::error::{NodeError, Result};

const SETTINGS_FILE: &str = ".nexus/notify.json";
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// The same event on the same server is not re-sent within this window.
const COOLDOWN: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    #[default]
    Warning,
    Critical,
}

/// The kinds of event that go out. Their names are what a server's owner
/// picks from and what a generic webhook receives as `kind`.
pub const EVENT_KINDS: &[&str] = &[
    "server.started",
    "server.stopped",
    "server.crashed",
    "server.crash_loop",
    "server.disk_exceeded",
    "backup.completed",
    "backup.failed",
    "schedule.failed",
    "mod.update_available",
    "mod.updated",
    "mod.update_failed",
    "node.health",
];

/// Something worth telling someone about.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub kind: &'static str,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    /// The server it concerns, if any: `(id, name)`.
    pub server: Option<(String, String)>,
}

impl Notification {
    pub fn new(
        kind: &'static str,
        severity: Severity,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            severity,
            title: title.into(),
            body: body.into(),
            server: None,
        }
    }

    pub fn for_server(mut self, id: &str, name: &str) -> Self {
        self.server = Some((id.to_string(), name.to_string()));
        self
    }
}

/// Where the operator's notifications go.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorChannels {
    /// Webhook URLs: Discord, Slack, or anything that takes JSON.
    #[serde(default)]
    pub webhooks: Vec<String>,
    /// Recipients for email; needs `smtp_url`.
    #[serde(default)]
    pub emails: Vec<String>,
    /// `smtps://user:pass@host:465` or `smtp://user:pass@host:587?tls=required`.
    #[serde(default)]
    pub smtp_url: Option<String>,
    #[serde(default)]
    pub smtp_from: Option<String>,
    /// Lowest severity that goes to the operator's channels.
    #[serde(default = "default_min_severity")]
    pub min_severity: Severity,
}

fn default_min_severity() -> Severity {
    Severity::Warning
}

/// A server owner's own webhook.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerHook {
    pub webhook_url: Option<String>,
    /// Event kinds to send; empty means everything about the server.
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    operator: OperatorChannels,
    #[serde(default)]
    servers: HashMap<String, ServerHook>,
}

/// The last delivery outcome per channel, for the Settings page.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ChannelStatus {
    pub last_sent_at: Option<i64>,
    pub last_error: Option<String>,
}

pub struct Notifier {
    path: PathBuf,
    settings: RwLock<SettingsFile>,
    status: RwLock<HashMap<String, ChannelStatus>>,
    recent: RwLock<HashMap<String, Instant>>,
    http: reqwest::Client,
    node_id: String,
    /// The panel's public URL, for links in messages.
    panel_url: Option<String>,
}

impl Notifier {
    /// Load persisted settings under `data_dir`; the environment supplies
    /// defaults for anything not set there.
    pub fn load(data_dir: &Path, node_id: &str) -> Self {
        let path = data_dir.join(SETTINGS_FILE);
        let mut settings: SettingsFile = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let from_env = OperatorChannels::from_env();
        if settings.operator.webhooks.is_empty() {
            settings.operator.webhooks = from_env.webhooks;
        }
        if settings.operator.emails.is_empty() {
            settings.operator.emails = from_env.emails;
        }
        if settings.operator.smtp_url.is_none() {
            settings.operator.smtp_url = from_env.smtp_url;
        }
        if settings.operator.smtp_from.is_none() {
            settings.operator.smtp_from = from_env.smtp_from;
        }
        Self {
            path,
            settings: RwLock::new(settings),
            status: RwLock::new(HashMap::new()),
            recent: RwLock::new(HashMap::new()),
            http: reqwest::Client::builder().timeout(DELIVERY_TIMEOUT).build().unwrap_or_default(),
            node_id: node_id.to_string(),
            panel_url: std::env::var("NEXUS_PANEL_URL")
                .ok()
                .map(|u| u.trim_end_matches('/').to_string())
                .filter(|u| !u.is_empty()),
        }
    }

    async fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&*self.settings.read().await)
            .map_err(|e| NodeError::Internal(format!("serialize notify settings: {}", e)))?;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await;
        }
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    pub async fn operator(&self) -> OperatorChannels {
        self.settings.read().await.operator.clone()
    }

    pub async fn set_operator(&self, channels: OperatorChannels) -> Result<()> {
        for url in &channels.webhooks {
            validate_webhook(url)?;
        }
        for email in &channels.emails {
            if !email.contains('@') || email.contains(char::is_whitespace) {
                return Err(NodeError::InvalidInput(format!(
                    "{:?} is not an email address",
                    email
                )));
            }
        }
        if let Some(url) = channels.smtp_url.as_deref().filter(|u| !u.is_empty()) {
            if !url.starts_with("smtp://") && !url.starts_with("smtps://") {
                return Err(NodeError::InvalidInput(
                    "smtp_url must start with smtp:// or smtps://".to_string(),
                ));
            }
        }
        self.settings.write().await.operator = channels;
        self.save().await
    }

    pub async fn server_hook(&self, server_id: &str) -> ServerHook {
        self.settings.read().await.servers.get(server_id).cloned().unwrap_or_default()
    }

    pub async fn set_server_hook(&self, server_id: &str, hook: ServerHook) -> Result<()> {
        if let Some(url) = hook.webhook_url.as_deref().filter(|u| !u.is_empty()) {
            validate_webhook(url)?;
        }
        for e in &hook.events {
            if !EVENT_KINDS.contains(&e.as_str()) {
                return Err(NodeError::InvalidInput(format!("unknown event {:?}", e)));
            }
        }
        let mut settings = self.settings.write().await;
        if hook.webhook_url.as_deref().map(str::is_empty).unwrap_or(true) {
            settings.servers.remove(server_id);
        } else {
            settings.servers.insert(server_id.to_string(), hook);
        }
        drop(settings);
        self.save().await
    }

    /// Drop a deleted server's hook.
    pub async fn forget_server(&self, server_id: &str) {
        if self.settings.write().await.servers.remove(server_id).is_some() {
            let _ = self.save().await;
        }
    }

    pub async fn status(&self) -> HashMap<String, ChannelStatus> {
        self.status.read().await.clone()
    }

    /// Send a notification everywhere it belongs. Returns at once; delivery
    /// happens on its own task.
    pub fn notify(self: &Arc<Self>, n: Notification) {
        let this = self.clone();
        tokio::spawn(async move {
            this.dispatch(n, false).await;
        });
    }

    /// Send to the operator's channels now and report each outcome. Used
    /// by the "Send a test" button.
    pub async fn test(self: &Arc<Self>) -> HashMap<String, ChannelStatus> {
        let n = Notification::new(
            "node.health",
            Severity::Info,
            format!("Test from {}", self.node_id),
            "Notifications are set up. This is a test message from the panel.",
        );
        self.dispatch(n, true).await;
        self.status().await
    }

    async fn dispatch(&self, n: Notification, force: bool) {
        if !force && !self.passes_cooldown(&n).await {
            return;
        }
        let (operator, hook) = {
            let s = self.settings.read().await;
            let hook = n.server.as_ref().and_then(|(id, _)| s.servers.get(id).cloned());
            (s.operator.clone(), hook)
        };

        let mut targets: Vec<(String, Target)> = Vec::new();
        if force || n.severity >= operator.min_severity {
            for url in &operator.webhooks {
                targets.push((url.clone(), Target::Webhook(url.clone())));
            }
            if let (Some(smtp), false) = (&operator.smtp_url, operator.emails.is_empty()) {
                targets.push((
                    format!("email:{}", operator.emails.join(",")),
                    Target::Email {
                        smtp_url: smtp.clone(),
                        from: operator.smtp_from.clone(),
                        to: operator.emails.clone(),
                    },
                ));
            }
        }
        if let Some(hook) = hook {
            if let Some(url) = hook.webhook_url.filter(|u| !u.is_empty()) {
                if hook.events.is_empty() || hook.events.iter().any(|e| e == n.kind) {
                    targets.push((format!("server:{}", url), Target::Webhook(url)));
                }
            }
        }
        if targets.is_empty() {
            return;
        }

        for (key, target) in targets {
            let result = match target {
                Target::Webhook(url) => self.send_webhook(&url, &n).await,
                Target::Email { smtp_url, from, to } => {
                    send_email(&smtp_url, from.as_deref(), &to, &n, &self.node_id).await
                }
            };
            let mut status = self.status.write().await;
            let entry = status.entry(key.clone()).or_default();
            match result {
                Ok(()) => {
                    entry.last_sent_at = Some(chrono::Utc::now().timestamp());
                    entry.last_error = None;
                }
                Err(e) => {
                    warn!("Notification to {} failed: {}", redact(&key), e);
                    entry.last_error = Some(e);
                }
            }
        }
    }

    async fn passes_cooldown(&self, n: &Notification) -> bool {
        // Starts, stops and completed backups are single events, not a
        // condition that persists; they always go through.
        if matches!(
            n.kind,
            "server.started" | "server.stopped" | "backup.completed"
        ) {
            return true;
        }
        let key = format!(
            "{}:{}",
            n.kind,
            n.server.as_ref().map(|s| s.0.as_str()).unwrap_or("node")
        );
        let mut recent = self.recent.write().await;
        let now = Instant::now();
        recent.retain(|_, t| now.duration_since(*t) < COOLDOWN);
        if recent.contains_key(&key) {
            return false;
        }
        recent.insert(key, now);
        true
    }

    async fn send_webhook(&self, url: &str, n: &Notification) -> std::result::Result<(), String> {
        let payload = webhook_payload(url, n, &self.node_id, self.panel_url.as_deref());
        let resp = self.http.post(url).json(&payload).send().await.map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("webhook answered {}", resp.status()))
        }
    }
}

enum Target {
    Webhook(String),
    Email {
        smtp_url: String,
        from: Option<String>,
        to: Vec<String>,
    },
}

impl OperatorChannels {
    /// `NEXUS_WEBHOOKS` (comma-separated), `NEXUS_ALERT_EMAILS`,
    /// `NEXUS_SMTP_URL`, `NEXUS_SMTP_FROM`.
    pub fn from_env() -> Self {
        let list = |name: &str| -> Vec<String> {
            std::env::var(name)
                .ok()
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            webhooks: list("NEXUS_WEBHOOKS"),
            emails: list("NEXUS_ALERT_EMAILS"),
            smtp_url: std::env::var("NEXUS_SMTP_URL").ok().filter(|s| !s.is_empty()),
            smtp_from: std::env::var("NEXUS_SMTP_FROM").ok().filter(|s| !s.is_empty()),
            min_severity: Severity::Warning,
        }
    }
}

pub fn validate_webhook(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| NodeError::InvalidInput(format!("{:?} is not a URL", url)))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err(NodeError::InvalidInput(
            "webhook URLs must be http(s)".to_string(),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(NodeError::InvalidInput(
            "webhook URL has no host".to_string(),
        ));
    }
    Ok(())
}

/// A webhook URL with its secret path hidden, for logs.
fn redact(key: &str) -> String {
    match key.find("://") {
        Some(i) => {
            let rest = &key[i + 3..];
            let host = rest.split('/').next().unwrap_or("");
            format!("{}://{}/…", &key[..i], host)
        }
        None => key.to_string(),
    }
}

/// The JSON a webhook receives, shaped for the service it belongs to.
pub fn webhook_payload(
    url: &str,
    n: &Notification,
    node_id: &str,
    panel_url: Option<&str>,
) -> serde_json::Value {
    let server_line = n.server.as_ref().map(|(id, name)| match panel_url {
        Some(base) => format!("{} ({}/#/servers/{})", name, base, id),
        None => format!("{} ({})", name, &id[..id.len().min(12)]),
    });
    let color = match n.severity {
        Severity::Info => 0x34d399,
        Severity::Warning => 0xfbbf24,
        Severity::Critical => 0xfb7185,
    };
    if url.contains("discord.com/api/webhooks") || url.contains("discordapp.com/api/webhooks") {
        let mut fields =
            vec![serde_json::json!({ "name": "Node", "value": node_id, "inline": true })];
        if let Some(s) = &server_line {
            fields.push(serde_json::json!({ "name": "Server", "value": s, "inline": true }));
        }
        serde_json::json!({
            "username": "Nexus Panel",
            "embeds": [{
                "title": n.title,
                "description": n.body,
                "color": color,
                "fields": fields,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }]
        })
    } else if url.contains("hooks.slack.com") {
        let mut text = format!("*{}*\n{}", n.title, n.body);
        if let Some(s) = &server_line {
            text.push_str(&format!("\nServer: {}", s));
        }
        text.push_str(&format!("\nNode: {}", node_id));
        serde_json::json!({ "text": text })
    } else {
        serde_json::json!({
            "kind": n.kind,
            "severity": n.severity,
            "title": n.title,
            "body": n.body,
            "server": n.server.as_ref().map(|(id, name)| serde_json::json!({ "id": id, "name": name })),
            "node": node_id,
            "panel_url": panel_url,
            "at": chrono::Utc::now().to_rfc3339(),
        })
    }
}

async fn send_email(
    smtp_url: &str,
    from: Option<&str>,
    to: &[String],
    n: &Notification,
    node_id: &str,
) -> std::result::Result<(), String> {
    use lettre::message::header::ContentType;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    let from = from
        .map(str::to_string)
        .unwrap_or_else(|| format!("Nexus Panel <nexus@{}>", node_id));
    let mut builder = Message::builder()
        .from(from.parse().map_err(|e| format!("bad from address: {}", e))?)
        .subject(format!("[{}] {}", node_id, n.title))
        .header(ContentType::TEXT_PLAIN);
    for addr in to {
        builder = builder.to(addr.parse().map_err(|e| format!("bad recipient {}: {}", addr, e))?);
    }
    let mut body = n.body.clone();
    if let Some((id, name)) = &n.server {
        body.push_str(&format!("\n\nServer: {} ({})", name, id));
    }
    body.push_str(&format!("\nNode: {}\nSeverity: {:?}", node_id, n.severity));
    let message = builder.body(body).map_err(|e| e.to_string())?;

    let transport: AsyncSmtpTransport<Tokio1Executor> =
        AsyncSmtpTransport::<Tokio1Executor>::from_url(smtp_url)
            .map_err(|e| format!("smtp url: {}", e))?
            .build();
    tokio::time::timeout(Duration::from_secs(20), transport.send(message))
        .await
        .map_err(|_| "smtp timed out".to_string())?
        .map_err(|e| e.to_string())?;
    info!("Sent alert email: {}", n.title);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_fit_their_service() {
        let n = Notification::new("server.crashed", Severity::Warning, "Crashed", "exit 137")
            .for_server("abcdef123456789", "Survival");
        let d = webhook_payload(
            "https://discord.com/api/webhooks/1/x",
            &n,
            "node-1",
            Some("https://panel.example"),
        );
        assert_eq!(d["embeds"][0]["title"], "Crashed");
        assert!(d["embeds"][0]["fields"][1]["value"]
            .as_str()
            .unwrap()
            .contains("https://panel.example/#/servers/abcdef123456789"));
        let s = webhook_payload("https://hooks.slack.com/services/x", &n, "node-1", None);
        assert!(s["text"].as_str().unwrap().contains("*Crashed*"));
        let g = webhook_payload("https://example.com/hook", &n, "node-1", None);
        assert_eq!(g["kind"], "server.crashed");
        assert_eq!(g["severity"], "warning");
        assert_eq!(g["server"]["name"], "Survival");
        assert_eq!(
            redact("https://discord.com/api/webhooks/123/secret"),
            "https://discord.com/…"
        );
    }

    #[tokio::test]
    async fn settings_persist_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        let n = Arc::new(Notifier::load(dir.path(), "node-1"));
        assert!(n
            .set_operator(OperatorChannels {
                webhooks: vec!["ftp://nope".into()],
                ..Default::default()
            })
            .await
            .is_err());
        assert!(n
            .set_operator(OperatorChannels {
                emails: vec!["not an email".into()],
                ..Default::default()
            })
            .await
            .is_err());
        n.set_operator(OperatorChannels {
            webhooks: vec!["https://example.com/hook".into()],
            emails: vec!["ops@example.com".into()],
            smtp_url: Some("smtps://u:p@mail.example.com:465".into()),
            smtp_from: None,
            min_severity: Severity::Critical,
        })
        .await
        .unwrap();
        assert!(n
            .set_server_hook(
                "srv",
                ServerHook {
                    webhook_url: Some("https://example.com/s".into()),
                    events: vec!["server.nope".into()],
                }
            )
            .await
            .is_err());
        n.set_server_hook(
            "srv",
            ServerHook {
                webhook_url: Some("https://example.com/s".into()),
                events: vec!["server.crashed".into()],
            },
        )
        .await
        .unwrap();

        let again = Notifier::load(dir.path(), "node-1");
        let op = again.operator().await;
        assert_eq!(op.webhooks, vec!["https://example.com/hook"]);
        assert_eq!(op.min_severity, Severity::Critical);
        assert_eq!(
            again.server_hook("srv").await.events,
            vec!["server.crashed"]
        );
        again.forget_server("srv").await;
        assert_eq!(again.server_hook("srv").await, ServerHook::default());
        // Clearing the URL removes the hook.
        again
            .set_server_hook(
                "x",
                ServerHook {
                    webhook_url: Some(String::new()),
                    events: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(again.server_hook("x").await, ServerHook::default());
    }

    #[tokio::test]
    async fn repeats_are_held_back_but_single_events_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let n = Notifier::load(dir.path(), "node-1");
        let crash = Notification::new("server.disk_exceeded", Severity::Critical, "t", "b")
            .for_server("a", "A");
        assert!(n.passes_cooldown(&crash).await);
        assert!(!n.passes_cooldown(&crash).await);
        let other = Notification::new("server.disk_exceeded", Severity::Critical, "t", "b")
            .for_server("b", "B");
        assert!(n.passes_cooldown(&other).await);
        let start =
            Notification::new("server.started", Severity::Info, "t", "b").for_server("a", "A");
        assert!(n.passes_cooldown(&start).await);
        assert!(n.passes_cooldown(&start).await);
    }

    #[tokio::test]
    async fn delivery_reports_per_channel() {
        // A server that answers 500 once and 204 once.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(RwLock::new(Vec::<String>::new()));
        let seen = received.clone();
        tokio::spawn(async move {
            for status in ["500 Internal Server Error", "204 No Content"] {
                let (mut sock, _) = listener.accept().await.unwrap();
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let read = sock.read(&mut buf).await.unwrap();
                seen.write().await.push(String::from_utf8_lossy(&buf[..read]).into_owned());
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 {}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                            status
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let n = Arc::new(Notifier::load(dir.path(), "node-1"));
        let url = format!("http://{}/hook", addr);
        n.set_operator(OperatorChannels {
            webhooks: vec![url.clone()],
            ..Default::default()
        })
        .await
        .unwrap();

        let status = n.test().await;
        assert!(status[&url].last_error.as_deref().unwrap().contains("500"));
        let status = n.test().await;
        assert!(status[&url].last_error.is_none());
        assert!(status[&url].last_sent_at.is_some());
        let bodies = received.read().await;
        assert_eq!(bodies.len(), 2);
        assert!(bodies[1].contains("\"kind\":\"node.health\""));
    }
}
