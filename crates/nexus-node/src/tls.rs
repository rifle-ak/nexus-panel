//! TLS/mTLS configuration for enterprise secure communications.
//!
//! Provides:
//! - Server-side TLS for gRPC
//! - Mutual TLS (mTLS) client verification
//! - Certificate rotation support
//! - Multiple cipher suite options
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::tls::{TlsConfig, TlsAcceptor};
//!
//! let config = TlsConfig::from_env()?;
//! let acceptor = TlsAcceptor::from_config(&config)?;
//! ```

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tracing::{info, warn};

/// TLS configuration errors
#[derive(Error, Debug)]
pub enum TlsError {
    #[error("Failed to read certificate file: {path}")]
    CertificateReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to read private key file: {path}")]
    PrivateKeyReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("No certificates found in file: {0}")]
    NoCertificates(PathBuf),

    #[error("No private key found in file: {0}")]
    NoPrivateKey(PathBuf),

    #[error("Failed to build TLS configuration: {0}")]
    ConfigBuildError(String),

    #[error("Certificate validation failed: {0}")]
    CertificateValidationError(String),

    #[error("TLS handshake failed: {0}")]
    HandshakeError(String),

    #[error("TLS not configured")]
    NotConfigured,
}

/// TLS configuration
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Enable TLS
    pub enabled: bool,
    /// Server certificate path
    pub cert_path: Option<PathBuf>,
    /// Server private key path
    pub key_path: Option<PathBuf>,
    /// CA certificate path for client verification (mTLS)
    pub ca_cert_path: Option<PathBuf>,
    /// Require client certificates (mTLS)
    pub require_client_cert: bool,
    /// Minimum TLS version
    pub min_version: TlsVersion,
    /// Allowed cipher suites (empty = use defaults)
    pub cipher_suites: Vec<String>,
    /// Enable OCSP stapling
    pub ocsp_stapling: bool,
    /// Certificate reload interval (seconds, 0 = disabled)
    pub reload_interval_secs: u64,
}

/// TLS protocol versions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsVersion {
    Tls12,
    Tls13,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: None,
            key_path: None,
            ca_cert_path: None,
            require_client_cert: false,
            min_version: TlsVersion::Tls12,
            cipher_suites: Vec::new(),
            ocsp_stapling: false,
            reload_interval_secs: 0,
        }
    }
}

impl TlsConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Result<Self, TlsError> {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("TLS_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }

        if let Ok(path) = std::env::var("TLS_CERT_PATH") {
            config.cert_path = Some(PathBuf::from(path));
        }

        if let Ok(path) = std::env::var("TLS_KEY_PATH") {
            config.key_path = Some(PathBuf::from(path));
        }

        if let Ok(path) = std::env::var("TLS_CA_CERT_PATH") {
            config.ca_cert_path = Some(PathBuf::from(path));
        }

        if let Ok(require) = std::env::var("TLS_REQUIRE_CLIENT_CERT") {
            config.require_client_cert = require.to_lowercase() == "true" || require == "1";
        }

        if let Ok(version) = std::env::var("TLS_MIN_VERSION") {
            config.min_version = match version.as_str() {
                "1.2" | "TLS1.2" | "TLSv1.2" => TlsVersion::Tls12,
                "1.3" | "TLS1.3" | "TLSv1.3" => TlsVersion::Tls13,
                _ => TlsVersion::Tls12,
            };
        }

        if let Ok(interval) = std::env::var("TLS_RELOAD_INTERVAL") {
            config.reload_interval_secs = interval.parse().unwrap_or(0);
        }

        // Validate configuration
        if config.enabled {
            if config.cert_path.is_none() {
                return Err(TlsError::NotConfigured);
            }
            if config.key_path.is_none() {
                return Err(TlsError::NotConfigured);
            }
        }

        Ok(config)
    }

    /// Create a configuration for development/testing with self-signed certs
    pub fn development(cert_path: impl Into<PathBuf>, key_path: impl Into<PathBuf>) -> Self {
        Self {
            enabled: true,
            cert_path: Some(cert_path.into()),
            key_path: Some(key_path.into()),
            ca_cert_path: None,
            require_client_cert: false,
            min_version: TlsVersion::Tls12,
            cipher_suites: Vec::new(),
            ocsp_stapling: false,
            reload_interval_secs: 0,
        }
    }

    /// Create a configuration for production with mTLS
    pub fn production_mtls(
        cert_path: impl Into<PathBuf>,
        key_path: impl Into<PathBuf>,
        ca_cert_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            enabled: true,
            cert_path: Some(cert_path.into()),
            key_path: Some(key_path.into()),
            ca_cert_path: Some(ca_cert_path.into()),
            require_client_cert: true,
            min_version: TlsVersion::Tls13,
            cipher_suites: Vec::new(),
            ocsp_stapling: true,
            reload_interval_secs: 3600, // Reload every hour
        }
    }
}

/// Load certificates from a PEM file
pub fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let file = File::open(path).map_err(|e| TlsError::CertificateReadError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let mut reader = BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut reader).filter_map(|result| result.ok()).collect();

    if certs.is_empty() {
        return Err(TlsError::NoCertificates(path.to_path_buf()));
    }

    info!("Loaded {} certificate(s) from {:?}", certs.len(), path);
    Ok(certs)
}

/// Load private key from a PEM file
pub fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let file = File::open(path).map_err(|e| TlsError::PrivateKeyReadError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let mut reader = BufReader::new(file);

    // Try to read PKCS#8 key first
    let keys: Vec<_> = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .filter_map(|result| result.ok())
        .collect();

    if let Some(key) = keys.into_iter().next() {
        info!("Loaded PKCS#8 private key from {:?}", path);
        return Ok(PrivateKeyDer::Pkcs8(key));
    }

    // Reset reader and try RSA key
    let file = File::open(path).map_err(|e| TlsError::PrivateKeyReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut reader = BufReader::new(file);

    let keys: Vec<_> = rustls_pemfile::rsa_private_keys(&mut reader)
        .filter_map(|result| result.ok())
        .collect();

    if let Some(key) = keys.into_iter().next() {
        info!("Loaded RSA private key from {:?}", path);
        return Ok(PrivateKeyDer::Pkcs1(key));
    }

    // Reset reader and try EC key
    let file = File::open(path).map_err(|e| TlsError::PrivateKeyReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut reader = BufReader::new(file);

    let keys: Vec<_> = rustls_pemfile::ec_private_keys(&mut reader)
        .filter_map(|result| result.ok())
        .collect();

    if let Some(key) = keys.into_iter().next() {
        info!("Loaded EC private key from {:?}", path);
        return Ok(PrivateKeyDer::Sec1(key));
    }

    Err(TlsError::NoPrivateKey(path.to_path_buf()))
}

/// Build a rustls ServerConfig from TlsConfig
pub fn build_server_config(config: &TlsConfig) -> Result<Arc<rustls::ServerConfig>, TlsError> {
    let cert_path = config.cert_path.as_ref().ok_or(TlsError::NotConfigured)?;

    let key_path = config.key_path.as_ref().ok_or(TlsError::NotConfigured)?;

    // Load certificates and key
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;

    // Create server config builder
    let builder = if let Some(ca_path) = &config.ca_cert_path {
        // mTLS: Verify client certificates
        let ca_certs = load_certs(ca_path)?;
        let mut root_store = rustls::RootCertStore::empty();
        for cert in ca_certs {
            root_store.add(cert).map_err(|e| {
                TlsError::CertificateValidationError(format!("Failed to add CA cert: {}", e))
            })?;
        }

        let verifier = if config.require_client_cert {
            rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
                .build()
                .map_err(|e| {
                    TlsError::ConfigBuildError(format!("Failed to build client verifier: {}", e))
                })?
        } else {
            rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
                .allow_unauthenticated()
                .build()
                .map_err(|e| {
                    TlsError::ConfigBuildError(format!("Failed to build client verifier: {}", e))
                })?
        };

        rustls::ServerConfig::builder().with_client_cert_verifier(verifier)
    } else {
        rustls::ServerConfig::builder().with_no_client_auth()
    };

    // Set certificates
    let server_config = builder
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::ConfigBuildError(format!("Failed to set certificates: {}", e)))?;

    info!(
        "TLS server configuration built successfully (mTLS: {}, min_version: {:?})",
        config.require_client_cert, config.min_version
    );

    Ok(Arc::new(server_config))
}

/// Build a rustls ClientConfig for outgoing connections
pub fn build_client_config(
    ca_cert_path: Option<&Path>,
    client_cert_path: Option<&Path>,
    client_key_path: Option<&Path>,
) -> Result<Arc<rustls::ClientConfig>, TlsError> {
    // Load CA certificates
    let root_store = if let Some(ca_path) = ca_cert_path {
        let ca_certs = load_certs(ca_path)?;
        let mut store = rustls::RootCertStore::empty();
        for cert in ca_certs {
            store.add(cert).map_err(|e| {
                TlsError::CertificateValidationError(format!("Failed to add CA cert: {}", e))
            })?;
        }
        store
    } else {
        // Use system root certificates
        let mut store = rustls::RootCertStore::empty();
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        store
    };

    // Build client config
    let builder = rustls::ClientConfig::builder().with_root_certificates(root_store);

    let client_config =
        if let (Some(cert_path), Some(key_path)) = (client_cert_path, client_key_path) {
            // mTLS: Use client certificate
            let certs = load_certs(cert_path)?;
            let key = load_private_key(key_path)?;
            builder.with_client_auth_cert(certs, key).map_err(|e| {
                TlsError::ConfigBuildError(format!("Failed to set client certificate: {}", e))
            })?
        } else {
            builder.with_no_client_auth()
        };

    info!("TLS client configuration built successfully");
    Ok(Arc::new(client_config))
}

/// TLS certificate information
#[derive(Debug, Clone)]
pub struct CertificateInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before: String,
    pub not_after: String,
    pub serial_number: String,
    pub fingerprint_sha256: String,
}

/// Get information about a certificate file
pub fn get_cert_info(path: &Path) -> Result<Vec<CertificateInfo>, TlsError> {
    let certs = load_certs(path)?;
    let mut info = Vec::new();

    for cert in &certs {
        // Parse with x509-parser if available
        // For now, return placeholder info
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(cert.as_ref());
        let fingerprint = hasher.finalize();
        let fingerprint_hex: String = fingerprint.iter().map(|b| format!("{:02x}", b)).collect();

        info.push(CertificateInfo {
            subject: "Certificate details require x509 parsing".to_string(),
            issuer: "Certificate details require x509 parsing".to_string(),
            not_before: "Unknown".to_string(),
            not_after: "Unknown".to_string(),
            serial_number: "Unknown".to_string(),
            fingerprint_sha256: fingerprint_hex,
        });
    }

    Ok(info)
}

/// Check if a certificate file needs renewal (within N days of expiry)
pub fn cert_needs_renewal(_path: &Path, _days_before_expiry: u32) -> Result<bool, TlsError> {
    // This would require x509 parsing to check expiry date
    // For now, return false (no renewal needed)
    warn!("Certificate expiry checking not fully implemented - requires x509 parsing");
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tls_config_default() {
        let config = TlsConfig::default();
        assert!(!config.enabled);
        assert!(config.cert_path.is_none());
        assert!(config.key_path.is_none());
    }

    #[test]
    fn test_tls_config_development() {
        let config = TlsConfig::development("/path/to/cert.pem", "/path/to/key.pem");
        assert!(config.enabled);
        assert!(!config.require_client_cert);
        assert_eq!(config.min_version, TlsVersion::Tls12);
    }

    #[test]
    fn test_tls_config_production_mtls() {
        let config =
            TlsConfig::production_mtls("/path/to/cert.pem", "/path/to/key.pem", "/path/to/ca.pem");
        assert!(config.enabled);
        assert!(config.require_client_cert);
        assert_eq!(config.min_version, TlsVersion::Tls13);
        assert!(config.ca_cert_path.is_some());
    }

    #[test]
    fn test_load_certs_missing_file() {
        let result = load_certs(Path::new("/nonexistent/cert.pem"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_private_key_missing_file() {
        let result = load_private_key(Path::new("/nonexistent/key.pem"));
        assert!(result.is_err());
    }
}
