use super::*;
use crate::error::{NodeError, Result};
use async_trait::async_trait;
use containerd_client::{
    services::v1::{
        containers_client::ContainersClient, content_client::ContentClient,
        images_client::ImagesClient, leases_client::LeasesClient,
        snapshots::snapshots_client::SnapshotsClient, snapshots::MountsRequest,
        snapshots::PrepareSnapshotRequest, snapshots::RemoveSnapshotRequest,
        tasks_client::TasksClient, Container as ContainerdContainer, CreateContainerRequest,
        CreateRequest as CreateLeaseRequest, CreateTaskRequest, DeleteContainerRequest,
        DeleteProcessRequest, DeleteRequest as DeleteLeaseRequest, DeleteTaskRequest,
        ExecProcessRequest, GetContainerRequest, GetImageRequest, GetRequest as GetTaskRequest,
        KillRequest, ReadContentRequest, StartRequest, WaitRequest,
    },
    tonic::{transport::Channel, Code, Request},
    with_namespace,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::image::{self, ImageConfig};

/// Snapshotter used for container root filesystems when none is configured.
/// This is containerd's own default, and the one `ctr` unpacks images into.
pub const DEFAULT_SNAPSHOTTER: &str = "overlayfs";

/// Where the runtime keeps per-container stdio: the stdin FIFO and the console
/// log the shim appends to.
///
/// The node overrides this with a directory under its data dir (see
/// `with_state_dir`), which is what the packaged systemd unit grants write
/// access to; this default only applies to callers that do not set one.
const DEFAULT_STATE_DIR: &str = "/run/nexus-node";

/// How long to wait for a container to actually die after SIGKILL before
/// giving up. The kernel delivers it immediately, but the process still has to
/// be reaped and its cgroup torn down.
const SIGKILL_GRACE: Duration = Duration::from_secs(15);

/// How long a `ctr images pull` may run. Game-server base images are large and
/// pulled over whatever link the node has, so this is generous; it exists to
/// stop a wedged pull from pinning a request forever, not to bound normal use.
const PULL_TIMEOUT: Duration = Duration::from_secs(45 * 60);

/// Containerd container runtime implementation
pub struct ContainerdRuntime {
    channel: Arc<RwLock<Option<Channel>>>,
    socket_path: String,
    namespace: String,
    snapshotter: String,
    state_dir: PathBuf,
    /// Open stdin FIFO per running container.
    ///
    /// The shim copies our stdin FIFO into the game process and closes that
    /// process's stdin as soon as every writer goes away — so if each console
    /// attach opened the FIFO itself, the first detach would hand the game
    /// server an EOF on stdin. Holding one writer open for the life of the
    /// task keeps stdin alive across attaches.
    stdin: Arc<RwLock<HashMap<String, Arc<Mutex<tokio::fs::File>>>>>,
}

impl ContainerdRuntime {
    /// Create a new Containerd runtime
    pub fn new(socket_path: String, namespace: String) -> Self {
        let snapshotter = std::env::var("NEXUS_SNAPSHOTTER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SNAPSHOTTER.to_string());

        let state_dir = std::env::var("NEXUS_RUNTIME_STATE_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR));

        Self {
            channel: Arc::new(RwLock::new(None)),
            socket_path,
            namespace,
            snapshotter,
            state_dir,
            stdin: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Override the snapshotter used for container root filesystems.
    pub fn with_snapshotter(mut self, snapshotter: impl Into<String>) -> Self {
        self.snapshotter = snapshotter.into();
        self
    }

    /// Override where per-container stdio (stdin FIFO, console log) is kept.
    pub fn with_state_dir(mut self, state_dir: impl Into<PathBuf>) -> Self {
        self.state_dir = state_dir.into();
        self
    }

    /// Path of the console log the shim appends a container's output to.
    pub fn log_path(&self, id: &str) -> PathBuf {
        self.state_dir.join("logs").join(&self.namespace).join(format!("{}.log", id))
    }

    /// Path of the FIFO carrying a container's stdin.
    fn stdin_path(&self, id: &str) -> PathBuf {
        self.state_dir.join("io").join(&self.namespace).join(id).join("stdin")
    }

    /// Connect to Containerd
    pub async fn connect(&self) -> Result<()> {
        info!(
            "Connecting to Containerd at {} (namespace: {})",
            self.socket_path, self.namespace
        );

        // Strip any "unix://" prefix to get the raw filesystem path
        let socket_path = self.socket_path.strip_prefix("unix://").unwrap_or(&self.socket_path);

        // Use the containerd-client crate's connect helper which properly
        // creates a Unix domain socket connection via tower::service_fn
        let channel = containerd_client::connect(socket_path)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to connect: {}", e)))?;

        let mut guard = self.channel.write().await;
        *guard = Some(channel);

        info!("Successfully connected to Containerd");
        Ok(())
    }

    /// Get or create a connected channel
    async fn get_channel(&self) -> Result<Channel> {
        let channel = self.channel.read().await;
        if let Some(ref ch) = *channel {
            return Ok(ch.clone());
        }

        drop(channel); // Release read lock before connecting
        self.connect().await?;

        let channel = self.channel.read().await;
        channel
            .as_ref()
            .cloned()
            .ok_or_else(|| NodeError::ContainerdError("Failed to connect".to_string()))
    }

    /// Create containers client
    async fn containers_client(&self) -> Result<ContainersClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ContainersClient::new(channel))
    }

    /// Create images client
    async fn images_client(&self) -> Result<ImagesClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ImagesClient::new(channel))
    }

    /// Create tasks client
    async fn tasks_client(&self) -> Result<TasksClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(TasksClient::new(channel))
    }

    /// Create a snapshots client
    async fn snapshots_client(&self) -> Result<SnapshotsClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(SnapshotsClient::new(channel))
    }

    /// Create a content client
    async fn content_client(&self) -> Result<ContentClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(ContentClient::new(channel))
    }

    /// Create a leases client
    async fn leases_client(&self) -> Result<LeasesClient<Channel>> {
        let channel = self.get_channel().await?;
        Ok(LeasesClient::new(channel))
    }

    /// Build a request carrying our namespace and, optionally, a lease.
    ///
    /// containerd's garbage collector removes any snapshot that nothing
    /// references. A snapshot is unreferenced between being prepared and the
    /// container that names it being created, so that window is held open by a
    /// lease — without it a GC pass landing in between deletes the rootfs out
    /// from under a container that is about to use it.
    fn request<T>(&self, message: T, lease: Option<&str>) -> Request<T> {
        let mut req = Request::new(message);
        let md = req.metadata_mut();
        md.insert(
            "containerd-namespace",
            self.namespace.parse().expect("namespace is not a valid header value"),
        );
        if let Some(lease) = lease {
            if let Ok(value) = lease.parse() {
                md.insert("containerd-lease", value);
            }
        }
        req
    }

    /// Read a blob out of containerd's content store.
    async fn read_content(&self, digest: &str) -> Result<Vec<u8>> {
        let mut client = self.content_client().await?;

        let req = self.request(
            ReadContentRequest {
                digest: digest.to_string(),
                offset: 0,
                size: 0,
            },
            None,
        );

        let mut stream = client
            .read(req)
            .await
            .map_err(|e| {
                NodeError::ContainerdError(format!("Failed to read content {}: {}", digest, e))
            })?
            .into_inner();

        let mut data = Vec::new();
        while let Some(chunk) = stream.message().await.map_err(|e| {
            NodeError::ContainerdError(format!("Failed to read content {}: {}", digest, e))
        })? {
            data.extend_from_slice(&chunk.data);
        }

        Ok(data)
    }

    /// Resolve an image reference to the config of the manifest for this host.
    ///
    /// Walks index → manifest → config in the content store. The config is
    /// what carries both the layer diff IDs (which give the rootfs chain ID)
    /// and the image's own process defaults.
    async fn resolve_image(&self, image: &str) -> Result<ImageConfig> {
        let mut images = self.images_client().await?;

        let req = self.request(
            GetImageRequest {
                name: image.to_string(),
            },
            None,
        );

        let record = images
            .get(req)
            .await
            .map_err(|status| {
                if status.code() == Code::NotFound {
                    NodeError::ContainerdError(format!(
                        "Image {} is not present on this node",
                        image
                    ))
                } else {
                    NodeError::ContainerdError(format!(
                        "Failed to look up image {}: {}",
                        image, status
                    ))
                }
            })?
            .into_inner()
            .image
            .ok_or_else(|| {
                NodeError::ContainerdError(format!("Image {} has no target descriptor", image))
            })?;

        let target = record.target.ok_or_else(|| {
            NodeError::ContainerdError(format!("Image {} has no target descriptor", image))
        })?;

        let (arch, variant) = image::host_architecture();

        // An index has to be narrowed to this host's manifest first.
        let manifest_digest = if image::is_index(&target.media_type) {
            let index = self.read_content(&target.digest).await?;
            image::select_manifest(&index, arch, variant)?.digest
        } else if image::is_manifest(&target.media_type) {
            target.digest.clone()
        } else {
            return Err(NodeError::ContainerdError(format!(
                "Image {} has unsupported media type {}",
                image, target.media_type
            )));
        };

        let manifest = self.read_content(&manifest_digest).await?;
        let config_descriptor = image::config_descriptor(&manifest)?;
        let config = self.read_content(&config_descriptor.digest).await?;

        image::parse_config(&config)
    }

    /// Prepare the writable snapshot that becomes a container's root
    /// filesystem.
    ///
    /// The parent is the image's chain ID — the key containerd committed when
    /// it unpacked the image. Returns whether the snapshot was created here: an
    /// existing one is reused, and must not be cleaned up on failure, because
    /// it belongs to a container that already exists.
    async fn prepare_rootfs(&self, id: &str, chain_id: &str, lease: &str) -> Result<bool> {
        let mut snapshots = self.snapshots_client().await?;

        let req = self.request(
            PrepareSnapshotRequest {
                snapshotter: self.snapshotter.clone(),
                key: id.to_string(),
                parent: chain_id.to_string(),
                ..Default::default()
            },
            Some(lease),
        );

        match snapshots.prepare(req).await {
            Ok(_) => {
                debug!(
                    "Prepared {} snapshot {} from {}",
                    self.snapshotter, id, chain_id
                );
                Ok(true)
            }
            Err(status) if status.code() == Code::AlreadyExists => {
                debug!("Reusing existing snapshot for container {}", id);
                Ok(false)
            }
            Err(status) if status.code() == Code::NotFound => {
                Err(NodeError::ContainerdError(format!(
                    "Image layers for chain {} are not unpacked into the {} snapshotter. \
                     Pull the image again so containerd unpacks it: \
                     ctr -n {} images pull {}",
                    chain_id, self.snapshotter, self.namespace, id
                )))
            }
            Err(status) => Err(NodeError::ContainerdError(format!(
                "Failed to prepare rootfs snapshot: {}",
                status
            ))),
        }
    }

    /// Look up the mounts that make up a container's root filesystem.
    ///
    /// containerd does not resolve these itself: a task create request has to
    /// carry the snapshot's mounts, or the shim has nothing to chroot into.
    async fn rootfs_mounts(
        &self,
        snapshotter: &str,
        key: &str,
    ) -> Result<Vec<containerd_client::types::Mount>> {
        let mut snapshots = self.snapshots_client().await?;

        let req = self.request(
            MountsRequest {
                snapshotter: snapshotter.to_string(),
                key: key.to_string(),
            },
            None,
        );

        let mounts = snapshots
            .mounts(req)
            .await
            .map_err(|e| {
                NodeError::ContainerdError(format!(
                    "Failed to resolve rootfs mounts for {}: {}",
                    key, e
                ))
            })?
            .into_inner()
            .mounts;

        if mounts.is_empty() {
            return Err(NodeError::ContainerdError(format!(
                "Container {} has no rootfs mounts — its snapshot is missing",
                key
            )));
        }

        Ok(mounts)
    }

    /// Create a lease that keeps freshly prepared content alive.
    ///
    /// The lease carries a GC expiry so a node that dies mid-create does not
    /// leave a snapshot pinned forever.
    async fn create_lease(&self, id: &str) -> Result<String> {
        let mut leases = self.leases_client().await?;

        let expires = chrono::Utc::now() + chrono::Duration::hours(24);
        let mut labels = HashMap::new();
        labels.insert("containerd.io/gc.expire".to_string(), expires.to_rfc3339());

        let req = self.request(
            CreateLeaseRequest {
                id: format!("nexus-{}", id),
                labels,
            },
            None,
        );

        let lease = leases
            .create(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to create lease: {}", e)))?
            .into_inner()
            .lease
            .ok_or_else(|| NodeError::ContainerdError("Lease service returned no lease".into()))?;

        Ok(lease.id)
    }

    /// Drop a lease once whatever it was protecting has a permanent reference.
    async fn delete_lease(&self, lease: &str) {
        let Ok(mut leases) = self.leases_client().await else {
            return;
        };

        let req = self.request(
            DeleteLeaseRequest {
                id: lease.to_string(),
                sync: false,
            },
            None,
        );

        if let Err(e) = leases.delete(req).await {
            warn!("Failed to release lease {}: {}", lease, e);
        }
    }

    /// Remove a container's rootfs snapshot.
    async fn remove_rootfs(&self, snapshotter: &str, key: &str) {
        let Ok(mut snapshots) = self.snapshots_client().await else {
            return;
        };

        let req = self.request(
            RemoveSnapshotRequest {
                snapshotter: snapshotter.to_string(),
                key: key.to_string(),
            },
            None,
        );

        if let Err(e) = snapshots.remove(req).await {
            if e.code() != Code::NotFound {
                warn!("Failed to remove snapshot for {}: {}", key, e);
            }
        }
    }

    /// Fetch the container record, which holds the snapshot the task needs.
    async fn get_container(&self, id: &str) -> Result<ContainerdContainer> {
        let mut client = self.containers_client().await?;
        let req = self.request(GetContainerRequest { id: id.to_string() }, None);

        client
            .get(req)
            .await
            .map_err(|e| {
                if e.code() == Code::NotFound {
                    NodeError::ContainerNotFound(id.to_string())
                } else {
                    NodeError::ContainerdError(format!("Failed to get container {}: {}", id, e))
                }
            })?
            .into_inner()
            .container
            .ok_or_else(|| NodeError::ContainerNotFound(id.to_string()))
    }

    /// Look up the task belonging to exactly this container.
    ///
    /// `Ok(None)` means the container has no task right now (never started, or
    /// stopped and cleaned up), which is different from an RPC failure.
    async fn task_status(&self, id: &str) -> Result<Option<containerd_client::types::v1::Process>> {
        let mut tasks_client = self.tasks_client().await?;

        let req = self.request(
            GetTaskRequest {
                container_id: id.to_string(),
                exec_id: String::new(),
            },
            None,
        );

        match tasks_client.get(req).await {
            Ok(response) => Ok(response.into_inner().process),
            Err(status) if status.code() == Code::NotFound => Ok(None),
            Err(status) => Err(NodeError::ContainerdError(format!(
                "Failed to get task for {}: {}",
                id, status
            ))),
        }
    }

    /// Block until a container's task exits, returning its exit code.
    async fn wait_task(&self, id: &str) -> Result<i32> {
        let mut tasks_client = self.tasks_client().await?;

        let req = self.request(
            WaitRequest {
                container_id: id.to_string(),
                exec_id: String::new(),
            },
            None,
        );

        match tasks_client.wait(req).await {
            Ok(response) => Ok(response.into_inner().exit_status as i32),
            // The task was reaped while we waited; treat that as a clean exit.
            Err(status) if status.code() == Code::NotFound => Ok(0),
            Err(status) => Err(NodeError::ContainerdError(format!(
                "Failed to wait for container {}: {}",
                id, status
            ))),
        }
    }

    /// Prepare the stdio a task is created with.
    ///
    /// stdout and stderr use containerd's `file://` stdio scheme: the shim
    /// itself appends the container's output to that file. A FIFO would need a
    /// reader attached for the whole life of the server — as soon as one filled
    /// up, the game process would block on its own log output.
    ///
    /// stdin has to stay a FIFO, since that is the only way to write into a
    /// running container; it is created here and held open by [`open_stdin`].
    async fn prepare_stdio(&self, id: &str) -> Result<(String, String)> {
        let log_path = self.log_path(id);
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                NodeError::Internal(format!(
                    "Failed to create log directory {:?}: {}",
                    parent, e
                ))
            })?;
        }

        let stdin_path = self.stdin_path(id);
        if let Some(parent) = stdin_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                NodeError::Internal(format!(
                    "Failed to create stdio directory {:?}: {}",
                    parent, e
                ))
            })?;
        }
        if !stdin_path.exists() {
            mkfifo(&stdin_path)?;
        }

        Ok((
            stdin_path.to_string_lossy().to_string(),
            format!("file://{}", log_path.to_string_lossy()),
        ))
    }

    /// Open (and remember) the writer that keeps a container's stdin alive.
    ///
    /// Opened read-write so the call never blocks waiting for the shim's reader
    /// and the FIFO never reports end-of-file while the container runs.
    async fn open_stdin(&self, id: &str) -> Result<Arc<Mutex<tokio::fs::File>>> {
        if let Some(existing) = self.stdin.read().await.get(id) {
            return Ok(existing.clone());
        }

        let path = self.stdin_path(id);
        let file = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
        })
        .await
        .map_err(|e| NodeError::Internal(format!("Failed to open stdin: {}", e)))?
        .map_err(|e| NodeError::Internal(format!("Failed to open stdin FIFO: {}", e)))?;

        let handle = Arc::new(Mutex::new(tokio::fs::File::from_std(file)));
        self.stdin.write().await.insert(id.to_string(), handle.clone());
        Ok(handle)
    }

    /// Drop the stdin writer for a container that is no longer running.
    async fn close_stdin(&self, id: &str) {
        self.stdin.write().await.remove(id);
    }
}

#[async_trait]
impl ContainerRuntime for ContainerdRuntime {
    async fn pull_image(&self, image: &str) -> Result<()> {
        info!("[Containerd] Checking image: {}", image);

        let mut client = self.images_client().await?;

        // Already present (and, since containerd unpacks on pull, already
        // usable as a rootfs) — nothing to do.
        let req = self.request(
            GetImageRequest {
                name: image.to_string(),
            },
            None,
        );

        match client.get(req).await {
            Ok(_) => {
                debug!("Image {} already exists", image);
                return Ok(());
            }
            Err(status) if status.code() == Code::NotFound => {}
            Err(status) => {
                return Err(NodeError::ContainerdError(format!(
                    "Failed to check image {}: {}",
                    image, status
                )));
            }
        }

        info!("Pulling image {} into namespace {}", image, self.namespace);
        self.pull_with_ctr(image).await?;

        // Confirm the pull actually registered the image, so a silent partial
        // pull surfaces here rather than as a missing rootfs at start.
        let req = self.request(
            GetImageRequest {
                name: image.to_string(),
            },
            None,
        );
        client.get(req).await.map_err(|status| {
            NodeError::ContainerdError(format!(
                "Image {} is still missing after pulling it: {}",
                image, status
            ))
        })?;

        info!("Successfully pulled image {}", image);
        Ok(())
    }

    async fn create(&self, id: &str, spec: ContainerSpec) -> Result<ContainerInfo> {
        info!("[Containerd] Creating container: {}", id);

        // The image's own config supplies both the rootfs chain ID and the
        // process defaults a blueprint only partly overrides.
        let image_config = self.resolve_image(&spec.image).await?;
        let chain_id = image_config.chain_id().ok_or_else(|| {
            NodeError::ContainerdError(format!(
                "Image {} declares no layers, so it has no root filesystem",
                spec.image
            ))
        })?;

        // Hold the snapshot alive until the container references it.
        let lease = self.create_lease(id).await?;
        let mut snapshot_is_ours = false;
        let result = match self.prepare_rootfs(id, &chain_id, &lease).await {
            Ok(created) => {
                snapshot_is_ours = created;
                self.create_record(id, &spec, &image_config, &lease).await
            }
            Err(e) => Err(e),
        };
        self.delete_lease(&lease).await;

        if result.is_err() && snapshot_is_ours {
            // Clean up the snapshot this call created, so a retry starts from
            // the image rather than silently inheriting a half-built rootfs.
            // A snapshot that was already there belongs to an existing
            // container and is emphatically not ours to delete.
            self.remove_rootfs(&self.snapshotter, id).await;
        }
        result?;

        info!("Successfully created container: {}", id);

        Ok(ContainerInfo {
            id: id.to_string(),
            pid: None,
            status: "created".to_string(),
            exit_code: None,
        })
    }

    async fn start(&self, id: &str) -> Result<u32> {
        info!("[Containerd] Starting container: {}", id);

        let container = self.get_container(id).await?;
        if container.snapshot_key.is_empty() {
            return Err(NodeError::ContainerdError(format!(
                "Container {} has no root filesystem snapshot — recreate it",
                id
            )));
        }

        let snapshotter = if container.snapshotter.is_empty() {
            self.snapshotter.clone()
        } else {
            container.snapshotter.clone()
        };

        // The task has to be handed the rootfs mounts explicitly; containerd
        // does not look them up from the container record.
        let rootfs = self.rootfs_mounts(&snapshotter, &container.snapshot_key).await?;
        let (stdin_path, stdout_uri) = self.prepare_stdio(id).await?;

        let mut tasks_client = self.tasks_client().await?;

        let req = self.request(
            CreateTaskRequest {
                container_id: id.to_string(),
                rootfs,
                stdin: stdin_path,
                stdout: stdout_uri.clone(),
                stderr: stdout_uri,
                terminal: false,
                ..Default::default()
            },
            None,
        );

        let task_response = tasks_client
            .create(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to create task: {}", e)))?
            .into_inner();

        let pid = task_response.pid;

        // Keep one writer on the stdin FIFO for the life of the task so console
        // sessions can come and go without closing the game process's stdin.
        if let Err(e) = self.open_stdin(id).await {
            warn!("Console input unavailable for container {}: {}", id, e);
        }

        // Start the task
        let req = self.request(
            StartRequest {
                container_id: id.to_string(),
                ..Default::default()
            },
            None,
        );

        if let Err(e) = tasks_client.start(req).await {
            // A created-but-never-started task would block every later start
            // with "task already exists", so clean it up before reporting.
            let cleanup = self.request(
                DeleteTaskRequest {
                    container_id: id.to_string(),
                },
                None,
            );
            let _ = tasks_client.delete(cleanup).await;
            self.close_stdin(id).await;

            return Err(NodeError::ContainerdError(format!(
                "Failed to start task: {}",
                e
            )));
        }

        info!("Successfully started container: {} (PID: {})", id, pid);
        Ok(pid)
    }

    async fn stop(&self, id: &str, timeout_secs: u32) -> Result<i32> {
        info!(
            "[Containerd] Stopping container: {} (timeout: {}s)",
            id, timeout_secs
        );

        let Some(task) = self.task_status(id).await? else {
            debug!("Container {} has no task to stop", id);
            self.close_stdin(id).await;
            return Ok(0);
        };

        let mut tasks_client = self.tasks_client().await?;

        // Status 3 is Stopped: the process is already gone and only the task
        // record is left to reap.
        let exit_code = if task.status == 3 {
            task.exit_status as i32
        } else {
            // Ask politely first.
            let req = self.request(
                KillRequest {
                    container_id: id.to_string(),
                    signal: 15, // SIGTERM
                    ..Default::default()
                },
                None,
            );

            if let Err(e) = tasks_client.kill(req).await {
                warn!("Failed to send SIGTERM to container {}: {}", id, e);
            }

            match tokio::time::timeout(Duration::from_secs(timeout_secs as u64), self.wait_task(id))
                .await
            {
                Ok(Ok(code)) => code,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    warn!(
                        "Container {} did not stop within {}s, sending SIGKILL",
                        id, timeout_secs
                    );

                    let req = self.request(
                        KillRequest {
                            container_id: id.to_string(),
                            signal: 9, // SIGKILL
                            ..Default::default()
                        },
                        None,
                    );

                    tasks_client.kill(req).await.map_err(|e| {
                        NodeError::ContainerdError(format!("Failed to send SIGKILL: {}", e))
                    })?;

                    // SIGKILL is not instantaneous, and deleting a task whose
                    // process is still alive is rejected — so wait for the
                    // exit rather than racing it.
                    match tokio::time::timeout(SIGKILL_GRACE, self.wait_task(id)).await {
                        Ok(Ok(code)) => code,
                        Ok(Err(e)) => return Err(e),
                        Err(_) => {
                            return Err(NodeError::ContainerdError(format!(
                                "Container {} is still running {}s after SIGKILL",
                                id,
                                SIGKILL_GRACE.as_secs()
                            )))
                        }
                    }
                }
            }
        };

        // Delete the task
        let req = self.request(
            DeleteTaskRequest {
                container_id: id.to_string(),
            },
            None,
        );

        tasks_client
            .delete(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to delete task: {}", e)))?;

        // The task is gone, so nothing is reading the stdin FIFO any more.
        self.close_stdin(id).await;

        info!(
            "Successfully stopped container: {} (exit code: {})",
            id, exit_code
        );
        Ok(exit_code)
    }

    async fn kill(&self, id: &str, signal: i32) -> Result<()> {
        let mut tasks_client = self.tasks_client().await?;
        let req = self.request(
            KillRequest {
                container_id: id.to_string(),
                signal: signal as u32,
                ..Default::default()
            },
            None,
        );
        tasks_client.kill(req).await.map_err(|e| {
            NodeError::ContainerdError(format!(
                "Failed to send signal {} to container {}: {}",
                signal, id, e
            ))
        })?;
        Ok(())
    }

    async fn trim_console_log(&self, id: &str, max_bytes: u64, keep_bytes: u64) -> Result<()> {
        let path = self.log_path(id);
        tokio::task::spawn_blocking(move || trim_log_file(&path, max_bytes, keep_bytes))
            .await
            .map_err(|e| NodeError::Internal(format!("log trim task failed: {}", e)))?
    }

    async fn wait(&self, id: &str, timeout: Duration) -> Result<i32> {
        debug!("[Containerd] Waiting for container {} to exit", id);

        match tokio::time::timeout(timeout, self.wait_task(id)).await {
            Ok(result) => result,
            Err(_) => Err(NodeError::ContainerdError(format!(
                "Container {} did not exit within {}s",
                id,
                timeout.as_secs()
            ))),
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        info!("[Containerd] Deleting container: {}", id);

        let mut tasks_client = self.tasks_client().await?;
        let mut containers_client = self.containers_client().await?;

        // The snapshotter the container was created with, before the record
        // that names it goes away.
        let snapshotter = match self.get_container(id).await {
            Ok(container) if !container.snapshotter.is_empty() => container.snapshotter,
            _ => self.snapshotter.clone(),
        };

        // Try to delete task first (if it exists)
        let req = self.request(
            DeleteTaskRequest {
                container_id: id.to_string(),
            },
            None,
        );

        if let Err(e) = tasks_client.delete(req).await {
            debug!("Failed to delete task (may not exist): {}", e);
        }

        self.close_stdin(id).await;

        // Delete the container
        let req = self.request(DeleteContainerRequest { id: id.to_string() }, None);

        containers_client.delete(req).await.map_err(|e| {
            NodeError::ContainerdError(format!("Failed to delete container: {}", e))
        })?;

        // Only once the container record is gone is its snapshot unreferenced.
        // Leaving it behind would leak a full rootfs per deleted server.
        self.remove_rootfs(&snapshotter, id).await;

        let io_dir = self.stdin_path(id);
        if let Some(dir) = io_dir.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }

        // Drop the console log with the container. Keeping it would leave a
        // deleted server's output on disk forever, and — because a container
        // id can be reused, as the per-server install container's is — the
        // next occupant would start by reading the last one's output.
        let _ = std::fs::remove_file(self.log_path(id));

        info!("Successfully deleted container: {}", id);
        Ok(())
    }

    async fn inspect(&self, id: &str) -> Result<ContainerInfo> {
        info!("[Containerd] Inspecting container: {}", id);

        // Confirm the container exists before reporting on its task.
        self.get_container(id).await?;

        // Get task info (if exists). This has to be a targeted lookup:
        // containerd's task list ignores its filter and hands back every task
        // in the namespace, so picking the first one reports another server's
        // state as this one's.
        let (pid, status, exit_code) = if let Some(task) = self.task_status(id).await? {
            // Status values: Unknown=0, Created=1, Running=2, Stopped=3, Paused=4, Pausing=5
            let status_str = match task.status {
                0 => "unknown",
                1 => "created",
                2 => "running",
                3 => "stopped",
                4 => "paused",
                5 => "pausing",
                _ => "unknown",
            };

            let pid = if task.pid > 0 { Some(task.pid) } else { None };
            let exit_code = if task.status == 3 {
                // Stopped
                Some(task.exit_status as i32)
            } else {
                None
            };

            (pid, status_str.to_string(), exit_code)
        } else {
            (None, "created".to_string(), None)
        };

        Ok(ContainerInfo {
            id: id.to_string(),
            pid,
            status,
            exit_code,
        })
    }

    async fn attach(&self, id: &str) -> Result<Box<dyn ConsoleStream>> {
        info!("[Containerd] Attaching to container (read-only): {}", id);

        Ok(Box::new(ContainerdConsoleStream::new(
            id,
            self.log_path(id),
        )))
    }

    async fn attach_bidirectional(&self, id: &str) -> Result<Box<dyn BidirectionalConsole>> {
        info!(
            "[Containerd] Attaching to container (bidirectional): {}",
            id
        );

        // Verify container exists and is running
        let info = self.inspect(id).await?;
        if info.status != "running" {
            return Err(NodeError::InvalidInput(format!(
                "Container {} is not running (status: {})",
                id, info.status
            )));
        }

        Ok(Box::new(ContainerdBidirectionalConsole::new(
            id,
            self.log_path(id),
            self.open_stdin(id).await?,
        )))
    }

    async fn send_command(&self, id: &str, command: &str) -> Result<()> {
        info!(
            "[Containerd] Sending command to container {}: {}",
            id, command
        );

        // Verify container exists and is running
        let info = self.inspect(id).await?;
        if info.status != "running" {
            return Err(NodeError::InvalidInput(format!(
                "Container {} is not running (status: {})",
                id, info.status
            )));
        }

        // Write straight to the container's stdin; a game server reads its
        // console commands as whole lines.
        let stdin = self.open_stdin(id).await?;
        let line = format!("{}\n", command);
        {
            use tokio::io::AsyncWriteExt;
            let mut file = stdin.lock().await;
            file.write_all(line.as_bytes())
                .await
                .map_err(|e| NodeError::Internal(format!("Failed to write to stdin: {}", e)))?;
            file.flush()
                .await
                .map_err(|e| NodeError::Internal(format!("Failed to flush stdin: {}", e)))?;
        }

        info!("Successfully sent command to container {}", id);
        Ok(())
    }

    async fn exec(&self, id: &str, command: &[String], timeout: Duration) -> Result<ExecOutput> {
        info!("[Containerd] Exec in container {}: {:?}", id, command);

        if command.is_empty() {
            return Err(NodeError::InvalidInput("Empty command".to_string()));
        }

        // Container must be running to exec into it.
        let info = self.inspect(id).await?;
        if info.status != "running" {
            return Err(NodeError::InvalidInput(format!(
                "Container {} is not running (status: {})",
                id, info.status
            )));
        }

        // The whole exec is bounded by `timeout` so a stuck process (or a
        // FIFO that never receives a writer) can never hang the node. A
        // timed-out process is killed and its record removed: abandoning it
        // would leave it running inside the customer's container.
        let exec_id = format!("exec-{}", Uuid::new_v4());
        let fifo_dir = std::env::temp_dir().join(format!("nexus-exec-{}", exec_id));
        match tokio::time::timeout(timeout, self.exec_inner(id, &exec_id, &fifo_dir, command)).await
        {
            Ok(result) => result,
            Err(_) => {
                self.reap_exec(id, &exec_id, &fifo_dir).await;
                Err(NodeError::Internal(format!(
                    "exec in container {} timed out after {}s",
                    id,
                    timeout.as_secs()
                )))
            }
        }
    }
}

impl ContainerdRuntime {
    /// Kill and forget an exec process that outlived its timeout.
    async fn reap_exec(&self, id: &str, exec_id: &str, fifo_dir: &Path) {
        if let Ok(mut tasks_client) = self.tasks_client().await {
            let kill = with_namespace!(
                KillRequest {
                    container_id: id.to_string(),
                    exec_id: exec_id.to_string(),
                    signal: 9,
                    all: false,
                },
                &self.namespace
            );
            let _ = tasks_client.kill(kill).await;
            // The process needs a moment to die before its record can go.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let delete = with_namespace!(
                DeleteProcessRequest {
                    container_id: id.to_string(),
                    exec_id: exec_id.to_string(),
                },
                &self.namespace
            );
            let _ = tasks_client.delete_process(delete).await;
        }
        let _ = std::fs::remove_dir_all(fifo_dir);
    }
}

impl ContainerdRuntime {
    /// Pull an image with containerd's own `ctr` client.
    ///
    /// containerd's pull is not a single RPC: fetching a manifest, ingesting
    /// every layer into the content store and unpacking them into the
    /// snapshotter is a client-side pipeline, and the transfer service that
    /// exposes it as one call only exists on containerd 1.7 and newer. `ctr`
    /// ships with the daemon at every version we support and does all of it,
    /// including registry auth from the host's containerd configuration.
    async fn pull_with_ctr(&self, image: &str) -> Result<()> {
        let ctr = ctr_binary().ok_or_else(|| {
            NodeError::ContainerdError(format!(
                "Cannot pull {}: containerd's `ctr` client was not found on PATH. \
                 Install it (it ships with containerd) or pull the image manually: \
                 ctr -n {} images pull {}",
                image, self.namespace, image
            ))
        })?;

        let socket = self.socket_path.strip_prefix("unix://").unwrap_or(&self.socket_path);
        let (arch, _) = image::host_architecture();

        let mut command = tokio::process::Command::new(&ctr);
        command
            .arg("--address")
            .arg(socket)
            .arg("--namespace")
            .arg(&self.namespace)
            .arg("images")
            .arg("pull")
            .arg("--platform")
            .arg(format!("linux/{}", arch))
            .arg(image)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        let output = tokio::time::timeout(PULL_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                NodeError::ContainerdError(format!(
                    "Pulling image {} timed out after {} minutes",
                    image,
                    PULL_TIMEOUT.as_secs() / 60
                ))
            })?
            .map_err(|e| NodeError::ContainerdError(format!("Failed to run {:?}: {}", ctr, e)))?;

        if !output.status.success() {
            // `ctr` reports the useful part (auth failure, unknown tag,
            // unreachable registry) on stderr; surfacing it verbatim is what
            // makes a failed pull diagnosable from the panel.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr.trim().lines().last().unwrap_or("no output").to_string();
            return Err(NodeError::ContainerdError(format!(
                "Failed to pull image {}: {}",
                image, reason
            )));
        }

        Ok(())
    }

    /// Register the container record naming an already-prepared snapshot.
    async fn create_record(
        &self,
        id: &str,
        spec: &ContainerSpec,
        image_config: &ImageConfig,
        lease: &str,
    ) -> Result<()> {
        let oci_spec = spec_to_oci(spec, image_config)?;

        let container = ContainerdContainer {
            id: id.to_string(),
            image: spec.image.clone(),
            runtime: Some(containerd_client::services::v1::container::Runtime {
                name: "io.containerd.runc.v2".to_string(),
                options: None,
            }),
            spec: Some(prost_types::Any {
                type_url: "types.containerd.io/opencontainers/runtime-spec/1/Spec".to_string(),
                value: oci_spec.into_bytes(),
            }),
            // Naming the snapshot here is what makes it the container's rootfs
            // and stops the garbage collector from reclaiming it.
            snapshotter: self.snapshotter.clone(),
            snapshot_key: id.to_string(),
            ..Default::default()
        };

        let mut client = self.containers_client().await?;
        let req = self.request(
            CreateContainerRequest {
                container: Some(container),
            },
            Some(lease),
        );

        client.create(req).await.map_err(|e| {
            NodeError::ContainerdError(format!("Failed to create container: {}", e))
        })?;

        Ok(())
    }

    /// Inner exec implementation (wrapped in a timeout by `exec`).
    ///
    /// Spawns a new process in the running task via the containerd Exec API,
    /// capturing stdout/stderr through FIFOs, and waits for the exit code.
    async fn exec_inner(
        &self,
        id: &str,
        exec_id: &str,
        fifo_dir: &Path,
        command: &[String],
    ) -> Result<ExecOutput> {
        let mut tasks_client = self.tasks_client().await?;
        let exec_id = exec_id.to_string();

        // Create a private FIFO directory for this exec's stdio.
        std::fs::create_dir_all(fifo_dir)
            .map_err(|e| NodeError::Internal(format!("Failed to create exec fifo dir: {}", e)))?;
        let stdout_path = fifo_dir.join("stdout");
        let stderr_path = fifo_dir.join("stderr");
        mkfifo(&stdout_path)?;
        mkfifo(&stderr_path)?;

        // OCI runtime-spec Process describing the command to run. containerd
        // decodes this Any via its typeurl registration for runtime-spec types.
        let process = serde_json::json!({
            "terminal": false,
            "cwd": "/",
            "args": command,
            "env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],
        });
        let spec = prost_types::Any {
            type_url: "types.containerd.io/opencontainers/runtime-spec/1/Process".to_string(),
            value: serde_json::to_vec(&process).map_err(|e| {
                NodeError::Internal(format!("Failed to encode process spec: {}", e))
            })?,
        };

        // Reader tasks block on opening the FIFO until containerd's shim opens
        // the write end at Start; spawning them first avoids a lost-output race.
        let out_reader = spawn_fifo_reader(stdout_path.clone());
        let err_reader = spawn_fifo_reader(stderr_path.clone());

        // Register the exec process.
        let req = with_namespace!(
            ExecProcessRequest {
                container_id: id.to_string(),
                exec_id: exec_id.clone(),
                terminal: false,
                stdin: String::new(),
                stdout: stdout_path.to_string_lossy().to_string(),
                stderr: stderr_path.to_string_lossy().to_string(),
                spec: Some(spec),
            },
            &self.namespace
        );
        tasks_client
            .exec(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to create exec: {}", e)))?;

        // Start it.
        let req = with_namespace!(
            StartRequest {
                container_id: id.to_string(),
                exec_id: exec_id.clone(),
            },
            &self.namespace
        );
        tasks_client
            .start(req)
            .await
            .map_err(|e| NodeError::ContainerdError(format!("Failed to start exec: {}", e)))?;

        // Wait for completion and collect the exit code.
        let req = with_namespace!(
            WaitRequest {
                container_id: id.to_string(),
                exec_id: exec_id.clone(),
            },
            &self.namespace
        );
        let exit_code = match tasks_client.wait(req).await {
            Ok(resp) => Some(resp.into_inner().exit_status as i32),
            Err(e) => {
                warn!("exec wait failed for {}: {}", exec_id, e);
                None
            }
        };

        // Collect captured output (readers finish at process EOF).
        let mut stdout = String::new();
        if let Ok(Ok(bytes)) = tokio::time::timeout(Duration::from_secs(2), out_reader).await {
            stdout = bytes;
        }
        let mut stderr = String::new();
        if let Ok(Ok(bytes)) = tokio::time::timeout(Duration::from_secs(2), err_reader).await {
            stderr = bytes;
        }

        // Best-effort cleanup of the exec process record and FIFOs.
        let req = with_namespace!(
            DeleteProcessRequest {
                container_id: id.to_string(),
                exec_id: exec_id.clone(),
            },
            &self.namespace
        );
        let _ = tasks_client.delete_process(req).await;
        let _ = std::fs::remove_dir_all(fifo_dir);

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
        })
    }
}

/// This process's hard limit on open files, which is the most it can grant a
/// container it starts.
fn max_open_files() -> u64 {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` only writes into the struct we hand it.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        limit.rlim_max
    } else {
        // Fall back to the conservative floor every Linux system allows.
        1024
    }
}

/// Locate containerd's `ctr` client.
///
/// Checked against `PATH` first, then the usual install locations, because the
/// node commonly runs from systemd with a minimal `PATH` that omits
/// `/usr/local/bin` — where a hand-installed containerd puts `ctr`.
fn ctr_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("NEXUS_CTR_BINARY") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("ctr");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    ["/usr/bin/ctr", "/usr/local/bin/ctr", "/bin/ctr"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Create a FIFO (named pipe) at `path`.
fn mkfifo(path: &std::path::Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| NodeError::Internal(format!("Invalid fifo path: {}", e)))?;
    // 0o600: owner read/write only.
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if rc != 0 {
        return Err(NodeError::Internal(format!(
            "mkfifo({:?}) failed: {}",
            path,
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// Spawn a task that reads a FIFO to EOF and returns its contents as a String.
fn spawn_fifo_reader(path: std::path::PathBuf) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        match tokio::fs::File::open(&path).await {
            Ok(mut file) => {
                let mut buf = Vec::new();
                let _ = file.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).to_string()
            }
            Err(_) => String::new(),
        }
    })
}

// Helper functions for converting between our types and Containerd types

/// `PATH` for containers whose image does not set one.
const DEFAULT_PATH: &str = "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The filesystems every Linux container needs before it can run anything.
///
/// Without these a container has no `/proc`, no `/dev/null` and no `/dev/shm`;
/// almost every real workload — a shell script, SteamCMD, a game server —
/// fails immediately and obscurely.
fn default_mounts() -> Vec<serde_json::Value> {
    use serde_json::json;

    vec![
        json!({
            "destination": "/proc",
            "type": "proc",
            "source": "proc",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        json!({
            "destination": "/dev",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
        }),
        json!({
            "destination": "/dev/pts",
            "type": "devpts",
            "source": "devpts",
            "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620", "gid=5"]
        }),
        json!({
            "destination": "/dev/shm",
            "type": "tmpfs",
            "source": "shm",
            "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
        }),
        json!({
            "destination": "/dev/mqueue",
            "type": "mqueue",
            "source": "mqueue",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        json!({
            "destination": "/sys",
            "type": "sysfs",
            "source": "sysfs",
            "options": ["nosuid", "noexec", "nodev", "ro"]
        }),
        json!({
            "destination": "/sys/fs/cgroup",
            "type": "cgroup",
            "source": "cgroup",
            "options": ["nosuid", "noexec", "nodev", "relatime", "ro"]
        }),
    ]
}

/// Host files a container needs to resolve names on the host network.
///
/// Servers run in the host's network namespace (see [`spec_to_oci`]), so they
/// should resolve names the same way the host does. Each is only bound in if
/// the host actually has it.
fn host_network_mounts() -> Vec<serde_json::Value> {
    use serde_json::json;

    ["/etc/resolv.conf", "/etc/hosts", "/etc/localtime"]
        .iter()
        .filter(|path| Path::new(path).exists())
        .map(|path| {
            json!({
                "destination": path,
                "type": "bind",
                "source": path,
                "options": ["rbind", "ro"]
            })
        })
        .collect()
}

/// Merge the image's environment with the blueprint's, blueprint winning.
fn merge_env(image_env: &[String], spec_env: &HashMap<String, String>) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();

    for (key, value) in spec_env {
        merged.push(format!("{}={}", key, value));
    }

    for entry in image_env {
        let key = entry.split('=').next().unwrap_or(entry);
        if !spec_env.contains_key(key) {
            merged.push(entry.clone());
        }
    }

    if !merged.iter().any(|e| e.starts_with("PATH=")) {
        merged.push(DEFAULT_PATH.to_string());
    }

    merged.sort();
    merged
}

/// Convert a [`ContainerSpec`] and its image's config into an OCI runtime spec.
///
/// Two deliberate choices are worth calling out:
///
/// * The container keeps the **host network namespace**. The node has no CNI
///   plugin, so a private namespace would give a game server a loopback
///   interface and nothing else — no Steam login, no players. Ports are
///   allocated on the host to match.
/// * Anything the blueprint does not specify falls back to the image's own
///   config, the way any other OCI runtime resolves it.
///
/// Confinement comes from `spec.security`: an unprivileged user, a bounded
/// capability set, `noNewPrivileges`, and a seccomp allowlist. With the host
/// network shared, these are what stand between a compromised game server and
/// the node.
fn spec_to_oci(spec: &ContainerSpec, image: &ImageConfig) -> Result<String> {
    use serde_json::json;

    // Build full command from command + args, falling back to the image's own
    // entrypoint when the blueprint gives none.
    let mut process_args = spec.command.clone();
    process_args.extend(spec.args.clone());
    if process_args.is_empty() {
        process_args = image.entrypoint.clone();
        process_args.extend(image.cmd.clone());
    }
    if process_args.is_empty() {
        return Err(NodeError::InvalidConfig {
            reason: format!(
                "Neither the blueprint nor image {} specifies a command to run",
                spec.image
            ),
        });
    }

    let mut env = merge_env(&image.env, &spec.env);
    // A process that is not root should not think it is.
    if spec.security.uid != 0 && !env.iter().any(|e| e.starts_with("HOME=")) {
        let home = if spec.working_dir.is_empty() {
            "/".to_string()
        } else {
            spec.working_dir.clone()
        };
        env.push(format!("HOME={}", home));
        env.sort();
    }

    // A container cannot be given a higher hard limit than the runtime that
    // launches it already holds — runc refuses the whole container with
    // "operation not permitted" rather than clamping. Asking for more than we
    // have would turn a working server into one that will not start, so the
    // request is capped at what this process can actually confer. Raise the
    // node's own `LimitNOFILE` to raise this ceiling.
    let nofile = spec.resources.nofile.min(max_open_files());
    let mut rlimits = vec![json!({
        "type": "RLIMIT_NOFILE",
        "hard": nofile,
        "soft": nofile
    })];
    for limit in &spec.resources.rlimits {
        if limit.kind == "RLIMIT_NOFILE" {
            continue;
        }
        rlimits.push(json!({
            "type": limit.kind,
            "hard": limit.hard,
            "soft": limit.soft
        }));
    }

    let working_dir = if !spec.working_dir.is_empty() {
        spec.working_dir.clone()
    } else {
        image.working_dir.clone().unwrap_or_else(|| "/".to_string())
    };

    // Default filesystems first, then the host's resolver config, then the
    // blueprint's own bind mounts — later mounts land on top.
    let mut mounts = default_mounts();
    mounts.extend(host_network_mounts());
    mounts.extend(spec.mounts.iter().map(|m| {
        json!({
            "destination": m.target,
            "type": "bind",
            "source": m.source,
            "options": if m.read_only { vec!["rbind", "ro"] } else { vec!["rbind", "rw"] }
        })
    }));
    if spec.security.read_only_root {
        // A read-only image still needs somewhere for scratch files.
        mounts.push(json!({
            "destination": "/tmp",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=1777", "size=1g"]
        }));
    }

    // ── Resources ────────────────────────────────────────────────────
    let mut cpu = json!({ "shares": spec.resources.cpu_shares });
    if let Some(millicores) = spec.resources.cpu_millicores {
        // A hard cap: `quota` microseconds of CPU per `period`. 1000
        // millicores = one full core = 100 000 of every 100 000 µs.
        cpu["period"] = json!(100_000u64);
        cpu["quota"] = json!(millicores as u64 * 100);
    }
    let memory_limit = spec.resources.memory_bytes as i64;
    let mut memory = json!({ "limit": memory_limit });
    if memory_limit > 0 {
        // OCI's `swap` is memory *plus* swap; equal to the limit means none.
        memory["swap"] =
            json!(memory_limit.saturating_add(spec.resources.memory_swap_bytes as i64));
    }
    let mut resources = json!({ "cpu": cpu, "memory": memory });
    if let Some(pids) = spec.resources.pids_limit {
        resources["pids"] = json!({ "limit": pids as i64 });
    }

    // ── Process identity and confinement ─────────────────────────────
    let caps = &spec.security.capabilities;
    let mut capabilities = json!({
        "bounding": caps,
        "effective": caps,
        "permitted": caps,
        "inheritable": caps,
    });
    if spec.security.uid != 0 {
        // Without an ambient set the kernel drops every capability on the
        // first exec of a non-root process; ambient is what lets a blueprint
        // grant NET_BIND_SERVICE to an unprivileged server.
        capabilities["ambient"] = json!(caps);
    }

    let hostname = if spec.hostname.is_empty() {
        "container".to_string()
    } else {
        spec.hostname.clone()
    };

    let mut linux = json!({
        "resources": resources,
        // No "network" entry: the container shares the host's network
        // namespace, since the node runs without CNI.
        "namespaces": [
            {"type": "pid"},
            {"type": "ipc"},
            {"type": "uts"},
            {"type": "mount"}
        ],
        "maskedPaths": [
            "/proc/acpi",
            "/proc/asound",
            "/proc/kcore",
            "/proc/keys",
            "/proc/latency_stats",
            "/proc/timer_list",
            "/proc/timer_stats",
            "/proc/sched_debug",
            "/proc/scsi",
            "/sys/firmware",
            "/sys/devices/virtual/powercap"
        ],
        "readonlyPaths": [
            "/proc/bus",
            "/proc/fs",
            "/proc/irq",
            "/proc/sys",
            "/proc/sysrq-trigger"
        ]
    });
    match &spec.security.seccomp {
        SeccompProfile::RuntimeDefault => {
            linux["seccomp"] = seccomp::default_profile();
        }
        SeccompProfile::Unconfined => {}
        SeccompProfile::Path(path) => {
            let raw = std::fs::read(path).map_err(|e| NodeError::InvalidConfig {
                reason: format!("seccomp profile {} is unreadable: {}", path, e),
            })?;
            let profile: serde_json::Value =
                serde_json::from_slice(&raw).map_err(|e| NodeError::InvalidConfig {
                    reason: format!("seccomp profile {} is not valid JSON: {}", path, e),
                })?;
            linux["seccomp"] = profile;
        }
    }

    let oci_spec = json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": {
                "uid": spec.security.uid,
                "gid": spec.security.gid
            },
            "args": process_args,
            "env": env,
            "cwd": working_dir,
            "capabilities": capabilities,
            "rlimits": rlimits,
            "noNewPrivileges": spec.security.no_new_privileges
        },
        "root": {
            // Relative to the bundle: the shim mounts the container's snapshot
            // here before handing the bundle to runc.
            "path": "rootfs",
            "readonly": spec.security.read_only_root
        },
        "hostname": hostname,
        "mounts": mounts,
        "linux": linux
    });

    serde_json::to_string(&oci_spec)
        .map_err(|e| NodeError::Internal(format!("Failed to serialize OCI spec: {}", e)))
}

/// Keep only the tail of a console log that has outgrown `max_bytes`.
///
/// The shim appends with `O_APPEND`, so truncating to zero and writing the
/// tail back is safe: its next write lands after whatever is there. A viewer
/// following the log notices the file shrank and reopens it.
fn trim_log_file(path: &Path, max_bytes: u64, keep_bytes: u64) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(NodeError::Internal(format!("stat {:?}: {}", path, e))),
    };
    if meta.len() <= max_bytes {
        return Ok(());
    }
    let keep = keep_bytes.min(max_bytes);

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| NodeError::Internal(format!("open {:?}: {}", path, e)))?;
    file.seek(SeekFrom::End(-(keep as i64)))
        .map_err(|e| NodeError::Internal(format!("seek {:?}: {}", path, e)))?;
    let mut tail = Vec::with_capacity(keep as usize);
    file.read_to_end(&mut tail)
        .map_err(|e| NodeError::Internal(format!("read {:?}: {}", path, e)))?;
    // Start on a line boundary so the first kept line is whole.
    if let Some(nl) = tail.iter().position(|&b| b == b'\n') {
        tail.drain(..=nl);
    }
    file.set_len(0)
        .map_err(|e| NodeError::Internal(format!("truncate {:?}: {}", path, e)))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| NodeError::Internal(format!("seek {:?}: {}", path, e)))?;
    file.write_all(&tail)
        .map_err(|e| NodeError::Internal(format!("write {:?}: {}", path, e)))?;
    Ok(())
}

/// How much of an existing console log a new attach starts from, so a panel
/// opening the console sees the last few lines rather than an empty pane.
const CONSOLE_BACKLOG_BYTES: u64 = 16 * 1024;

/// Open a container's console log positioned near its end.
///
/// The log is the file containerd's shim appends the container's output to.
/// Reading it (rather than the stdio FIFO directly) means several viewers can
/// follow the same server at once, and that output written while nobody was
/// watching is still there.
async fn open_console_log(path: &Path) -> Result<Option<tokio::io::BufReader<tokio::fs::File>>> {
    use tokio::io::{AsyncBufReadExt, AsyncSeekExt};

    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The shim creates the log when the task starts; before that there
            // is simply nothing to read yet.
            return Ok(None);
        }
        Err(e) => {
            return Err(NodeError::Internal(format!(
                "Failed to open console log {:?}: {}",
                path, e
            )))
        }
    };

    let len = file
        .metadata()
        .await
        .map_err(|e| NodeError::Internal(format!("Failed to stat console log: {}", e)))?
        .len();

    let reader = if len > CONSOLE_BACKLOG_BYTES {
        file.seek(std::io::SeekFrom::Start(len - CONSOLE_BACKLOG_BYTES))
            .await
            .map_err(|e| NodeError::Internal(format!("Failed to seek console log: {}", e)))?;
        let mut reader = tokio::io::BufReader::new(file);
        // The seek lands mid-line; drop that partial line.
        let mut partial = String::new();
        let _ = reader.read_line(&mut partial).await;
        reader
    } else {
        tokio::io::BufReader::new(file)
    };

    Ok(Some(reader))
}

/// Console stream that follows a container's console log.
struct ContainerdConsoleStream {
    container_id: String,
    log_path: PathBuf,
    reader: Option<tokio::io::BufReader<tokio::fs::File>>,
    initialized: bool,
}

impl ContainerdConsoleStream {
    fn new(container_id: &str, log_path: PathBuf) -> Self {
        Self {
            container_id: container_id.to_string(),
            log_path,
            reader: None,
            initialized: false,
        }
    }

    /// Open the console log.
    ///
    /// The shim creates the log when the task starts, which can be after a
    /// reader attaches — so a missing file is retried rather than latched as
    /// "no output", which would silently swallow everything a short-lived
    /// container prints.
    async fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        match open_console_log(&self.log_path).await {
            Ok(Some(reader)) => {
                debug!(
                    "Opened console log for container {}: {:?}",
                    self.container_id, self.log_path
                );
                self.reader = Some(reader);
                self.initialized = true;
            }
            // Not there yet; try again on the next read.
            Ok(None) => {}
            Err(e) => {
                warn!(
                    "Failed to open console log for container {}: {}",
                    self.container_id, e
                );
                self.initialized = true;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl ConsoleStream for ContainerdConsoleStream {
    async fn read_line(&mut self) -> Result<Option<String>> {
        use tokio::io::AsyncBufReadExt;

        self.init().await?;

        let Some(reader) = self.reader.as_mut() else {
            return Ok(None);
        };

        let mut line = String::new();
        match reader.read_line(&mut line).await {
            // Caught up with the writer: no more output *for now*. Callers
            // that follow the log come back for more — unless the log was
            // trimmed underneath us, in which case start over from its
            // new beginning.
            Ok(0) => {
                if log_shrank(reader).await {
                    self.reader = None;
                    self.initialized = false;
                }
                Ok(None)
            }
            Ok(_) => Ok(Some(line)),
            Err(e) => Err(NodeError::Internal(format!("Console read error: {}", e))),
        }
    }
}

/// Whether the file behind `reader` is now shorter than the reader's position,
/// which happens when the log was trimmed.
async fn log_shrank(reader: &mut tokio::io::BufReader<tokio::fs::File>) -> bool {
    use tokio::io::AsyncSeekExt;
    let Ok(pos) = reader.stream_position().await else {
        return false;
    };
    match reader.get_ref().metadata().await {
        Ok(meta) => meta.len() < pos,
        Err(_) => false,
    }
}

/// Bidirectional console for Containerd containers.
///
/// Output is followed from the container's console log; input goes to the
/// stdin FIFO writer the runtime holds open for the life of the task.
pub struct ContainerdBidirectionalConsole {
    container_id: String,
    log_path: PathBuf,
    stdin: Arc<Mutex<tokio::fs::File>>,
    stdout_reader: Option<tokio::io::BufReader<tokio::fs::File>>,
    initialized: bool,
    is_open: bool,
}

impl ContainerdBidirectionalConsole {
    pub fn new(container_id: &str, log_path: PathBuf, stdin: Arc<Mutex<tokio::fs::File>>) -> Self {
        Self {
            container_id: container_id.to_string(),
            log_path,
            stdin,
            stdout_reader: None,
            initialized: false,
            is_open: true,
        }
    }

    /// Open the console log, retrying while the shim has yet to create it.
    async fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        match open_console_log(&self.log_path).await {
            Ok(Some(reader)) => {
                self.stdout_reader = Some(reader);
                self.initialized = true;
            }
            Ok(None) => {}
            Err(e) => {
                warn!(
                    "Failed to open console log for container {}: {}",
                    self.container_id, e
                );
                self.initialized = true;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl BidirectionalConsole for ContainerdBidirectionalConsole {
    async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        use tokio::io::AsyncBufReadExt;

        if !self.is_open {
            return Ok(None);
        }

        self.init().await?;

        let Some(reader) = self.stdout_reader.as_mut() else {
            return Ok(None);
        };

        let mut line = String::new();
        match reader.read_line(&mut line).await {
            // Caught up with the container's output for now.
            Ok(0) => {
                if log_shrank(reader).await {
                    self.stdout_reader = None;
                    self.initialized = false;
                }
                Ok(None)
            }
            Ok(_) => Ok(Some(line.into_bytes())),
            Err(e) => Err(NodeError::Internal(format!("Read error: {}", e))),
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        if !self.is_open {
            return Err(NodeError::InvalidInput("Console is closed".to_string()));
        }

        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(data)
            .await
            .map_err(|e| NodeError::Internal(format!("Write error: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| NodeError::Internal(format!("Flush error: {}", e)))?;

        debug!(
            "Wrote {} bytes to container {} stdin",
            data.len(),
            self.container_id
        );
        Ok(())
    }

    async fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        debug!(
            "Resize request for container {}: {}x{}",
            self.container_id, cols, rows
        );

        // Terminal resize requires ioctl on the PTY
        // This is typically handled by containerd's shim process
        // For now, log and acknowledge the resize request
        // Full implementation would require:
        // 1. Getting the PTY master fd from containerd
        // 2. Calling ioctl(fd, TIOCSWINSZ, &winsize)

        info!(
            "Terminal resize requested for container {}: {}x{} (not yet implemented)",
            self.container_id, cols, rows
        );

        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        debug!("Closing console for container {}", self.container_id);
        self.is_open = false;
        // The stdin writer is the runtime's, shared with every other console
        // session; only this session's view of the output is dropped.
        self.stdout_reader = None;
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.is_open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(security: SecurityOptions) -> ContainerSpec {
        ContainerSpec {
            image: "example/game:1".into(),
            command: vec![],
            args: vec!["./game".into(), "-port".into(), "27015".into()],
            env: HashMap::from([("SERVER_PORT".to_string(), "27015".to_string())]),
            working_dir: "/home/container".into(),
            mounts: vec![Mount {
                source: "/data/x".into(),
                target: "/home/container".into(),
                read_only: false,
            }],
            ports: vec![],
            resources: ResourceLimits {
                cpu_shares: 1024,
                cpu_millicores: Some(1500),
                memory_bytes: 1024 * 1024 * 1024,
                memory_swap_bytes: 256 * 1024 * 1024,
                pids_limit: Some(300),
                rlimits: vec![Rlimit {
                    kind: "RLIMIT_NPROC".into(),
                    soft: 512,
                    hard: 512,
                }],
                nofile: 4096,
            },
            security,
            hostname: "nx-abc".into(),
        }
    }

    fn image() -> ImageConfig {
        ImageConfig {
            diff_ids: vec![],
            env: vec!["PATH=/usr/bin".into(), "FOO=bar".into()],
            working_dir: None,
            entrypoint: vec![],
            cmd: vec![],
        }
    }

    #[test]
    fn oci_spec_confines_the_game_process() {
        let security = SecurityOptions {
            uid: 988,
            gid: 988,
            capabilities: vec!["CAP_NET_BIND_SERVICE".into()],
            no_new_privileges: true,
            read_only_root: false,
            seccomp: SeccompProfile::RuntimeDefault,
        };
        let json: serde_json::Value =
            serde_json::from_str(&spec_to_oci(&spec(security), &image()).unwrap()).unwrap();

        let process = &json["process"];
        assert_eq!(process["user"]["uid"], 988);
        assert_eq!(process["noNewPrivileges"], true);
        assert_eq!(
            process["args"],
            serde_json::json!(["./game", "-port", "27015"])
        );
        for set in [
            "bounding",
            "effective",
            "permitted",
            "inheritable",
            "ambient",
        ] {
            assert_eq!(
                process["capabilities"][set],
                serde_json::json!(["CAP_NET_BIND_SERVICE"]),
                "{}",
                set
            );
        }
        let env: Vec<&str> =
            process["env"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(env.contains(&"HOME=/home/container"), "{:?}", env);
        assert!(env.contains(&"SERVER_PORT=27015"));
        assert!(env.contains(&"FOO=bar"), "image env is inherited");

        let rlimits = process["rlimits"].as_array().unwrap();
        assert!(rlimits.iter().any(|r| r["type"] == "RLIMIT_NOFILE"));
        assert!(rlimits.iter().any(|r| r["type"] == "RLIMIT_NPROC" && r["hard"] == 512));

        let linux = &json["linux"];
        assert_eq!(linux["resources"]["cpu"]["quota"], 150_000);
        assert_eq!(linux["resources"]["cpu"]["period"], 100_000);
        assert_eq!(linux["resources"]["memory"]["limit"], 1024 * 1024 * 1024);
        assert_eq!(
            linux["resources"]["memory"]["swap"],
            1024 * 1024 * 1024 + 256 * 1024 * 1024
        );
        assert_eq!(linux["resources"]["pids"]["limit"], 300);
        assert_eq!(linux["seccomp"]["defaultAction"], "SCMP_ACT_ERRNO");
        assert!(linux["seccomp"]["syscalls"].as_array().unwrap().len() > 1);
        assert_eq!(json["hostname"], "nx-abc");
        assert_eq!(json["root"]["readonly"], false);
        // Still the host network namespace: no "network" entry.
        assert!(!linux["namespaces"].as_array().unwrap().iter().any(|n| n["type"] == "network"));
    }

    #[test]
    fn oci_spec_honours_unconfined_and_read_only_root() {
        let security = SecurityOptions {
            uid: 0,
            gid: 0,
            read_only_root: true,
            seccomp: SeccompProfile::Unconfined,
            ..SecurityOptions::default()
        };
        let json: serde_json::Value =
            serde_json::from_str(&spec_to_oci(&spec(security), &image()).unwrap()).unwrap();
        assert!(json["linux"].get("seccomp").is_none());
        assert_eq!(json["root"]["readonly"], true);
        // Root gets no ambient set; the kernel grants it capabilities itself.
        assert!(json["process"]["capabilities"].get("ambient").is_none());
        // A read-only root still has a writable /tmp.
        assert!(json["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["destination"] == "/tmp" && m["type"] == "tmpfs"));
    }

    #[test]
    fn trimming_keeps_the_tail_on_a_line_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.log");
        let mut content = String::new();
        for i in 0..2000 {
            content.push_str(&format!("line {}\n", i));
        }
        std::fs::write(&path, &content).unwrap();
        let len = content.len() as u64;

        // Under the cap: untouched.
        trim_log_file(&path, len + 1, 100).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), len);

        // Over the cap: only the tail survives, starting on a whole line.
        trim_log_file(&path, 1000, 200).unwrap();
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(kept.len() <= 200);
        assert!(kept.starts_with("line "));
        assert!(kept.ends_with("line 1999\n"));

        // A missing log is not an error.
        trim_log_file(&dir.path().join("missing.log"), 10, 5).unwrap();
    }
}
