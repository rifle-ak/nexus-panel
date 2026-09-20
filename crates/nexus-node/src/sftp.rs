//! SFTP access to a server's files, served by the node itself.
//!
//! Game-server customers expect to drag a modpack into FileZilla. The node
//! runs a small SSH server that speaks only the SFTP subsystem: no shell,
//! no exec, no forwarding. Each login is jailed to one server's directory,
//! through the same path checks the web file manager uses, and whatever a
//! customer uploads belongs to the game user afterwards.
//!
//! Two kinds of login:
//!
//! - `<server>`: the server's short id (its first eight characters) with the
//!   SFTP password its owner set on the Settings tab. This is how a billing
//!   customer, who signs in to the panel through a one-time link and has no
//!   panel password, gets in.
//! - `<account>.<server>`: a panel account and its password, if the account
//!   is an admin or has `file.sftp` on that server.
//!
//! Failed logins count against the same throttle as the web login, and
//! every attempt is audited.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodSet};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::audit::{AuditEvent, AuditLogger};
use crate::container::ContainerManager;
use crate::error::{NodeError, Result};
use crate::files::FileManager;
use crate::subuser::Permission;
use crate::users::UserStore;
use crate::web::auth::{LoginThrottle, LoginVerdict};

const CREDENTIALS_FILE: &str = ".nexus/sftp.json";
const HOST_KEY_FILE: &str = ".nexus/sftp_host_key.pem";
/// How much of a container id the username carries.
pub const SHORT_ID_LEN: usize = 8;
const INACTIVITY: Duration = Duration::from_secs(15 * 60);
const MAX_READ: u32 = 256 * 1024;

/// How the SFTP server is configured, from the environment.
#[derive(Debug, Clone)]
pub struct SftpSettings {
    /// `NEXUS_SFTP` (default on).
    pub enabled: bool,
    /// `NEXUS_SFTP_BIND` (default `0.0.0.0:2022`).
    pub bind: String,
    /// The host customers connect to (`NEXUS_SFTP_HOST`, else
    /// `NODE_PUBLIC_IP`).
    pub public_host: Option<String>,
}

impl SftpSettings {
    pub fn from_env() -> Self {
        let enabled = std::env::var("NEXUS_SFTP")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "off" | "false" | "0"
                )
            })
            .unwrap_or(true);
        let bind = std::env::var("NEXUS_SFTP_BIND").unwrap_or_else(|_| "0.0.0.0:2022".to_string());
        let public_host = std::env::var("NEXUS_SFTP_HOST")
            .ok()
            .or_else(|| std::env::var("NODE_PUBLIC_IP").ok())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty());
        Self {
            enabled,
            bind,
            public_host,
        }
    }

    pub fn port(&self) -> u16 {
        self.bind.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(2022)
    }
}

/// What the panel shows a server's owner.
#[derive(Debug, Clone, Serialize)]
pub struct SftpInfo {
    pub enabled: bool,
    pub host: Option<String>,
    pub port: u16,
    /// The username this session would use.
    pub username: String,
    /// Whether the server has its own SFTP password set.
    pub has_password: bool,
}

/// Per-server SFTP passwords, hashed.
#[derive(Default, Serialize, Deserialize)]
struct CredentialsFile {
    #[serde(default)]
    servers: HashMap<String, String>,
}

pub struct SftpCredentials {
    path: PathBuf,
    inner: RwLock<CredentialsFile>,
}

impl SftpCredentials {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(CREDENTIALS_FILE);
        let inner = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            inner: RwLock::new(inner),
        }
    }

    async fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&*self.inner.read().await)
            .map_err(|e| NodeError::Internal(format!("serialize sftp credentials: {}", e)))?;
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

    pub async fn has(&self, server_id: &str) -> bool {
        self.inner.read().await.servers.contains_key(server_id)
    }

    pub async fn set(&self, server_id: &str, password: &str) -> Result<()> {
        crate::users::validate_password(password)?;
        let hash = crate::users::hash_password(password)?;
        self.inner.write().await.servers.insert(server_id.to_string(), hash);
        self.save().await
    }

    pub async fn clear(&self, server_id: &str) -> Result<()> {
        if self.inner.write().await.servers.remove(server_id).is_some() {
            self.save().await?;
        }
        Ok(())
    }

    pub async fn verify(&self, server_id: &str, password: &str) -> bool {
        let hash = self.inner.read().await.servers.get(server_id).cloned();
        match hash {
            Some(h) => crate::users::verify_password(&h, password),
            None => false,
        }
    }
}

/// The SFTP server: everything a connection needs to authenticate and to
/// find its jail.
pub struct SftpServer {
    pub settings: SftpSettings,
    data_dir: PathBuf,
    manager: Arc<ContainerManager>,
    users: Arc<UserStore>,
    pub credentials: Arc<SftpCredentials>,
    throttle: Arc<LoginThrottle>,
    audit: Option<Arc<AuditLogger>>,
    node_id: String,
}

/// Who logged in and to what.
#[derive(Debug, Clone)]
pub struct Login {
    pub server_id: String,
    pub server_name: String,
    /// `account` for a panel account, `server` for the server password.
    pub subject: String,
}

impl SftpServer {
    pub fn new(
        settings: SftpSettings,
        data_dir: PathBuf,
        manager: Arc<ContainerManager>,
        users: Arc<UserStore>,
        throttle: Arc<LoginThrottle>,
        audit: Option<Arc<AuditLogger>>,
        node_id: &str,
    ) -> Self {
        Self {
            settings,
            credentials: Arc::new(SftpCredentials::load(&data_dir)),
            data_dir,
            manager,
            users,
            throttle,
            audit,
            node_id: node_id.to_string(),
        }
    }

    /// The short id customers type: the first characters of the container id.
    pub fn short_id(server_id: &str) -> String {
        server_id.chars().take(SHORT_ID_LEN).collect()
    }

    /// What a session should be told about connecting to `server_id`.
    pub async fn info(&self, server_id: &str, account: Option<&str>) -> SftpInfo {
        let short = Self::short_id(server_id);
        SftpInfo {
            enabled: self.settings.enabled,
            host: self.settings.public_host.clone(),
            port: self.settings.port(),
            username: match account {
                Some(a) => format!("{}.{}", a, short),
                None => short,
            },
            has_password: self.credentials.has(server_id).await,
        }
    }

    /// Resolve an SFTP username to a container and, for account logins,
    /// the account. `None` when it names nothing on this node.
    pub async fn resolve_username(&self, user: &str) -> Option<(Option<String>, String)> {
        let user = user.trim();
        let (account, short) = match user.rsplit_once('.') {
            Some((a, s)) if !a.is_empty() => (Some(a.to_ascii_lowercase()), s),
            _ => (None, user),
        };
        if short.len() < 4 || !short.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        let candidates: Vec<String> = self
            .manager
            .list_containers()
            .await
            .into_iter()
            .filter(|c| c.id.starts_with(short))
            .map(|c| c.id)
            .collect();
        match candidates.as_slice() {
            [id] => Some((account, id.clone())),
            _ => None,
        }
    }

    /// Check a password against what the username names. Throttled per
    /// address and audited either way.
    pub async fn authenticate(&self, peer: &str, user: &str, password: &str) -> Option<Login> {
        if let LoginVerdict::Blocked { .. } = self.throttle.check(peer).await {
            warn!("SFTP login from {} refused: too many failed attempts", peer);
            return None;
        }
        let resolved = self.resolve_username(user).await;
        let login = match &resolved {
            Some((Some(account), server_id)) => {
                match self.users.verify_login(account, password).await {
                    Some(u)
                        if u.admin
                            || u.grants
                                .get(server_id)
                                .is_some_and(|g| g.contains(&Permission::FileSftp)) =>
                    {
                        Some((server_id.clone(), format!("account:{}", u.username)))
                    }
                    _ => None,
                }
            }
            Some((None, server_id)) => {
                if self.credentials.verify(server_id, password).await {
                    Some((server_id.clone(), "server".to_string()))
                } else {
                    None
                }
            }
            None => None,
        };
        match login {
            Some((server_id, subject)) => {
                self.throttle.record_success(peer).await;
                let server_name = self
                    .manager
                    .get_state(&server_id)
                    .await
                    .map(|s| s.name)
                    .unwrap_or_else(|_| server_id.clone());
                self.record(
                    AuditEvent::authentication_success(user, "sftp")
                        .with_source_ip(peer.to_string())
                        .with_context("server", &server_id),
                )
                .await;
                info!("SFTP login {} from {} to {}", user, peer, server_id);
                Some(Login {
                    server_id,
                    server_name,
                    subject,
                })
            }
            None => {
                self.throttle.record_failure(peer).await;
                self.record(
                    AuditEvent::authentication_failure(user, "invalid credentials")
                        .with_context("method", "sftp")
                        .with_source_ip(peer.to_string()),
                )
                .await;
                None
            }
        }
    }

    async fn record(&self, event: AuditEvent) {
        if let Some(a) = &self.audit {
            a.log(event.with_node_id(self.node_id.clone())).await;
        }
    }

    /// The file manager a login works through: jailed to its server,
    /// handing what it writes to the game user.
    pub fn files_for(&self, server_id: &str) -> FileManager {
        FileManager::new(server_id, &self.data_dir).with_owner(self.manager.game_user())
    }

    async fn over_allowance(&self, server_id: &str) -> bool {
        self.manager
            .get_state(server_id)
            .await
            .map(|s| s.disk_exceeded())
            .unwrap_or(false)
    }

    /// The host key, generated on first run.
    fn host_key(&self) -> Result<russh_keys::key::KeyPair> {
        let path = self.data_dir.join(HOST_KEY_FILE);
        if path.exists() {
            return russh_keys::load_secret_key(&path, None).map_err(|e| {
                NodeError::Internal(format!("SFTP host key {:?} is unreadable: {}", path, e))
            });
        }
        let key = russh_keys::key::KeyPair::generate_ed25519()
            .ok_or_else(|| NodeError::Internal("could not generate an SSH host key".to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut pem = Vec::new();
        russh_keys::encode_pkcs8_pem(&key, &mut pem)
            .map_err(|e| NodeError::Internal(format!("could not encode the host key: {}", e)))?;
        std::fs::write(&path, pem)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        info!("Generated SFTP host key at {:?}", path);
        Ok(key)
    }

    fn ssh_config(&self) -> Result<russh::server::Config> {
        Ok(russh::server::Config {
            methods: MethodSet::PASSWORD,
            auth_rejection_time: Duration::from_secs(2),
            auth_rejection_time_initial: Some(Duration::from_millis(200)),
            keys: vec![self.host_key()?],
            inactivity_timeout: Some(INACTIVITY),
            max_auth_attempts: 3,
            ..Default::default()
        })
    }

    /// Bind and serve for the life of the process. Returns the address
    /// bound, so a test can use port 0.
    pub async fn start(self: &Arc<Self>) -> Result<SocketAddr> {
        let config = Arc::new(self.ssh_config()?);
        let listener = tokio::net::TcpListener::bind(&self.settings.bind).await?;
        let addr = listener.local_addr()?;
        let mut server = Listener {
            inner: self.clone(),
        };
        tokio::spawn(async move {
            use russh::server::Server as _;
            if let Err(e) = server.run_on_socket(config, &listener).await {
                warn!("SFTP server stopped: {}", e);
            }
        });
        info!("SFTP server listening on {}", addr);
        Ok(addr)
    }
}

struct Listener {
    inner: Arc<SftpServer>,
}

impl russh::server::Server for Listener {
    type Handler = Connection;

    fn new_client(&mut self, peer_addr: Option<SocketAddr>) -> Self::Handler {
        Connection {
            server: self.inner.clone(),
            peer: peer_addr.map(|a| a.ip().to_string()).unwrap_or_else(|| "unknown".to_string()),
            login: None,
            channels: HashMap::new(),
        }
    }
}

/// One SSH connection.
pub struct Connection {
    server: Arc<SftpServer>,
    peer: String,
    login: Option<Login>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

#[async_trait::async_trait]
impl russh::server::Handler for Connection {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _user: &str) -> std::result::Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: Some(MethodSet::PASSWORD),
        })
    }

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> std::result::Result<Auth, Self::Error> {
        match self.server.authenticate(&self.peer, user, password).await {
            Some(login) => {
                self.login = Some(login);
                Ok(Auth::Accept)
            }
            None => Ok(Auth::Reject {
                proceed_with_methods: Some(MethodSet::PASSWORD),
            }),
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> std::result::Result<bool, Self::Error> {
        if self.login.is_none() {
            return Ok(false);
        }
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        session.close(channel);
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_failure(channel);
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_failure(channel);
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        let (Some(login), Some(channel)) = (self.login.clone(), self.channels.remove(&channel_id))
        else {
            session.channel_failure(channel_id);
            return Ok(());
        };
        if name != "sftp" {
            session.channel_failure(channel_id);
            return Ok(());
        }
        session.channel_success(channel_id);
        let jail = Jail::new(self.server.clone(), login);
        russh_sftp::server::run(channel.into_stream(), jail).await;
        Ok(())
    }
}

enum Opened {
    File {
        file: tokio::fs::File,
        path: PathBuf,
        written: bool,
    },
    Dir {
        entries: Vec<File>,
        sent: bool,
    },
}

/// The SFTP session for one login: every path goes through the server's
/// file jail.
pub struct Jail {
    server: Arc<SftpServer>,
    login: Login,
    files: FileManager,
    handles: HashMap<String, Opened>,
    next_handle: u64,
}

impl Jail {
    fn new(server: Arc<SftpServer>, login: Login) -> Self {
        let files = server.files_for(&login.server_id);
        Self {
            server,
            login,
            files,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }

    fn resolve(&self, path: &str) -> std::result::Result<PathBuf, StatusCode> {
        self.files.resolve_path(path).map_err(|_| StatusCode::PermissionDenied)
    }

    /// The path as the client sees it: absolute within the jail.
    fn virtual_path(&self, real: &Path) -> String {
        let root = self.files.server_dir();
        match real.strip_prefix(root) {
            Ok(rel) if rel.as_os_str().is_empty() => "/".to_string(),
            Ok(rel) => format!("/{}", rel.display()),
            Err(_) => "/".to_string(),
        }
    }

    fn handle(&mut self, opened: Opened) -> String {
        let id = format!("h{}", self.next_handle);
        self.next_handle += 1;
        self.handles.insert(id.clone(), opened);
        id
    }

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_string(),
            language_tag: "en-US".to_string(),
        }
    }
}

fn io_status(e: &std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

fn attrs_of(meta: &std::fs::Metadata) -> FileAttributes {
    let mut attrs = FileAttributes::from(meta);
    // The client sees the game user's files as its own.
    attrs.user = Some("server".to_string());
    attrs.group = Some("server".to_string());
    attrs
}

async fn stat_path(path: &Path, follow: bool) -> std::result::Result<FileAttributes, StatusCode> {
    let meta = if follow {
        tokio::fs::metadata(path).await
    } else {
        tokio::fs::symlink_metadata(path).await
    }
    .map_err(|e| io_status(&e))?;
    Ok(attrs_of(&meta))
}

impl russh_sftp::server::Handler for Jail {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> std::result::Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> std::result::Result<Name, Self::Error> {
        let real = self.resolve(&path)?;
        Ok(Name {
            id,
            files: vec![File::dummy(self.virtual_path(&real))],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> std::result::Result<Attrs, Self::Error> {
        let real = self.resolve(&path)?;
        Ok(Attrs {
            id,
            attrs: stat_path(&real, true).await?,
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> std::result::Result<Attrs, Self::Error> {
        let real = self.resolve(&path)?;
        Ok(Attrs {
            id,
            attrs: stat_path(&real, false).await?,
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> std::result::Result<Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(Opened::File { file, .. }) => {
                let meta = file.metadata().await.map_err(|e| io_status(&e))?;
                Ok(Attrs {
                    id,
                    attrs: attrs_of(&meta),
                })
            }
            Some(Opened::Dir { .. }) => Err(StatusCode::OpUnsupported),
            None => Err(StatusCode::NoSuchFile),
        }
    }

    async fn opendir(&mut self, id: u32, path: String) -> std::result::Result<Handle, Self::Error> {
        let real = self.resolve(&path)?;
        let mut dir = tokio::fs::read_dir(&real).await.map_err(|e| io_status(&e))?;
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let attrs = match entry.metadata().await {
                Ok(m) => attrs_of(&m),
                Err(_) => FileAttributes::default(),
            };
            entries.push(File::new(name, attrs));
        }
        entries.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(Handle {
            id,
            handle: self.handle(Opened::Dir {
                entries,
                sent: false,
            }),
        })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> std::result::Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Opened::Dir { entries, sent }) => {
                if *sent {
                    return Err(StatusCode::Eof);
                }
                *sent = true;
                Ok(Name {
                    id,
                    files: std::mem::take(entries),
                })
            }
            _ => Err(StatusCode::NoSuchFile),
        }
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> std::result::Result<Handle, Self::Error> {
        let real = self.resolve(&filename)?;
        let writing = pflags.intersects(
            OpenFlags::WRITE | OpenFlags::APPEND | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        );
        if writing && self.server.over_allowance(&self.login.server_id).await {
            return Err(StatusCode::PermissionDenied);
        }
        let mut options = tokio::fs::OpenOptions::new();
        options
            .read(pflags.contains(OpenFlags::READ) || !writing)
            .write(pflags.contains(OpenFlags::WRITE))
            .append(pflags.contains(OpenFlags::APPEND))
            .create(pflags.contains(OpenFlags::CREATE))
            .truncate(pflags.contains(OpenFlags::TRUNCATE))
            .create_new(pflags.contains(OpenFlags::EXCLUDE));
        let file = options.open(&real).await.map_err(|e| io_status(&e))?;
        Ok(Handle {
            id,
            handle: self.handle(Opened::File {
                file,
                path: real,
                written: false,
            }),
        })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> std::result::Result<Data, Self::Error> {
        let Some(Opened::File { file, .. }) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| io_status(&e))?;
        let mut buf = vec![0u8; len.min(MAX_READ) as usize];
        let mut filled = 0;
        while filled < buf.len() {
            let n = file.read(&mut buf[filled..]).await.map_err(|e| io_status(&e))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(filled);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> std::result::Result<Status, Self::Error> {
        let Some(Opened::File { file, written, .. }) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| io_status(&e))?;
        file.write_all(&data).await.map_err(|e| io_status(&e))?;
        *written = true;
        Ok(Self::ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> std::result::Result<Status, Self::Error> {
        match self.handles.remove(&handle) {
            Some(Opened::File {
                mut file,
                path,
                written,
            }) => {
                if written {
                    let _ = file.flush().await;
                    drop(file);
                    self.files.claim_path(&path);
                }
                Ok(Self::ok(id))
            }
            Some(Opened::Dir { .. }) => Ok(Self::ok(id)),
            None => Err(StatusCode::NoSuchFile),
        }
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> std::result::Result<Status, Self::Error> {
        let real = self.resolve(&path)?;
        tokio::fs::create_dir(&real).await.map_err(|e| io_status(&e))?;
        self.files.claim_path(&real);
        Ok(Self::ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> std::result::Result<Status, Self::Error> {
        let real = self.resolve(&path)?;
        if real == self.files.server_dir() {
            return Err(StatusCode::PermissionDenied);
        }
        tokio::fs::remove_dir(&real).await.map_err(|e| io_status(&e))?;
        Ok(Self::ok(id))
    }

    async fn remove(
        &mut self,
        id: u32,
        filename: String,
    ) -> std::result::Result<Status, Self::Error> {
        let real = self.resolve(&filename)?;
        tokio::fs::remove_file(&real).await.map_err(|e| io_status(&e))?;
        Ok(Self::ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> std::result::Result<Status, Self::Error> {
        let from = self.resolve(&oldpath)?;
        let to = self.resolve(&newpath)?;
        if from == self.files.server_dir() || to == self.files.server_dir() {
            return Err(StatusCode::PermissionDenied);
        }
        tokio::fs::rename(&from, &to).await.map_err(|e| io_status(&e))?;
        Ok(Self::ok(id))
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> std::result::Result<Status, Self::Error> {
        let real = self.resolve(&path)?;
        if let Some(size) = attrs.size {
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&real)
                .await
                .map_err(|e| io_status(&e))?;
            file.set_len(size).await.map_err(|e| io_status(&e))?;
        }
        #[cfg(unix)]
        if let Some(mode) = attrs.permissions {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                tokio::fs::set_permissions(&real, std::fs::Permissions::from_mode(mode & 0o777))
                    .await;
        }
        Ok(Self::ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> std::result::Result<Status, Self::Error> {
        let Some(Opened::File { file, path, .. }) = self.handles.get(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        if let Some(size) = attrs.size {
            file.set_len(size).await.map_err(|e| io_status(&e))?;
        }
        #[cfg(unix)]
        if let Some(mode) = attrs.permissions {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
                .await;
        }
        Ok(Self::ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> std::result::Result<Name, Self::Error> {
        let real = self.resolve(&path)?;
        let target = tokio::fs::read_link(&real).await.map_err(|e| io_status(&e))?;
        Ok(Name {
            id,
            files: vec![File::dummy(target.to_string_lossy().into_owned())],
        })
    }

    async fn symlink(
        &mut self,
        _id: u32,
        _linkpath: String,
        _targetpath: String,
    ) -> std::result::Result<Status, Self::Error> {
        // A link is the one thing that could point outside the jail.
        Err(StatusCode::PermissionDenied)
    }
}

/// Seconds since the epoch, for tests that compare timestamps.
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn credentials_hash_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let c = SftpCredentials::load(dir.path());
        assert!(c.set("srv", "short").await.is_err());
        c.set("srv", "correct horse battery").await.unwrap();
        assert!(c.has("srv").await);
        assert!(c.verify("srv", "correct horse battery").await);
        assert!(!c.verify("srv", "wrong password!!").await);
        assert!(!c.verify("other", "correct horse battery").await);
        let text = std::fs::read_to_string(dir.path().join(CREDENTIALS_FILE)).unwrap();
        assert!(!text.contains("correct horse"));
        let again = SftpCredentials::load(dir.path());
        assert!(again.verify("srv", "correct horse battery").await);
        again.clear("srv").await.unwrap();
        assert!(!again.has("srv").await);
    }

    struct TrustAll;

    #[async_trait::async_trait]
    impl russh::client::Handler for TrustAll {
        type Error = anyhow::Error;

        async fn check_server_key(
            &mut self,
            _key: &russh_keys::key::PublicKey,
        ) -> std::result::Result<bool, Self::Error> {
            Ok(true)
        }
    }

    const SIMPLE: &str = r#"
metadata: { id: simple, name: Simple, version: "1", game: simple, author: test }
container: { image: example/simple:latest }
resources:
  cpu: { min: 500, max: 1000, shares: 1024 }
  memory: { min: 512Mi, max: 1Gi }
  disk: { min: 1Gi }
startup: { command: /bin/true, working_dir: /home/container }
variables: []
networking: { ports: [] }
security: { capabilities: { drop: [], add: [] } }
"#;

    async fn connect(
        addr: SocketAddr,
        user: &str,
        password: &str,
    ) -> std::result::Result<russh::client::Handle<TrustAll>, anyhow::Error> {
        let config = Arc::new(russh::client::Config::default());
        let mut session = russh::client::connect(config, addr, TrustAll).await?;
        if !session.authenticate_password(user, password).await? {
            anyhow::bail!("authentication refused");
        }
        Ok(session)
    }

    /// The whole thing, end to end: a real SSH client against the server on
    /// a loopback port, jailed to one server directory.
    #[tokio::test]
    async fn sftp_end_to_end_is_jailed_and_authenticated() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let manager = Arc::new(ContainerManager::new(data_dir.clone()));
        let config: nexus_config::GameConfig = serde_yaml::from_str(SIMPLE).unwrap();
        let id = manager.create_container(&config, None).await.unwrap();
        let other = manager.create_container(&config, None).await.unwrap();
        std::fs::write(data_dir.join(&id).join("server.properties"), "motd=hi").unwrap();
        std::fs::write(data_dir.join(&other).join("secret.txt"), "other tenant").unwrap();
        std::fs::write(data_dir.join("node-secret"), "outside").unwrap();

        let users = Arc::new(UserStore::load(&data_dir));
        let mut grants = crate::users::Grants::new();
        grants.insert(
            id.clone(),
            [Permission::FileSftp, Permission::FileRead].into_iter().collect(),
        );
        users.create_user("ops", "ops password 123", false, grants).await.unwrap();
        users
            .create_user("viewer", "viewer password 1", false, Default::default())
            .await
            .unwrap();

        let server = Arc::new(SftpServer::new(
            SftpSettings {
                enabled: true,
                bind: "127.0.0.1:0".into(),
                public_host: None,
            },
            data_dir.clone(),
            manager.clone(),
            users,
            Arc::new(LoginThrottle::new()),
            None,
            "test",
        ));
        server.credentials.set(&id, "customer password").await.unwrap();
        let addr = server.start().await.unwrap();
        let short = SftpServer::short_id(&id);

        // Wrong password, a server with no password, an account without the
        // grant, and a name that matches nothing: all refused.
        assert!(connect(addr, &short, "wrong password!!").await.is_err());
        assert!(connect(addr, &SftpServer::short_id(&other), "customer password").await.is_err());
        assert!(connect(addr, &format!("viewer.{}", short), "viewer password 1").await.is_err());
        assert!(connect(addr, "nope1234", "customer password").await.is_err());

        // The customer login: jailed to its server.
        let session = connect(addr, &short, "customer password").await.unwrap();
        let channel = session.channel_open_session().await.unwrap();
        channel.request_subsystem(true, "sftp").await.unwrap();
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.unwrap();
        assert_eq!(sftp.canonicalize(".").await.unwrap(), "/");
        assert_eq!(sftp.canonicalize("/a/../..").await.unwrap_or_default(), "");
        let names: Vec<String> = sftp.read_dir("/").await.unwrap().map(|e| e.file_name()).collect();
        assert_eq!(names, vec!["server.properties"]);

        // Traversal, in every spelling, stays inside.
        assert!(sftp.read_dir("..").await.is_err());
        assert!(sftp.metadata("../node-secret").await.is_err());
        assert!(sftp.metadata(&format!("../{}/secret.txt", other)).await.is_err());
        assert!(sftp.symlink("link", "/etc").await.is_err());

        // Upload, download, rename, delete.
        let mut f = sftp
            .open_with_flags(
                "/plugins.txt",
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            )
            .await
            .unwrap();
        f.write_all(b"hello over sftp").await.unwrap();
        f.shutdown().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(data_dir.join(&id).join("plugins.txt")).unwrap(),
            "hello over sftp"
        );
        let mut f = sftp.open("/server.properties").await.unwrap();
        let mut text = String::new();
        f.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "motd=hi");
        sftp.create_dir("/mods").await.unwrap();
        sftp.rename("/plugins.txt", "/mods/plugins.txt").await.unwrap();
        assert!(data_dir.join(&id).join("mods/plugins.txt").exists());
        sftp.remove_file("/mods/plugins.txt").await.unwrap();
        sftp.remove_dir("/mods").await.unwrap();
        assert!(sftp.remove_dir("/").await.is_err());
        sftp.close().await.unwrap();

        // The account login works with the grant, on that server only.
        let session = connect(addr, &format!("ops.{}", short), "ops password 123").await.unwrap();
        let channel = session.channel_open_session().await.unwrap();
        channel.request_subsystem(true, "sftp").await.unwrap();
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.unwrap();
        assert!(sftp.metadata("/server.properties").await.is_ok());
        sftp.close().await.unwrap();
        assert!(connect(
            addr,
            &format!("ops.{}", SftpServer::short_id(&other)),
            "ops password 123"
        )
        .await
        .is_err());

        // No shell.
        let session = connect(addr, &short, "customer password").await.unwrap();
        let mut channel = session.channel_open_session().await.unwrap();
        channel.request_shell(true).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), channel.wait())
            .await
            .expect("a reply to the shell request");
        assert!(
            matches!(reply, Some(russh::ChannelMsg::Failure) | None),
            "shell request was not refused: {:?}",
            reply
        );
    }

    #[test]
    fn settings_read_the_environment_shape() {
        let s = SftpSettings {
            enabled: true,
            bind: "0.0.0.0:2022".into(),
            public_host: None,
        };
        assert_eq!(s.port(), 2022);
        assert_eq!(SftpServer::short_id("abcdefghijkl"), "abcdefgh");
    }
}
