//! Database management for game servers
//!
//! Provides database provisioning and management for containers:
//! - MySQL/MariaDB database management
//! - User and permission management
//! - Connection string generation

use crate::error::{NodeError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Database type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatabaseType {
    MySQL,
    MariaDB,
    PostgreSQL,
}

impl DatabaseType {
    /// Get default port for this database type
    pub fn default_port(&self) -> u16 {
        match self {
            DatabaseType::MySQL | DatabaseType::MariaDB => 3306,
            DatabaseType::PostgreSQL => 5432,
        }
    }

    /// Get connection string format
    pub fn connection_string_format(&self) -> &'static str {
        match self {
            DatabaseType::MySQL | DatabaseType::MariaDB => {
                "mysql://{username}:{password}@{host}:{port}/{database}"
            }
            DatabaseType::PostgreSQL => {
                "postgresql://{username}:{password}@{host}:{port}/{database}"
            }
        }
    }
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatabaseType::MySQL => write!(f, "mysql"),
            DatabaseType::MariaDB => write!(f, "mariadb"),
            DatabaseType::PostgreSQL => write!(f, "postgresql"),
        }
    }
}

/// Database host configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseHost {
    /// Unique host ID
    pub id: String,
    /// Host name/address
    pub host: String,
    /// Port number
    pub port: u16,
    /// Database type
    pub db_type: DatabaseType,
    /// Username for admin access
    pub username: String,
    /// Password for admin access (should be stored securely)
    #[serde(skip_serializing)]
    pub password: String,
    /// Maximum databases per container
    pub max_databases: u32,
    /// Node ID this host is associated with
    pub node_id: Option<String>,
    /// Whether this host is available for new databases
    pub is_available: bool,
}

impl DatabaseHost {
    /// Create a new database host
    pub fn new(
        host: impl Into<String>,
        port: u16,
        db_type: DatabaseType,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            host: host.into(),
            port,
            db_type,
            username: username.into(),
            password: password.into(),
            max_databases: 10,
            node_id: None,
            is_available: true,
        }
    }

    /// Get admin connection string
    pub fn admin_connection_string(&self) -> String {
        self.db_type
            .connection_string_format()
            .replace("{username}", &self.username)
            .replace("{password}", &self.password)
            .replace("{host}", &self.host)
            .replace("{port}", &self.port.to_string())
            .replace("{database}", "")
    }
}

/// Database instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Database {
    /// Unique database ID
    pub id: String,
    /// Database name
    pub name: String,
    /// Container ID this database belongs to
    pub container_id: String,
    /// Host ID this database is on
    pub host_id: String,
    /// Database username
    pub username: String,
    /// Database password
    #[serde(skip_serializing)]
    pub password: String,
    /// Remote access host pattern (e.g., "%" for any, or specific IP)
    pub remote: String,
    /// Maximum connections allowed
    pub max_connections: u32,
    /// When the database was created
    pub created_at: DateTime<Utc>,
}

impl Database {
    /// Generate a connection string for this database
    pub fn connection_string(&self, host: &DatabaseHost) -> String {
        host.db_type
            .connection_string_format()
            .replace("{username}", &self.username)
            .replace("{password}", &self.password)
            .replace("{host}", &host.host)
            .replace("{port}", &host.port.to_string())
            .replace("{database}", &self.name)
    }

    /// Get JDBC connection string
    pub fn jdbc_connection_string(&self, host: &DatabaseHost) -> String {
        match host.db_type {
            DatabaseType::MySQL | DatabaseType::MariaDB => {
                format!(
                    "jdbc:mysql://{}:{}/{}",
                    host.host, host.port, self.name
                )
            }
            DatabaseType::PostgreSQL => {
                format!(
                    "jdbc:postgresql://{}:{}/{}",
                    host.host, host.port, self.name
                )
            }
        }
    }
}

/// Database manager
pub struct DatabaseManager {
    /// Database hosts
    hosts: Arc<RwLock<HashMap<String, DatabaseHost>>>,
    /// All databases indexed by database ID
    databases: Arc<RwLock<HashMap<String, Database>>>,
    /// Databases by container ID
    container_databases: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Count of databases per host
    host_database_counts: Arc<RwLock<HashMap<String, u32>>>,
}

impl DatabaseManager {
    /// Create a new database manager
    pub fn new() -> Self {
        Self {
            hosts: Arc::new(RwLock::new(HashMap::new())),
            databases: Arc::new(RwLock::new(HashMap::new())),
            container_databases: Arc::new(RwLock::new(HashMap::new())),
            host_database_counts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a database host
    pub async fn register_host(&self, host: DatabaseHost) -> Result<()> {
        let host_id = host.id.clone();
        let mut hosts = self.hosts.write().await;
        hosts.insert(host_id.clone(), host);

        let mut counts = self.host_database_counts.write().await;
        counts.insert(host_id.clone(), 0);

        info!("Registered database host {}", host_id);
        Ok(())
    }

    /// Get a database host
    pub async fn get_host(&self, host_id: &str) -> Result<DatabaseHost> {
        let hosts = self.hosts.read().await;
        hosts
            .get(host_id)
            .cloned()
            .ok_or_else(|| NodeError::InvalidInput(format!("Host {} not found", host_id)))
    }

    /// List all database hosts
    pub async fn list_hosts(&self) -> Vec<DatabaseHost> {
        let hosts = self.hosts.read().await;
        hosts.values().cloned().collect()
    }

    /// Find a suitable host for a new database
    async fn find_available_host(&self) -> Option<String> {
        let hosts = self.hosts.read().await;
        let counts = self.host_database_counts.read().await;

        for (host_id, host) in hosts.iter() {
            if !host.is_available {
                continue;
            }
            let count = counts.get(host_id).copied().unwrap_or(0);
            if count < host.max_databases {
                return Some(host_id.clone());
            }
        }

        None
    }

    /// Create a new database
    pub async fn create_database(
        &self,
        container_id: &str,
        name: Option<&str>,
        host_id: Option<&str>,
        remote: Option<&str>,
    ) -> Result<Database> {
        // Find or validate host
        let host_id = if let Some(hid) = host_id {
            // Validate host exists and has capacity
            let hosts = self.hosts.read().await;
            let host = hosts.get(hid).ok_or_else(|| {
                NodeError::InvalidInput(format!("Host {} not found", hid))
            })?;
            if !host.is_available {
                return Err(NodeError::InvalidInput(format!(
                    "Host {} is not available",
                    hid
                )));
            }
            drop(hosts);

            let counts = self.host_database_counts.read().await;
            let count = counts.get(hid).copied().unwrap_or(0);
            let hosts = self.hosts.read().await;
            if count >= hosts.get(hid).map(|h| h.max_databases).unwrap_or(0) {
                return Err(NodeError::InvalidInput(format!(
                    "Host {} has reached maximum database limit",
                    hid
                )));
            }
            hid.to_string()
        } else {
            self.find_available_host().await.ok_or_else(|| {
                NodeError::InvalidInput("No available database hosts".to_string())
            })?
        };

        // Generate database name if not provided
        let db_name = name
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("s{}_{}", container_id.chars().take(8).collect::<String>(), Uuid::new_v4().to_string().chars().take(8).collect::<String>()));

        // Generate credentials
        let db_id = Uuid::new_v4().to_string();
        let username = format!("u{}", db_id.chars().take(16).collect::<String>());
        let password = generate_secure_password(24);

        let database = Database {
            id: db_id.clone(),
            name: db_name.clone(),
            container_id: container_id.to_string(),
            host_id: host_id.clone(),
            username,
            password,
            remote: remote.unwrap_or("%").to_string(),
            max_connections: 10,
            created_at: Utc::now(),
        };

        // Store database
        {
            let mut databases = self.databases.write().await;
            databases.insert(db_id.clone(), database.clone());
        }

        // Update container index
        {
            let mut container_dbs = self.container_databases.write().await;
            container_dbs
                .entry(container_id.to_string())
                .or_insert_with(Vec::new)
                .push(db_id.clone());
        }

        // Update host count
        {
            let mut counts = self.host_database_counts.write().await;
            *counts.entry(host_id.clone()).or_insert(0) += 1;
        }

        info!(
            "Created database {} on host {} for container {}",
            db_name, host_id, container_id
        );

        Ok(database)
    }

    /// Get a database
    pub async fn get_database(&self, database_id: &str) -> Result<Database> {
        let databases = self.databases.read().await;
        databases
            .get(database_id)
            .cloned()
            .ok_or_else(|| NodeError::InvalidInput(format!("Database {} not found", database_id)))
    }

    /// List databases for a container
    pub async fn list_container_databases(&self, container_id: &str) -> Vec<Database> {
        let container_dbs = self.container_databases.read().await;
        let databases = self.databases.read().await;

        let mut result = Vec::new();

        if let Some(db_ids) = container_dbs.get(container_id) {
            for db_id in db_ids {
                if let Some(db) = databases.get(db_id) {
                    result.push(db.clone());
                }
            }
        }

        result
    }

    /// Delete a database
    pub async fn delete_database(&self, database_id: &str) -> Result<()> {
        // Get database info
        let database = {
            let databases = self.databases.read().await;
            databases.get(database_id).cloned().ok_or_else(|| {
                NodeError::InvalidInput(format!("Database {} not found", database_id))
            })?
        };

        // Remove from databases
        {
            let mut databases = self.databases.write().await;
            databases.remove(database_id);
        }

        // Update container index
        {
            let mut container_dbs = self.container_databases.write().await;
            if let Some(dbs) = container_dbs.get_mut(&database.container_id) {
                dbs.retain(|id| id != database_id);
            }
        }

        // Update host count
        {
            let mut counts = self.host_database_counts.write().await;
            if let Some(count) = counts.get_mut(&database.host_id) {
                *count = count.saturating_sub(1);
            }
        }

        info!(
            "Deleted database {} from host {}",
            database.name, database.host_id
        );

        Ok(())
    }

    /// Rotate database password
    pub async fn rotate_password(&self, database_id: &str) -> Result<String> {
        let mut databases = self.databases.write().await;
        let database = databases.get_mut(database_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("Database {} not found", database_id))
        })?;

        let new_password = generate_secure_password(24);
        database.password = new_password.clone();

        info!("Rotated password for database {}", database_id);

        Ok(new_password)
    }

    /// Get database statistics
    pub async fn get_stats(&self) -> DatabaseStats {
        let hosts = self.hosts.read().await;
        let databases = self.databases.read().await;
        let counts = self.host_database_counts.read().await;

        let total_capacity: u32 = hosts.values().map(|h| h.max_databases).sum();
        let total_databases = databases.len() as u32;

        DatabaseStats {
            total_hosts: hosts.len() as u32,
            available_hosts: hosts.values().filter(|h| h.is_available).count() as u32,
            total_capacity,
            total_databases,
            available_slots: total_capacity.saturating_sub(total_databases),
        }
    }

    /// Get database with connection info
    pub async fn get_database_with_connection(
        &self,
        database_id: &str,
    ) -> Result<DatabaseConnection> {
        let databases = self.databases.read().await;
        let database = databases.get(database_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("Database {} not found", database_id))
        })?;

        let hosts = self.hosts.read().await;
        let host = hosts.get(&database.host_id).ok_or_else(|| {
            NodeError::Internal(format!("Host {} not found for database", database.host_id))
        })?;

        Ok(DatabaseConnection {
            database: database.clone(),
            host: host.host.clone(),
            port: host.port,
            db_type: host.db_type,
            connection_string: database.connection_string(host),
            jdbc_connection_string: database.jdbc_connection_string(host),
        })
    }
}

impl Default for DatabaseManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Database with full connection information
#[derive(Debug, Clone, Serialize)]
pub struct DatabaseConnection {
    /// Database info
    #[serde(flatten)]
    pub database: Database,
    /// Host address
    pub host: String,
    /// Port number
    pub port: u16,
    /// Database type
    pub db_type: DatabaseType,
    /// Standard connection string
    pub connection_string: String,
    /// JDBC connection string
    pub jdbc_connection_string: String,
}

/// Database statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseStats {
    /// Total database hosts
    pub total_hosts: u32,
    /// Available hosts
    pub available_hosts: u32,
    /// Total database capacity
    pub total_capacity: u32,
    /// Total databases created
    pub total_databases: u32,
    /// Available slots for new databases
    pub available_slots: u32,
}

/// Generate a secure random password
fn generate_secure_password(length: usize) -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_database_type_port() {
        assert_eq!(DatabaseType::MySQL.default_port(), 3306);
        assert_eq!(DatabaseType::MariaDB.default_port(), 3306);
        assert_eq!(DatabaseType::PostgreSQL.default_port(), 5432);
    }

    #[tokio::test]
    async fn test_register_host() {
        let manager = DatabaseManager::new();

        let host = DatabaseHost::new(
            "localhost",
            3306,
            DatabaseType::MySQL,
            "root",
            "password",
        );

        manager.register_host(host.clone()).await.unwrap();

        let retrieved = manager.get_host(&host.id).await.unwrap();
        assert_eq!(retrieved.host, "localhost");
    }

    #[tokio::test]
    async fn test_create_database() {
        let manager = DatabaseManager::new();

        let host = DatabaseHost::new(
            "localhost",
            3306,
            DatabaseType::MySQL,
            "root",
            "password",
        );
        let host_id = host.id.clone();

        manager.register_host(host).await.unwrap();

        let database = manager
            .create_database("container-1", Some("testdb"), Some(&host_id), None)
            .await
            .unwrap();

        assert_eq!(database.name, "testdb");
        assert_eq!(database.container_id, "container-1");
        assert_eq!(database.host_id, host_id);
    }

    #[tokio::test]
    async fn test_list_container_databases() {
        let manager = DatabaseManager::new();

        let host = DatabaseHost::new(
            "localhost",
            3306,
            DatabaseType::MySQL,
            "root",
            "password",
        );
        let host_id = host.id.clone();

        manager.register_host(host).await.unwrap();

        // Create two databases
        manager
            .create_database("container-1", Some("db1"), Some(&host_id), None)
            .await
            .unwrap();
        manager
            .create_database("container-1", Some("db2"), Some(&host_id), None)
            .await
            .unwrap();

        let databases = manager.list_container_databases("container-1").await;
        assert_eq!(databases.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_database() {
        let manager = DatabaseManager::new();

        let host = DatabaseHost::new(
            "localhost",
            3306,
            DatabaseType::MySQL,
            "root",
            "password",
        );
        let host_id = host.id.clone();

        manager.register_host(host).await.unwrap();

        let database = manager
            .create_database("container-1", Some("testdb"), Some(&host_id), None)
            .await
            .unwrap();

        manager.delete_database(&database.id).await.unwrap();

        // Should be gone
        assert!(manager.get_database(&database.id).await.is_err());
    }

    #[test]
    fn test_generate_password() {
        let password = generate_secure_password(24);
        assert_eq!(password.len(), 24);
    }
}
