//! OCI image metadata helpers.
//!
//! Starting a container from an image needs two things that live in the image's
//! own metadata rather than in containerd's image record:
//!
//! * the **chain ID** of the unpacked root filesystem, which is the parent a
//!   new snapshot has to be prepared from, and
//! * the image's default process configuration (`Env`, `WorkingDir`,
//!   `Entrypoint`, `Cmd`), which a blueprint only partially overrides.
//!
//! Both are derived by walking the image's content: manifest list → manifest →
//! config. The functions here do the parsing and the arithmetic; fetching the
//! blobs from containerd's content store is the caller's job, which keeps this
//! module free of I/O and unit-testable.

use crate::error::{NodeError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Media types that hold a list of per-platform manifests.
const INDEX_MEDIA_TYPES: &[&str] = &[
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

/// Media types that hold a single platform's manifest.
const MANIFEST_MEDIA_TYPES: &[&str] = &[
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// The parts of an image's OCI config that shape the container process.
#[derive(Debug, Clone, Default)]
pub struct ImageConfig {
    /// Uncompressed layer digests, oldest first. Used to derive the chain ID.
    pub diff_ids: Vec<String>,
    /// Image environment, in `KEY=VALUE` form.
    pub env: Vec<String>,
    /// Image working directory, if it declares one.
    pub working_dir: Option<String>,
    /// Image entrypoint.
    pub entrypoint: Vec<String>,
    /// Image default command.
    pub cmd: Vec<String>,
}

impl ImageConfig {
    /// Chain ID of the fully unpacked root filesystem.
    ///
    /// This is the snapshot key containerd committed when it unpacked the
    /// image, and therefore the parent for a container's active snapshot.
    pub fn chain_id(&self) -> Option<String> {
        chain_id(&self.diff_ids)
    }
}

/// A content descriptor: a pointer to a blob in the content store.
#[derive(Debug, Clone, Deserialize)]
pub struct Descriptor {
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    pub digest: String,
    #[serde(default)]
    pub platform: Option<Platform>,
}

/// The platform a manifest in an index was built for.
#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub variant: Option<String>,
}

/// Whether `media_type` is a multi-platform index/manifest list.
pub fn is_index(media_type: &str) -> bool {
    INDEX_MEDIA_TYPES.contains(&media_type)
}

/// Whether `media_type` is a single-platform manifest.
pub fn is_manifest(media_type: &str) -> bool {
    MANIFEST_MEDIA_TYPES.contains(&media_type)
}

/// The OCI architecture name (and variant) for the host we are running on.
///
/// OCI uses Go's `GOARCH` names, which differ from Rust's target arch names.
pub fn host_architecture() -> (&'static str, Option<&'static str>) {
    match std::env::consts::ARCH {
        "x86_64" => ("amd64", None),
        "x86" => ("386", None),
        "aarch64" => ("arm64", Some("v8")),
        "arm" => ("arm", Some("v7")),
        "powerpc64" => ("ppc64le", None),
        "s390x" => ("s390x", None),
        "riscv64" => ("riscv64", None),
        other => (other, None),
    }
}

/// Pick the manifest matching `arch` out of an image index.
///
/// Attestation entries (which carry `os: unknown`) are skipped, and an exact
/// variant match wins over a bare architecture match — otherwise an arm64/v8
/// host could be handed an arm64/v9 manifest that its snapshots cannot run.
pub fn select_manifest(index_json: &[u8], arch: &str, variant: Option<&str>) -> Result<Descriptor> {
    #[derive(Deserialize)]
    struct Index {
        #[serde(default)]
        manifests: Vec<Descriptor>,
    }

    let index: Index = serde_json::from_slice(index_json)
        .map_err(|e| NodeError::ContainerdError(format!("Invalid image index: {}", e)))?;

    let candidates: Vec<&Descriptor> = index
        .manifests
        .iter()
        .filter(|d| match &d.platform {
            Some(p) => p.os == "linux" && p.architecture == arch,
            // A manifest without platform info in an index is ambiguous, but
            // some registries omit it for single-platform images.
            None => true,
        })
        .collect();

    let exact = candidates.iter().find(|d| match (&d.platform, variant) {
        (Some(p), Some(want)) => p.variant.as_deref() == Some(want),
        _ => false,
    });

    exact.or(candidates.first()).map(|d| (*d).clone()).ok_or_else(|| {
        NodeError::ContainerdError(format!(
            "Image has no linux/{} manifest — it cannot run on this node",
            arch
        ))
    })
}

/// Extract the config descriptor from a single-platform manifest.
pub fn config_descriptor(manifest_json: &[u8]) -> Result<Descriptor> {
    #[derive(Deserialize)]
    struct Manifest {
        config: Descriptor,
    }

    let manifest: Manifest = serde_json::from_slice(manifest_json)
        .map_err(|e| NodeError::ContainerdError(format!("Invalid image manifest: {}", e)))?;
    Ok(manifest.config)
}

/// Parse an image config blob into the fields we act on.
pub fn parse_config(config_json: &[u8]) -> Result<ImageConfig> {
    #[derive(Deserialize)]
    struct RootFs {
        #[serde(rename = "diff_ids", default)]
        diff_ids: Vec<String>,
    }

    #[derive(Deserialize, Default)]
    struct Config {
        #[serde(rename = "Env", default)]
        env: Vec<String>,
        #[serde(rename = "WorkingDir", default)]
        working_dir: String,
        #[serde(rename = "Entrypoint", default)]
        entrypoint: Vec<String>,
        #[serde(rename = "Cmd", default)]
        cmd: Vec<String>,
    }

    #[derive(Deserialize)]
    struct ImageJson {
        rootfs: RootFs,
        #[serde(default)]
        config: Config,
    }

    let image: ImageJson = serde_json::from_slice(config_json)
        .map_err(|e| NodeError::ContainerdError(format!("Invalid image config: {}", e)))?;

    Ok(ImageConfig {
        diff_ids: image.rootfs.diff_ids,
        env: image.config.env,
        working_dir: Some(image.config.working_dir).filter(|d| !d.is_empty()),
        entrypoint: image.config.entrypoint,
        cmd: image.config.cmd,
    })
}

/// Fold a list of layer diff IDs into the chain ID of the topmost layer.
///
/// Defined by the OCI image spec: each step hashes the literal bytes
/// `"<parent chain id> <diff id>"`. containerd names committed snapshots by
/// this value, so it has to match byte for byte or the rootfs will not be
/// found.
pub fn chain_id(diff_ids: &[String]) -> Option<String> {
    let mut iter = diff_ids.iter();
    let mut chain = iter.next()?.clone();
    for diff_id in iter {
        let mut hasher = Sha256::new();
        hasher.update(chain.as_bytes());
        hasher.update(b" ");
        hasher.update(diff_id.as_bytes());
        chain = format!("sha256:{:x}", hasher.finalize());
    }
    Some(chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single-layer image's chain ID is just its diff ID.
    #[test]
    fn chain_id_of_one_layer_is_the_layer() {
        let ids = vec!["sha256:aaaa".to_string()];
        assert_eq!(chain_id(&ids), Some("sha256:aaaa".to_string()));
    }

    /// The fold has to hash `"<parent> <child>"` exactly — a different
    /// separator or ordering silently yields a snapshot key that does not
    /// exist, which surfaces much later as an unstartable container.
    #[test]
    fn chain_id_folds_layers_in_order() {
        let ids = vec![
            "sha256:aaaa".to_string(),
            "sha256:bbbb".to_string(),
            "sha256:cccc".to_string(),
        ];

        // sha256("sha256:aaaa sha256:bbbb"), then folded with sha256:cccc.
        let first = {
            let mut h = Sha256::new();
            h.update(b"sha256:aaaa sha256:bbbb");
            format!("sha256:{:x}", h.finalize())
        };
        let expected = {
            let mut h = Sha256::new();
            h.update(format!("{} sha256:cccc", first).as_bytes());
            format!("sha256:{:x}", h.finalize())
        };

        assert_eq!(chain_id(&ids), Some(expected));
        assert_eq!(chain_id(&[]), None);
    }

    /// Known-good vector: the two-layer fold above, pinned to a literal so a
    /// refactor of the hashing cannot quietly change the result.
    #[test]
    fn chain_id_matches_known_vector() {
        let ids = vec!["sha256:aaaa".to_string(), "sha256:bbbb".to_string()];
        assert_eq!(
            chain_id(&ids),
            Some(
                "sha256:2c4d8760379f05d0ad1ed610712309f0114e7345e6e6a351494cff030ffdf197"
                    .to_string()
            )
        );
    }

    #[test]
    fn selects_the_matching_platform_from_an_index() {
        let index = br#"{
            "manifests": [
                {"mediaType":"m","digest":"sha256:arm","platform":{"architecture":"arm64","os":"linux"}},
                {"mediaType":"m","digest":"sha256:amd","platform":{"architecture":"amd64","os":"linux"}},
                {"mediaType":"m","digest":"sha256:att","platform":{"architecture":"unknown","os":"unknown"}}
            ]
        }"#;

        let picked = select_manifest(index, "amd64", None).unwrap();
        assert_eq!(picked.digest, "sha256:amd");
    }

    #[test]
    fn prefers_an_exact_variant_match() {
        let index = br#"{
            "manifests": [
                {"mediaType":"m","digest":"sha256:v9","platform":{"architecture":"arm64","os":"linux","variant":"v9"}},
                {"mediaType":"m","digest":"sha256:v8","platform":{"architecture":"arm64","os":"linux","variant":"v8"}}
            ]
        }"#;

        assert_eq!(
            select_manifest(index, "arm64", Some("v8")).unwrap().digest,
            "sha256:v8"
        );
        // With no variant preference the first match is fine.
        assert_eq!(
            select_manifest(index, "arm64", None).unwrap().digest,
            "sha256:v9"
        );
    }

    /// An image built only for another architecture must fail loudly here
    /// rather than at task start with an opaque runc error.
    #[test]
    fn rejects_an_index_without_this_architecture() {
        let index = br#"{
            "manifests": [
                {"mediaType":"m","digest":"sha256:arm","platform":{"architecture":"arm64","os":"linux"}}
            ]
        }"#;

        let err = select_manifest(index, "amd64", None).unwrap_err().to_string();
        assert!(err.contains("linux/amd64"), "unexpected error: {}", err);
    }

    #[test]
    fn parses_the_process_fields_of_an_image_config() {
        let config = br#"{
            "config": {
                "Env": ["PATH=/usr/bin", "HOME=/home/container"],
                "WorkingDir": "/home/container",
                "Entrypoint": ["/entrypoint.sh"],
                "Cmd": ["bash"]
            },
            "rootfs": {"type": "layers", "diff_ids": ["sha256:aaaa", "sha256:bbbb"]}
        }"#;

        let parsed = parse_config(config).unwrap();
        assert_eq!(parsed.diff_ids.len(), 2);
        assert_eq!(parsed.env[0], "PATH=/usr/bin");
        assert_eq!(parsed.working_dir.as_deref(), Some("/home/container"));
        assert_eq!(parsed.entrypoint, vec!["/entrypoint.sh".to_string()]);
        assert_eq!(parsed.cmd, vec!["bash".to_string()]);
        assert!(parsed.chain_id().is_some());
    }

    /// A config with no `config` block at all still has to yield its layers.
    #[test]
    fn parses_a_config_without_a_process_block() {
        let config = br#"{"rootfs": {"type": "layers", "diff_ids": ["sha256:aaaa"]}}"#;
        let parsed = parse_config(config).unwrap();
        assert_eq!(parsed.chain_id(), Some("sha256:aaaa".to_string()));
        assert!(parsed.env.is_empty());
        assert!(parsed.working_dir.is_none());
    }

    #[test]
    fn media_type_classification() {
        assert!(is_index("application/vnd.oci.image.index.v1+json"));
        assert!(is_index(
            "application/vnd.docker.distribution.manifest.list.v2+json"
        ));
        assert!(is_manifest("application/vnd.oci.image.manifest.v1+json"));
        assert!(!is_manifest("application/vnd.oci.image.index.v1+json"));
    }
}
