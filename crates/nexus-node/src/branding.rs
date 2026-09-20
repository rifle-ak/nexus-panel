//! What the panel calls itself: name, logo, colour, and where a customer
//! goes for billing and support. The environment gives defaults
//! (`NEXUS_BRAND_*`); the Settings page can override them, and the result
//! is persisted under the data directory. The login screen reads it before
//! anyone signs in, so it is public.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{NodeError, Result};

const SETTINGS_FILE: &str = ".nexus/branding.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Brand {
    /// Shown in the sidebar, the title bar and the login screen.
    pub name: String,
    /// Under the name on the login screen.
    pub tagline: String,
    /// An image URL (or data URL) to show in place of the built-in mark.
    #[serde(default)]
    pub logo_url: Option<String>,
    /// Accent colour as `#rrggbb`; empty keeps the default theme.
    #[serde(default)]
    pub accent: Option<String>,
    /// Links offered to customers in the sidebar.
    #[serde(default)]
    pub support_url: Option<String>,
    #[serde(default)]
    pub billing_url: Option<String>,
}

impl Default for Brand {
    fn default() -> Self {
        Self {
            name: "Nexus Panel".to_string(),
            tagline: "Sign in to your control panel".to_string(),
            logo_url: None,
            accent: None,
            support_url: None,
            billing_url: None,
        }
    }
}

impl Brand {
    pub fn from_env() -> Self {
        let get = |name: &str| {
            std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        };
        let mut b = Self::default();
        if let Some(n) = get("NEXUS_BRAND_NAME") {
            b.name = n;
        }
        if let Some(t) = get("NEXUS_BRAND_TAGLINE") {
            b.tagline = t;
        }
        b.logo_url = get("NEXUS_BRAND_LOGO_URL");
        b.accent = get("NEXUS_BRAND_ACCENT");
        b.support_url = get("NEXUS_SUPPORT_URL");
        b.billing_url = get("NEXUS_BILLING_URL");
        b
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || self.name.len() > 60 {
            return Err(NodeError::InvalidInput(
                "name must be 1–60 characters".to_string(),
            ));
        }
        if self.tagline.len() > 120 {
            return Err(NodeError::InvalidInput(
                "tagline must be at most 120 characters".to_string(),
            ));
        }
        if let Some(a) = self.accent.as_deref().filter(|a| !a.is_empty()) {
            let ok =
                a.len() == 7 && a.starts_with('#') && a[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !ok {
                return Err(NodeError::InvalidInput(
                    "accent must be #rrggbb".to_string(),
                ));
            }
        }
        for (label, url) in [
            ("logo_url", &self.logo_url),
            ("support_url", &self.support_url),
            ("billing_url", &self.billing_url),
        ] {
            if let Some(u) = url.as_deref().filter(|u| !u.is_empty()) {
                let ok = u.starts_with("https://")
                    || u.starts_with("http://")
                    || (label == "logo_url" && u.starts_with("data:image/"));
                if !ok || u.len() > 200_000 {
                    return Err(NodeError::InvalidInput(format!(
                        "{} must be an http(s) URL{}",
                        label,
                        if label == "logo_url" {
                            " or a data:image URL"
                        } else {
                            ""
                        }
                    )));
                }
            }
        }
        Ok(())
    }

    /// Empty strings mean "unset".
    fn normalized(mut self) -> Self {
        let clean = |v: &mut Option<String>| {
            if v.as_deref().map(|s| s.trim().is_empty()).unwrap_or(false) {
                *v = None;
            }
        };
        clean(&mut self.logo_url);
        clean(&mut self.accent);
        clean(&mut self.support_url);
        clean(&mut self.billing_url);
        self.name = self.name.trim().to_string();
        self.tagline = self.tagline.trim().to_string();
        self
    }
}

pub struct BrandStore {
    path: PathBuf,
    brand: RwLock<Brand>,
}

impl BrandStore {
    /// The persisted brand if there is one, else the environment's.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SETTINGS_FILE);
        let brand = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Brand>(&b).ok())
            .unwrap_or_else(Brand::from_env);
        Self {
            path,
            brand: RwLock::new(brand),
        }
    }

    pub async fn get(&self) -> Brand {
        self.brand.read().await.clone()
    }

    pub async fn set(&self, brand: Brand) -> Result<Brand> {
        let brand = brand.normalized();
        brand.validate()?;
        let bytes = serde_json::to_vec_pretty(&brand)
            .map_err(|e| NodeError::Internal(format!("serialize branding: {}", e)))?;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        *self.brand.write().await = brand.clone();
        Ok(brand)
    }

    /// Back to the environment's defaults.
    pub async fn reset(&self) -> Brand {
        let _ = tokio::fs::remove_file(&self.path).await;
        let brand = Brand::from_env();
        *self.brand.write().await = brand.clone();
        brand
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn brand_validates_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let store = BrandStore::load(dir.path());
        assert_eq!(store.get().await.name, "Nexus Panel");
        let bad = Brand {
            accent: Some("blue".into()),
            ..Default::default()
        };
        assert!(store.set(bad).await.is_err());
        let bad = Brand {
            logo_url: Some("javascript:alert(1)".into()),
            ..Default::default()
        };
        assert!(store.set(bad).await.is_err());
        let good = Brand {
            name: "  Acme Hosting ".into(),
            tagline: "Game servers, done right".into(),
            logo_url: Some("https://cdn.example/logo.png".into()),
            accent: Some("#ff6600".into()),
            support_url: Some("".into()),
            billing_url: Some("https://billing.example".into()),
        };
        let saved = store.set(good).await.unwrap();
        assert_eq!(saved.name, "Acme Hosting");
        assert_eq!(saved.support_url, None);
        let again = BrandStore::load(dir.path());
        assert_eq!(again.get().await, saved);
        assert_eq!(again.reset().await.name, "Nexus Panel");
        assert_eq!(BrandStore::load(dir.path()).get().await.name, "Nexus Panel");
    }
}
