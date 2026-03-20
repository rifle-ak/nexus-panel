//! Secrets management for Nexus Node
//!
//! Provides a trait-based interface for secrets management with multiple
//! backend implementations (Vault, AWS Secrets Manager, Kubernetes, etc.)

use crate::error::{NodeError, Result};
use async_trait::async_trait;
use std::collections::HashMap;

/// Trait for secrets management backends
#[async_trait]
pub trait SecretsManager: Send + Sync {
    /// Get a secret value by key
    async fn get_secret(&self, key: &str) -> Result<String>;

    /// Set a secret value
    async fn set_secret(&self, key: &str, value: &str) -> Result<()>;

    /// Delete a secret
    async fn delete_secret(&self, key: &str) -> Result<()>;

    /// List all secret keys (optional, may not be supported by all backends)
    async fn list_secrets(&self) -> Result<Vec<String>>;
}

/// Environment variable-based secrets manager (for development/testing)
pub struct EnvSecretsManager {
    prefix: String,
}

impl EnvSecretsManager {
    /// Create a new environment variable secrets manager
    pub fn new(prefix: String) -> Self {
        Self { prefix }
    }

    fn env_key(&self, key: &str) -> String {
        format!("{}_{}", self.prefix, key.to_uppercase().replace('.', "_"))
    }
}

#[async_trait]
impl SecretsManager for EnvSecretsManager {
    async fn get_secret(&self, key: &str) -> Result<String> {
        let env_key = self.env_key(key);
        std::env::var(&env_key).map_err(|_| {
            NodeError::Internal(format!(
                "Secret not found: {} (looked for env var: {})",
                key, env_key
            ))
        })
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        let env_key = self.env_key(key);
        std::env::set_var(&env_key, value);
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        let env_key = self.env_key(key);
        std::env::remove_var(&env_key);
        Ok(())
    }

    async fn list_secrets(&self) -> Result<Vec<String>> {
        // List all environment variables with the prefix
        let prefix = self.env_key("");
        let secrets: Vec<String> = std::env::vars()
            .filter_map(|(key, _)| {
                if key.starts_with(&prefix) {
                    Some(key.trim_start_matches(&prefix).to_lowercase().replace('_', "."))
                } else {
                    None
                }
            })
            .collect();
        Ok(secrets)
    }
}

/// In-memory secrets manager (for testing)
pub struct MemorySecretsManager {
    secrets: tokio::sync::RwLock<HashMap<String, String>>,
}

impl MemorySecretsManager {
    /// Create a new in-memory secrets manager
    pub fn new() -> Self {
        Self {
            secrets: tokio::sync::RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemorySecretsManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretsManager for MemorySecretsManager {
    async fn get_secret(&self, key: &str) -> Result<String> {
        let secrets = self.secrets.read().await;
        secrets
            .get(key)
            .cloned()
            .ok_or_else(|| NodeError::Internal(format!("Secret not found: {}", key)))
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        let mut secrets = self.secrets.write().await;
        secrets.insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        let mut secrets = self.secrets.write().await;
        secrets.remove(key);
        Ok(())
    }

    async fn list_secrets(&self) -> Result<Vec<String>> {
        let secrets = self.secrets.read().await;
        Ok(secrets.keys().cloned().collect())
    }
}

/// Helper to resolve secrets in configuration
pub struct SecretResolver {
    manager: Box<dyn SecretsManager>,
}

impl SecretResolver {
    /// Create a new secret resolver
    pub fn new(manager: Box<dyn SecretsManager>) -> Self {
        Self { manager }
    }

    /// Resolve a secret reference (e.g., "secret:database/password")
    pub async fn resolve(&self, reference: &str) -> Result<String> {
        if reference.starts_with("secret:") {
            let key = reference.trim_start_matches("secret:");
            self.manager.get_secret(key).await
        } else {
            // Not a secret reference, return as-is
            Ok(reference.to_string())
        }
    }

    /// Resolve multiple secret references in a map
    pub async fn resolve_map(
        &self,
        values: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>> {
        let mut resolved = HashMap::new();
        for (key, value) in values {
            resolved.insert(key.clone(), self.resolve(value).await?);
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_secrets_manager() {
        let manager = MemorySecretsManager::new();

        // Set a secret
        manager.set_secret("test.key", "test.value").await.unwrap();

        // Get the secret
        let value = manager.get_secret("test.key").await.unwrap();
        assert_eq!(value, "test.value");

        // List secrets
        let secrets = manager.list_secrets().await.unwrap();
        assert!(secrets.contains(&"test.key".to_string()));

        // Delete the secret
        manager.delete_secret("test.key").await.unwrap();
        assert!(manager.get_secret("test.key").await.is_err());
    }

    #[tokio::test]
    async fn test_secret_resolver() {
        let manager = Box::new(MemorySecretsManager::new());
        manager.set_secret("database.password", "secret123").await.unwrap();

        let resolver = SecretResolver::new(manager);

        // Resolve a secret reference
        let value = resolver.resolve("secret:database.password").await.unwrap();
        assert_eq!(value, "secret123");

        // Non-secret reference returns as-is
        let value = resolver.resolve("plain-text").await.unwrap();
        assert_eq!(value, "plain-text");
    }
}
