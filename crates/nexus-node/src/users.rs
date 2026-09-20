//! Panel accounts and API keys, kept on disk.
//!
//! The node has always had one operator credential from the environment
//! (`WEB_PASSWORD`, `AUTH_API_KEYS`). This adds named accounts: admins with
//! the run of the node, and users granted a set of permissions on
//! particular servers, so a co-owner can restart and read the console
//! without being able to delete files. API keys are named and revocable,
//! and every action is attributed to the account or key that took it.
//!
//! Passwords are hashed with Argon2id; API keys are stored as SHA-256 and
//! shown once, at creation. Everything lives in `DATA_DIR/.nexus/users.json`,
//! written atomically and readable only by the node.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::error::{NodeError, Result};
use crate::subuser::Permission;

const STORE_FILE: &str = ".nexus/users.json";
/// The prefix every key the panel mints carries, so one is recognisable in
/// a config file or a log.
pub const API_KEY_PREFIX: &str = "nxk_";
pub const MIN_PASSWORD_LEN: usize = 10;

/// What a user may do on each server.
pub type Grants = BTreeMap<String, BTreeSet<Permission>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    /// Argon2id PHC string.
    pub password_hash: String,
    /// Admins have the run of the node; `grants` are ignored for them.
    pub admin: bool,
    #[serde(default)]
    pub grants: Grants,
    #[serde(default)]
    pub disabled: bool,
    pub created_at: i64,
    #[serde(default)]
    pub last_login_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    /// The first characters of the key, for telling keys apart.
    pub prefix: String,
    /// SHA-256 of the whole key, hex.
    pub hash: String,
    pub created_at: i64,
    pub created_by: String,
    #[serde(default)]
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    users: Vec<User>,
    #[serde(default)]
    api_keys: Vec<ApiKey>,
}

/// The account and key registry.
pub struct UserStore {
    path: PathBuf,
    inner: RwLock<StoreFile>,
}

/// A preset bundle of permissions, as the UI offers them.
pub fn preset(name: &str) -> Option<BTreeSet<Permission>> {
    let set = match name {
        "read_only" => Permission::read_only(),
        "default" => Permission::default_permissions(),
        "operator" => Permission::operator(),
        "full" => {
            let mut all = Permission::all();
            // Node-level things a per-server grant never carries.
            all.retain(|p| {
                !matches!(
                    p,
                    Permission::UserCreate
                        | Permission::UserRead
                        | Permission::UserUpdate
                        | Permission::UserDelete
                        | Permission::AllocationCreate
                        | Permission::AllocationRead
                        | Permission::AllocationUpdate
                        | Permission::AllocationDelete
                )
            });
            all
        }
        _ => return None,
    };
    Some(set.into_iter().collect())
}

pub fn validate_username(name: &str) -> Result<()> {
    let ok = (3..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        Err(NodeError::InvalidInput(
            "username must be 3–32 characters of a-z, 0-9, '.', '_' or '-', starting with a letter or digit"
                .to_string(),
        ))
    }
}

pub fn validate_password(password: &str) -> Result<()> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err(NodeError::InvalidInput(format!(
            "password must be at least {} characters",
            MIN_PASSWORD_LEN
        )));
    }
    if password.len() > 512 {
        return Err(NodeError::InvalidInput("password is too long".to_string()));
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| NodeError::Internal(format!("could not hash password: {}", e)))
}

fn verify_password(hash: &str, candidate: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default().verify_password(candidate.as_bytes(), &parsed).is_ok()
}

pub(crate) fn sha256_hex(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl UserStore {
    /// Load the store under `data_dir`, or start empty.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(STORE_FILE);
        let inner = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoreFile>(&bytes) {
                Ok(f) => {
                    info!(
                        "Loaded {} user(s) and {} API key(s)",
                        f.users.len(),
                        f.api_keys.len()
                    );
                    f
                }
                Err(e) => {
                    warn!(
                        "User store {:?} is unreadable ({}); starting empty",
                        path, e
                    );
                    StoreFile::default()
                }
            },
            Err(_) => StoreFile::default(),
        };
        Self {
            path,
            inner: RwLock::new(inner),
        }
    }

    async fn save(&self) -> Result<()> {
        let bytes = {
            let inner = self.inner.read().await;
            serde_json::to_vec_pretty(&*inner)
                .map_err(|e| NodeError::Internal(format!("serialize user store: {}", e)))?
        };
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, &bytes).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await;
        }
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    // ── Users ───────────────────────────────────────────────────────

    pub async fn list_users(&self) -> Vec<User> {
        self.inner.read().await.users.clone()
    }

    pub async fn get_user(&self, id: &str) -> Option<User> {
        self.inner.read().await.users.iter().find(|u| u.id == id).cloned()
    }

    pub async fn find_by_username(&self, username: &str) -> Option<User> {
        let username = username.trim().to_ascii_lowercase();
        self.inner.read().await.users.iter().find(|u| u.username == username).cloned()
    }

    pub async fn create_user(
        &self,
        username: &str,
        password: &str,
        admin: bool,
        grants: Grants,
    ) -> Result<User> {
        let username = username.trim().to_ascii_lowercase();
        validate_username(&username)?;
        validate_password(password)?;
        let user = User {
            id: uuid::Uuid::new_v4().to_string(),
            username: username.clone(),
            password_hash: hash_password(password)?,
            admin,
            grants,
            disabled: false,
            created_at: now(),
            last_login_at: None,
        };
        {
            let mut inner = self.inner.write().await;
            if inner.users.iter().any(|u| u.username == username) {
                return Err(NodeError::InvalidInput(format!(
                    "a user named {} already exists",
                    username
                )));
            }
            inner.users.push(user.clone());
        }
        self.save().await?;
        info!("Created user {} (admin: {})", username, admin);
        Ok(user)
    }

    /// Change what an account is: any of password, admin flag, grants,
    /// disabled. `None` leaves a field alone.
    pub async fn update_user(
        &self,
        id: &str,
        password: Option<&str>,
        admin: Option<bool>,
        grants: Option<Grants>,
        disabled: Option<bool>,
    ) -> Result<User> {
        let password_hash = match password {
            Some(p) => {
                validate_password(p)?;
                Some(hash_password(p)?)
            }
            None => None,
        };
        let updated = {
            let mut inner = self.inner.write().await;
            let user = inner
                .users
                .iter_mut()
                .find(|u| u.id == id)
                .ok_or_else(|| NodeError::InvalidInput(format!("no user with id {}", id)))?;
            if let Some(h) = password_hash {
                user.password_hash = h;
            }
            if let Some(a) = admin {
                user.admin = a;
            }
            if let Some(g) = grants {
                user.grants = g;
            }
            if let Some(d) = disabled {
                user.disabled = d;
            }
            user.clone()
        };
        self.save().await?;
        Ok(updated)
    }

    pub async fn delete_user(&self, id: &str) -> Result<()> {
        {
            let mut inner = self.inner.write().await;
            let before = inner.users.len();
            inner.users.retain(|u| u.id != id);
            if inner.users.len() == before {
                return Err(NodeError::InvalidInput(format!("no user with id {}", id)));
            }
        }
        self.save().await
    }

    /// Check a username and password; `None` for anything but an enabled
    /// account with that password. Records the login time.
    pub async fn verify_login(&self, username: &str, password: &str) -> Option<User> {
        let user = self.find_by_username(username).await?;
        if user.disabled || !verify_password(&user.password_hash, password) {
            return None;
        }
        let stamped = {
            let mut inner = self.inner.write().await;
            let u = inner.users.iter_mut().find(|u| u.id == user.id)?;
            u.last_login_at = Some(now());
            u.clone()
        };
        if let Err(e) = self.save().await {
            warn!("Could not record login time: {}", e);
        }
        Some(stamped)
    }

    /// Change a password, given the current one.
    pub async fn change_password(&self, id: &str, current: &str, new: &str) -> Result<()> {
        let user = self
            .get_user(id)
            .await
            .ok_or_else(|| NodeError::InvalidInput(format!("no user with id {}", id)))?;
        if !verify_password(&user.password_hash, current) {
            return Err(NodeError::InvalidInput(
                "current password is wrong".to_string(),
            ));
        }
        self.update_user(id, Some(new), None, None, None).await.map(|_| ())
    }

    // ── API keys ────────────────────────────────────────────────────

    pub async fn list_api_keys(&self) -> Vec<ApiKey> {
        self.inner.read().await.api_keys.clone()
    }

    /// Mint a key. The plaintext is returned once and never stored.
    pub async fn create_api_key(&self, name: &str, created_by: &str) -> Result<(ApiKey, String)> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(NodeError::InvalidInput(
                "key name must be 1–64 characters".to_string(),
            ));
        }
        let mut raw = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut raw);
        let secret = format!(
            "{}{}",
            API_KEY_PREFIX,
            raw.iter().map(|b| format!("{:02x}", b)).collect::<String>()
        );
        let key = ApiKey {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            prefix: secret[..API_KEY_PREFIX.len() + 8].to_string(),
            hash: sha256_hex(&secret),
            created_at: now(),
            created_by: created_by.to_string(),
            last_used_at: None,
        };
        self.inner.write().await.api_keys.push(key.clone());
        self.save().await?;
        info!("Created API key {} ({})", key.name, key.prefix);
        Ok((key, secret))
    }

    pub async fn revoke_api_key(&self, id: &str) -> Result<ApiKey> {
        let removed = {
            let mut inner = self.inner.write().await;
            let pos = inner
                .api_keys
                .iter()
                .position(|k| k.id == id)
                .ok_or_else(|| NodeError::InvalidInput(format!("no API key with id {}", id)))?;
            inner.api_keys.remove(pos)
        };
        self.save().await?;
        info!("Revoked API key {} ({})", removed.name, removed.prefix);
        Ok(removed)
    }

    /// The key a candidate is, if it is one of ours. Stamps its last use.
    pub async fn verify_api_key(&self, candidate: &str) -> Option<ApiKey> {
        if !candidate.starts_with(API_KEY_PREFIX) {
            return None;
        }
        let hash = sha256_hex(candidate);
        let found = {
            let mut inner = self.inner.write().await;
            let key = inner.api_keys.iter_mut().find(|k| ct_eq(&k.hash, &hash))?;
            // Stamp at most once a minute; a busy billing system would
            // otherwise rewrite the file on every call.
            let stamp = now();
            if key.last_used_at.map_or(true, |t| stamp - t >= 60) {
                key.last_used_at = Some(stamp);
                Some((key.clone(), true))
            } else {
                Some((key.clone(), false))
            }
        };
        let (key, changed) = found?;
        if changed {
            if let Err(e) = self.save().await {
                warn!("Could not record API key use: {}", e);
            }
        }
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn users_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserStore::load(dir.path());
        let mut grants = Grants::new();
        grants.insert("srv-1".into(), preset("operator").unwrap());
        let alice = store
            .create_user("Alice", "correct horse battery", false, grants.clone())
            .await
            .unwrap();
        assert_eq!(alice.username, "alice");
        assert!(alice.password_hash.starts_with("$argon2id$"));
        assert!(store
            .create_user("alice", "another long password", true, Grants::new())
            .await
            .is_err());
        assert!(store
            .create_user("a", "another long password", true, Grants::new())
            .await
            .is_err());
        assert!(store.create_user("bob", "short", true, Grants::new()).await.is_err());

        assert!(store.verify_login("alice", "wrong password!!").await.is_none());
        let logged = store.verify_login("ALICE", "correct horse battery").await.unwrap();
        assert!(logged.last_login_at.is_some());

        let again = UserStore::load(dir.path());
        let loaded = again.find_by_username("alice").await.unwrap();
        assert_eq!(loaded.grants, grants);
        assert!(loaded.grants["srv-1"].contains(&Permission::PowerRestart));
        assert!(!loaded.grants["srv-1"].contains(&Permission::UserDelete));

        again.update_user(&alice.id, None, None, None, Some(true)).await.unwrap();
        assert!(again.verify_login("alice", "correct horse battery").await.is_none());
        assert!(again
            .change_password(&alice.id, "nope nope nope", "new password here")
            .await
            .is_err());
        again
            .change_password(&alice.id, "correct horse battery", "new password here")
            .await
            .unwrap();
        again.update_user(&alice.id, None, None, None, Some(false)).await.unwrap();
        assert!(again.verify_login("alice", "new password here").await.is_some());
        again.delete_user(&alice.id).await.unwrap();
        assert!(again.delete_user(&alice.id).await.is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(STORE_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn api_keys_are_shown_once_and_verified_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserStore::load(dir.path());
        let (key, secret) = store.create_api_key("billing", "admin").await.unwrap();
        assert!(secret.starts_with(API_KEY_PREFIX));
        assert_eq!(secret.len(), API_KEY_PREFIX.len() + 48);
        assert!(secret.starts_with(&key.prefix));
        assert_ne!(key.hash, secret);
        let text = std::fs::read_to_string(dir.path().join(STORE_FILE)).unwrap();
        assert!(!text.contains(&secret));

        let used = store.verify_api_key(&secret).await.unwrap();
        assert_eq!(used.id, key.id);
        assert!(used.last_used_at.is_some());
        assert!(store.verify_api_key("nxk_notakey").await.is_none());
        assert!(store.verify_api_key("plainkey").await.is_none());

        store.revoke_api_key(&key.id).await.unwrap();
        assert!(store.verify_api_key(&secret).await.is_none());
        assert!(store.create_api_key("", "admin").await.is_err());
    }

    #[test]
    fn presets_and_validation() {
        assert!(preset("full").unwrap().contains(&Permission::FileDelete));
        assert!(!preset("full").unwrap().contains(&Permission::UserDelete));
        assert!(preset("read_only").unwrap().contains(&Permission::ConsoleRead));
        assert!(!preset("read_only").unwrap().contains(&Permission::ConsoleWrite));
        assert!(preset("nope").is_none());
        assert!(validate_username("ops-team.1").is_ok());
        assert!(validate_username("-bad").is_err());
        assert!(validate_username("Has Space").is_err());
    }
}
