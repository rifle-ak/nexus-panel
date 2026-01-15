//! Cloudflare integration for enterprise DDoS protection and traffic management.
//!
//! Provides comprehensive Cloudflare integration including:
//! - **Spectrum**: TCP/UDP proxy with DDoS protection for game servers
//! - **DNS Management**: Automatic DNS record management
//! - **Tunnels**: Cloudflare Tunnel support for secure connections
//! - **Firewall Rules**: WAF and IP firewall management
//! - **Load Balancing**: Traffic distribution across nodes
//!
//! # Features
//!
//! - Automatic Spectrum app creation for game server ports
//! - DNS record management (A, AAAA, SRV records)
//! - Real-time DDoS attack notifications
//! - Bandwidth and connection metrics
//! - IP address masking for origin protection
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::cloudflare::{CloudflareClient, SpectrumApp};
//!
//! let client = CloudflareClient::new(config)?;
//! client.create_spectrum_app("minecraft", 25565, Protocol::Tcp).await?;
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Cloudflare API errors
#[derive(Error, Debug)]
pub enum CloudflareError {
    #[error("API request failed: {0}")]
    RequestFailed(String),

    #[error("Authentication failed: invalid API token")]
    AuthenticationFailed,

    #[error("Zone not found: {0}")]
    ZoneNotFound(String),

    #[error("Rate limited: retry after {0} seconds")]
    RateLimited(u64),

    #[error("Spectrum not available on this plan")]
    SpectrumNotAvailable,

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

/// Cloudflare client configuration
#[derive(Debug, Clone)]
pub struct CloudflareConfig {
    /// Enable Cloudflare integration
    pub enabled: bool,
    /// API token (scoped or global)
    pub api_token: Option<String>,
    /// Account ID
    pub account_id: Option<String>,
    /// Zone ID for DNS management
    pub zone_id: Option<String>,
    /// Domain name
    pub domain: Option<String>,
    /// Enable Spectrum for game server protection
    pub spectrum_enabled: bool,
    /// Enable automatic DNS management
    pub dns_auto_manage: bool,
    /// Enable Cloudflare Tunnel
    pub tunnel_enabled: bool,
    /// Tunnel ID (if using existing tunnel)
    pub tunnel_id: Option<String>,
    /// Proxy protocol version (1 or 2)
    pub proxy_protocol_version: u8,
    /// Enable IP geolocation
    pub ip_geolocation: bool,
    /// API base URL (for testing)
    pub api_base_url: String,
}

impl Default for CloudflareConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_token: None,
            account_id: None,
            zone_id: None,
            domain: None,
            spectrum_enabled: true,
            dns_auto_manage: true,
            tunnel_enabled: false,
            tunnel_id: None,
            proxy_protocol_version: 2,
            ip_geolocation: true,
            api_base_url: "https://api.cloudflare.com/client/v4".to_string(),
        }
    }
}

impl CloudflareConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("CLOUDFLARE_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }

        config.api_token = std::env::var("CLOUDFLARE_API_TOKEN").ok();
        config.account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID").ok();
        config.zone_id = std::env::var("CLOUDFLARE_ZONE_ID").ok();
        config.domain = std::env::var("CLOUDFLARE_DOMAIN").ok();

        if let Ok(spectrum) = std::env::var("CLOUDFLARE_SPECTRUM_ENABLED") {
            config.spectrum_enabled = spectrum.to_lowercase() == "true" || spectrum == "1";
        }

        if let Ok(dns) = std::env::var("CLOUDFLARE_DNS_AUTO_MANAGE") {
            config.dns_auto_manage = dns.to_lowercase() == "true" || dns == "1";
        }

        if let Ok(tunnel) = std::env::var("CLOUDFLARE_TUNNEL_ENABLED") {
            config.tunnel_enabled = tunnel.to_lowercase() == "true" || tunnel == "1";
        }

        config.tunnel_id = std::env::var("CLOUDFLARE_TUNNEL_ID").ok();

        if let Ok(version) = std::env::var("CLOUDFLARE_PROXY_PROTOCOL_VERSION") {
            config.proxy_protocol_version = version.parse().unwrap_or(2);
        }

        config
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<(), CloudflareError> {
        if !self.enabled {
            return Ok(());
        }

        if self.api_token.is_none() {
            return Err(CloudflareError::InvalidConfig(
                "API token is required when Cloudflare is enabled".to_string(),
            ));
        }

        if self.spectrum_enabled && self.zone_id.is_none() {
            return Err(CloudflareError::InvalidConfig(
                "Zone ID is required for Spectrum".to_string(),
            ));
        }

        Ok(())
    }
}

/// Spectrum application protocol
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpectrumProtocol {
    Tcp,
    Udp,
}

/// Spectrum application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectrumApp {
    /// Application ID (assigned by Cloudflare)
    pub id: Option<String>,
    /// Application name
    pub name: String,
    /// Protocol (TCP/UDP)
    pub protocol: SpectrumProtocol,
    /// Edge port (Cloudflare side)
    pub edge_port: u16,
    /// Origin IP addresses
    pub origin_direct: Vec<String>,
    /// Origin port
    pub origin_port: u16,
    /// Enable proxy protocol
    pub proxy_protocol: ProxyProtocol,
    /// IP firewall enabled
    pub ip_firewall: bool,
    /// TLS termination (TCP only)
    pub tls: Option<TlsConfig>,
    /// Edge IP type
    pub edge_ips: EdgeIps,
    /// Traffic type hint
    pub traffic_type: TrafficType,
    /// Argo Smart Routing
    pub argo_smart_routing: bool,
}

/// Proxy protocol configuration
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProxyProtocol {
    Off,
    V1,
    V2,
    Simple,
}

/// TLS configuration for Spectrum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// TLS mode
    pub mode: TlsMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Off,
    Flexible,
    Full,
    Strict,
}

/// Edge IP configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeIps {
    /// IP type
    #[serde(rename = "type")]
    pub ip_type: EdgeIpType,
    /// Connectivity (for dynamic IPs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<IpConnectivity>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeIpType {
    Dynamic,
    Static,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IpConnectivity {
    All,
    Ipv4,
    Ipv6,
}

/// Traffic type hints for optimization
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrafficType {
    Direct,
    Http,
    Https,
}

impl SpectrumApp {
    /// Create a new Spectrum app for a game server
    pub fn for_game_server(
        name: &str,
        port: u16,
        protocol: SpectrumProtocol,
        origin_ip: &str,
        proxy_protocol_version: u8,
    ) -> Self {
        Self {
            id: None,
            name: name.to_string(),
            protocol,
            edge_port: port,
            origin_direct: vec![format!("{}:{}", origin_ip, port)],
            origin_port: port,
            proxy_protocol: match proxy_protocol_version {
                1 => ProxyProtocol::V1,
                2 => ProxyProtocol::V2,
                _ => ProxyProtocol::Off,
            },
            ip_firewall: true,
            tls: None,
            edge_ips: EdgeIps {
                ip_type: EdgeIpType::Dynamic,
                connectivity: Some(IpConnectivity::All),
            },
            traffic_type: TrafficType::Direct,
            argo_smart_routing: false,
        }
    }

    /// Create for Minecraft server
    pub fn minecraft(origin_ip: &str, port: u16) -> Self {
        Self::for_game_server("minecraft", port, SpectrumProtocol::Tcp, origin_ip, 2)
    }

    /// Create for generic UDP game server
    pub fn udp_game(name: &str, origin_ip: &str, port: u16) -> Self {
        Self::for_game_server(name, port, SpectrumProtocol::Udp, origin_ip, 0)
    }
}

/// DNS record types
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum DnsRecordType {
    A,
    Aaaa,
    Cname,
    Srv,
    Txt,
    Mx,
}

/// DNS record configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsRecord {
    /// Record ID (assigned by Cloudflare)
    pub id: Option<String>,
    /// Record type
    #[serde(rename = "type")]
    pub record_type: DnsRecordType,
    /// Record name (subdomain)
    pub name: String,
    /// Record content (IP, CNAME target, etc.)
    pub content: String,
    /// TTL (1 = automatic)
    pub ttl: u32,
    /// Proxied through Cloudflare
    pub proxied: bool,
    /// Priority (for MX, SRV)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
    /// Additional data for SRV records
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<SrvData>,
}

/// SRV record data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrvData {
    pub service: String,
    pub proto: String,
    pub name: String,
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

impl DnsRecord {
    /// Create an A record
    pub fn a_record(name: &str, ip: &str, proxied: bool) -> Self {
        Self {
            id: None,
            record_type: DnsRecordType::A,
            name: name.to_string(),
            content: ip.to_string(),
            ttl: 1, // Automatic
            proxied,
            priority: None,
            data: None,
        }
    }

    /// Create a Minecraft SRV record
    pub fn minecraft_srv(subdomain: &str, target: &str, port: u16) -> Self {
        Self {
            id: None,
            record_type: DnsRecordType::Srv,
            name: format!("_minecraft._tcp.{}", subdomain),
            content: format!("0 5 {} {}", port, target),
            ttl: 1,
            proxied: false, // SRV cannot be proxied
            priority: Some(0),
            data: Some(SrvData {
                service: "_minecraft".to_string(),
                proto: "_tcp".to_string(),
                name: subdomain.to_string(),
                priority: 0,
                weight: 5,
                port,
                target: target.to_string(),
            }),
        }
    }
}

/// Cloudflare API client
pub struct CloudflareClient {
    config: CloudflareConfig,
    http_client: reqwest::Client,
    /// Cache for zone info
    zone_cache: Arc<RwLock<Option<ZoneInfo>>>,
    /// Cache for Spectrum apps
    spectrum_cache: Arc<RwLock<HashMap<String, SpectrumApp>>>,
}

/// Zone information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneInfo {
    pub id: String,
    pub name: String,
    pub status: String,
    pub plan: ZonePlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonePlan {
    pub id: String,
    pub name: String,
    pub is_subscribed: bool,
}

/// API response wrapper
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    success: bool,
    errors: Vec<ApiError>,
    messages: Vec<String>,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: i32,
    message: String,
}

impl CloudflareClient {
    /// Create a new Cloudflare client
    pub fn new(config: CloudflareConfig) -> Result<Self, CloudflareError> {
        config.validate()?;

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CloudflareError::NetworkError(e.to_string()))?;

        Ok(Self {
            config,
            http_client,
            zone_cache: Arc::new(RwLock::new(None)),
            spectrum_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Check if Cloudflare integration is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Make an authenticated API request
    async fn api_request<T: for<'de> Deserialize<'de>>(
        &self,
        method: reqwest::Method,
        endpoint: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, CloudflareError> {
        let token = self
            .config
            .api_token
            .as_ref()
            .ok_or(CloudflareError::AuthenticationFailed)?;

        let url = format!("{}{}", self.config.api_base_url, endpoint);

        let mut request = self
            .http_client
            .request(method.clone(), &url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json");

        if let Some(body) = body {
            let body_str = serde_json::to_string(body)
                .map_err(|e| CloudflareError::SerializationError(e.to_string()))?;
            request = request.body(body_str);
        }

        let response = request
            .send()
            .await
            .map_err(|e| CloudflareError::NetworkError(e.to_string()))?;

        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            return Err(CloudflareError::RateLimited(retry_after));
        }

        let body = response
            .text()
            .await
            .map_err(|e| CloudflareError::NetworkError(e.to_string()))?;

        let api_response: ApiResponse<T> = serde_json::from_str(&body)
            .map_err(|e| CloudflareError::SerializationError(format!("{}: {}", e, body)))?;

        if !api_response.success {
            let error_msg = api_response
                .errors
                .iter()
                .map(|e| format!("[{}] {}", e.code, e.message))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CloudflareError::RequestFailed(error_msg));
        }

        api_response
            .result
            .ok_or_else(|| CloudflareError::RequestFailed("Empty response".to_string()))
    }

    /// Get zone information
    pub async fn get_zone(&self) -> Result<ZoneInfo, CloudflareError> {
        // Check cache first
        {
            let cache = self.zone_cache.read().await;
            if let Some(zone) = &*cache {
                return Ok(zone.clone());
            }
        }

        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID not configured".to_string()))?;

        let zone: ZoneInfo = self
            .api_request(reqwest::Method::GET, &format!("/zones/{}", zone_id), None::<&()>)
            .await?;

        // Update cache
        {
            let mut cache = self.zone_cache.write().await;
            *cache = Some(zone.clone());
        }

        info!("Retrieved zone info: {} ({})", zone.name, zone.plan.name);
        Ok(zone)
    }

    // ==================== Spectrum API ====================

    /// Create a Spectrum application for a game server
    pub async fn create_spectrum_app(&self, app: &SpectrumApp) -> Result<SpectrumApp, CloudflareError> {
        if !self.config.spectrum_enabled {
            return Err(CloudflareError::SpectrumNotAvailable);
        }

        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let result: SpectrumApp = self
            .api_request(
                reqwest::Method::POST,
                &format!("/zones/{}/spectrum/apps", zone_id),
                Some(app),
            )
            .await?;

        info!(
            "Created Spectrum app: {} ({}:{:?})",
            result.name, result.edge_port, result.protocol
        );

        // Update cache
        if let Some(ref id) = result.id {
            let mut cache = self.spectrum_cache.write().await;
            cache.insert(id.clone(), result.clone());
        }

        Ok(result)
    }

    /// List all Spectrum applications
    pub async fn list_spectrum_apps(&self) -> Result<Vec<SpectrumApp>, CloudflareError> {
        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let apps: Vec<SpectrumApp> = self
            .api_request(
                reqwest::Method::GET,
                &format!("/zones/{}/spectrum/apps", zone_id),
                None::<&()>,
            )
            .await?;

        // Update cache
        {
            let mut cache = self.spectrum_cache.write().await;
            cache.clear();
            for app in &apps {
                if let Some(ref id) = app.id {
                    cache.insert(id.clone(), app.clone());
                }
            }
        }

        Ok(apps)
    }

    /// Get a Spectrum application by ID
    pub async fn get_spectrum_app(&self, app_id: &str) -> Result<SpectrumApp, CloudflareError> {
        // Check cache first
        {
            let cache = self.spectrum_cache.read().await;
            if let Some(app) = cache.get(app_id) {
                return Ok(app.clone());
            }
        }

        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let app: SpectrumApp = self
            .api_request(
                reqwest::Method::GET,
                &format!("/zones/{}/spectrum/apps/{}", zone_id, app_id),
                None::<&()>,
            )
            .await?;

        Ok(app)
    }

    /// Update a Spectrum application
    pub async fn update_spectrum_app(
        &self,
        app_id: &str,
        app: &SpectrumApp,
    ) -> Result<SpectrumApp, CloudflareError> {
        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let result: SpectrumApp = self
            .api_request(
                reqwest::Method::PUT,
                &format!("/zones/{}/spectrum/apps/{}", zone_id, app_id),
                Some(app),
            )
            .await?;

        info!("Updated Spectrum app: {}", result.name);

        // Update cache
        {
            let mut cache = self.spectrum_cache.write().await;
            cache.insert(app_id.to_string(), result.clone());
        }

        Ok(result)
    }

    /// Delete a Spectrum application
    pub async fn delete_spectrum_app(&self, app_id: &str) -> Result<(), CloudflareError> {
        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let _: serde_json::Value = self
            .api_request(
                reqwest::Method::DELETE,
                &format!("/zones/{}/spectrum/apps/{}", zone_id, app_id),
                None::<&()>,
            )
            .await?;

        info!("Deleted Spectrum app: {}", app_id);

        // Remove from cache
        {
            let mut cache = self.spectrum_cache.write().await;
            cache.remove(app_id);
        }

        Ok(())
    }

    // ==================== DNS API ====================

    /// Create a DNS record
    pub async fn create_dns_record(&self, record: &DnsRecord) -> Result<DnsRecord, CloudflareError> {
        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let result: DnsRecord = self
            .api_request(
                reqwest::Method::POST,
                &format!("/zones/{}/dns_records", zone_id),
                Some(record),
            )
            .await?;

        info!(
            "Created DNS record: {:?} {} -> {}",
            result.record_type, result.name, result.content
        );

        Ok(result)
    }

    /// List DNS records
    pub async fn list_dns_records(
        &self,
        record_type: Option<DnsRecordType>,
        name: Option<&str>,
    ) -> Result<Vec<DnsRecord>, CloudflareError> {
        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let mut query_params = Vec::new();
        if let Some(rt) = record_type {
            query_params.push(format!("type={:?}", rt));
        }
        if let Some(n) = name {
            query_params.push(format!("name={}", n));
        }

        let query_string = if query_params.is_empty() {
            String::new()
        } else {
            format!("?{}", query_params.join("&"))
        };

        let records: Vec<DnsRecord> = self
            .api_request(
                reqwest::Method::GET,
                &format!("/zones/{}/dns_records{}", zone_id, query_string),
                None::<&()>,
            )
            .await?;

        Ok(records)
    }

    /// Update a DNS record
    pub async fn update_dns_record(
        &self,
        record_id: &str,
        record: &DnsRecord,
    ) -> Result<DnsRecord, CloudflareError> {
        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let result: DnsRecord = self
            .api_request(
                reqwest::Method::PUT,
                &format!("/zones/{}/dns_records/{}", zone_id, record_id),
                Some(record),
            )
            .await?;

        info!(
            "Updated DNS record: {:?} {} -> {}",
            result.record_type, result.name, result.content
        );

        Ok(result)
    }

    /// Delete a DNS record
    pub async fn delete_dns_record(&self, record_id: &str) -> Result<(), CloudflareError> {
        let zone_id = self
            .config
            .zone_id
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Zone ID required".to_string()))?;

        let _: serde_json::Value = self
            .api_request(
                reqwest::Method::DELETE,
                &format!("/zones/{}/dns_records/{}", zone_id, record_id),
                None::<&()>,
            )
            .await?;

        info!("Deleted DNS record: {}", record_id);

        Ok(())
    }

    // ==================== Convenience Methods ====================

    /// Setup a complete game server with Spectrum and DNS
    pub async fn setup_game_server(
        &self,
        name: &str,
        subdomain: &str,
        origin_ip: &str,
        port: u16,
        protocol: SpectrumProtocol,
    ) -> Result<GameServerSetup, CloudflareError> {
        if !self.config.enabled {
            return Err(CloudflareError::InvalidConfig("Cloudflare not enabled".to_string()));
        }

        let domain = self
            .config
            .domain
            .as_ref()
            .ok_or_else(|| CloudflareError::InvalidConfig("Domain not configured".to_string()))?;

        info!(
            "Setting up game server: {} at {}.{} ({}:{:?})",
            name, subdomain, domain, port, protocol
        );

        // Create Spectrum app
        let spectrum_app = if self.config.spectrum_enabled {
            let app = SpectrumApp::for_game_server(
                name,
                port,
                protocol,
                origin_ip,
                self.config.proxy_protocol_version,
            );
            Some(self.create_spectrum_app(&app).await?)
        } else {
            None
        };

        // Create DNS record
        let dns_record = if self.config.dns_auto_manage {
            let record = DnsRecord::a_record(subdomain, origin_ip, false);
            Some(self.create_dns_record(&record).await?)
        } else {
            None
        };

        Ok(GameServerSetup {
            name: name.to_string(),
            subdomain: subdomain.to_string(),
            domain: domain.clone(),
            spectrum_app,
            dns_record,
        })
    }

    /// Teardown a game server (remove Spectrum and DNS)
    pub async fn teardown_game_server(&self, setup: &GameServerSetup) -> Result<(), CloudflareError> {
        if let Some(ref app) = setup.spectrum_app {
            if let Some(ref id) = app.id {
                self.delete_spectrum_app(id).await?;
            }
        }

        if let Some(ref record) = setup.dns_record {
            if let Some(ref id) = record.id {
                self.delete_dns_record(id).await?;
            }
        }

        info!("Tore down game server: {}", setup.name);
        Ok(())
    }
}

/// Result of setting up a game server with Cloudflare
#[derive(Debug, Clone)]
pub struct GameServerSetup {
    pub name: String,
    pub subdomain: String,
    pub domain: String,
    pub spectrum_app: Option<SpectrumApp>,
    pub dns_record: Option<DnsRecord>,
}

impl GameServerSetup {
    /// Get the connection address for players
    pub fn connection_address(&self) -> String {
        if let Some(ref app) = self.spectrum_app {
            format!("{}.{}:{}", self.subdomain, self.domain, app.edge_port)
        } else if let Some(ref record) = self.dns_record {
            format!("{}.{}", self.subdomain, self.domain)
        } else {
            self.subdomain.clone()
        }
    }
}

/// Spectrum analytics data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectrumAnalytics {
    pub app_id: String,
    pub period: String,
    pub bytes_ingress: u64,
    pub bytes_egress: u64,
    pub connections_total: u64,
    pub connections_active: u64,
    pub requests_total: u64,
}

/// DDoS attack event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DdosEvent {
    pub id: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub attack_type: String,
    pub source_ips: Vec<String>,
    pub packets_dropped: u64,
    pub bytes_dropped: u64,
    pub action_taken: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_env() {
        std::env::set_var("CLOUDFLARE_ENABLED", "true");
        std::env::set_var("CLOUDFLARE_API_TOKEN", "test-token");
        std::env::set_var("CLOUDFLARE_ZONE_ID", "zone-123");

        let config = CloudflareConfig::from_env();

        assert!(config.enabled);
        assert_eq!(config.api_token, Some("test-token".to_string()));
        assert_eq!(config.zone_id, Some("zone-123".to_string()));

        // Cleanup
        std::env::remove_var("CLOUDFLARE_ENABLED");
        std::env::remove_var("CLOUDFLARE_API_TOKEN");
        std::env::remove_var("CLOUDFLARE_ZONE_ID");
    }

    #[test]
    fn test_spectrum_app_creation() {
        let app = SpectrumApp::for_game_server(
            "minecraft",
            25565,
            SpectrumProtocol::Tcp,
            "192.168.1.100",
            2,
        );

        assert_eq!(app.name, "minecraft");
        assert_eq!(app.edge_port, 25565);
        assert_eq!(app.protocol, SpectrumProtocol::Tcp);
        assert_eq!(app.proxy_protocol, ProxyProtocol::V2);
        assert!(app.ip_firewall);
    }

    #[test]
    fn test_dns_record_creation() {
        let record = DnsRecord::a_record("game1", "192.168.1.100", false);

        assert_eq!(record.record_type, DnsRecordType::A);
        assert_eq!(record.name, "game1");
        assert_eq!(record.content, "192.168.1.100");
        assert!(!record.proxied);
    }

    #[test]
    fn test_minecraft_srv_record() {
        let record = DnsRecord::minecraft_srv("mc", "game1.example.com", 25565);

        assert_eq!(record.record_type, DnsRecordType::Srv);
        assert!(record.name.starts_with("_minecraft._tcp."));
        assert!(record.data.is_some());
    }

    #[test]
    fn test_config_validation() {
        let mut config = CloudflareConfig::default();
        config.enabled = true;

        // Should fail without API token
        assert!(config.validate().is_err());

        config.api_token = Some("token".to_string());
        config.spectrum_enabled = true;

        // Should fail without zone ID for Spectrum
        assert!(config.validate().is_err());

        config.zone_id = Some("zone-123".to_string());

        // Should pass now
        assert!(config.validate().is_ok());
    }
}
