use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// Custom deserializer for JSON-encoded strings or raw objects
/// Pterodactyl stores some fields as JSON strings that need to be parsed,
/// but some eggs have them as raw objects
/// Also handles malformed JSON with duplicate keys
fn deserialize_json_string_or_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: for<'a> Deserialize<'a> + Default,
{
    use serde::de::Error;
    use serde_json::Value;

    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => {
            if s.is_empty() || s == "{}" {
                Ok(T::default())
            } else {
                // First parse to Value (handles duplicate keys by keeping last)
                // then convert to target type
                match serde_json::from_str::<Value>(&s) {
                    Ok(inner_value) => serde_json::from_value(inner_value).map_err(Error::custom),
                    Err(_) => {
                        // If still fails, return default
                        Ok(T::default())
                    }
                }
            }
        }
        Value::Object(_) => serde_json::from_value(value).map_err(Error::custom),
        Value::Null => Ok(T::default()),
        _ => Ok(T::default()),
    }
}

/// Custom deserializer that handles both string and array of strings
/// Also handles empty strings, null, and nested structures
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value;

    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => {
            if s.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![s])
            }
        }
        Value::Array(arr) => {
            Ok(arr.into_iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        }
        Value::Null => Ok(Vec::new()),
        _ => Ok(Vec::new()),
    }
}

/// Default empty HashMap for missing fields
fn default_files() -> HashMap<String, FileConfig> {
    HashMap::new()
}

/// Default StartupConfig for missing fields
fn default_startup_config() -> StartupConfig {
    StartupConfig {
        done: Vec::new(),
        user_interaction: Vec::new(),
    }
}

/// Default LogsConfig for missing fields
fn default_logs_config() -> LogsConfig {
    LogsConfig {
        custom: false,
        location: "logs/latest.log".to_string(),
    }
}

/// Default empty docker images map
fn default_docker_images() -> HashMap<String, String> {
    HashMap::new()
}

/// Default exported_at value
fn default_exported_at() -> String {
    "unknown".to_string()
}

/// Pterodactyl Egg JSON structure
/// Based on: https://github.com/pterodactyl/panel/wiki/Egg-JSON-Format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PterodactylEgg {
    #[serde(rename = "_comment")]
    pub comment: Option<String>,
    pub meta: Meta,
    #[serde(default = "default_exported_at")]
    pub exported_at: String,
    pub name: String,
    pub author: String,
    pub description: Option<String>,
    #[serde(deserialize_with = "deserialize_string_or_vec", default)]
    pub features: Vec<String>,
    #[serde(default = "default_docker_images")]
    pub docker_images: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_string_or_vec", default)]
    pub file_denylist: Vec<String>,
    pub startup: String,
    pub config: Config,
    #[serde(default)]
    pub scripts: Scripts,
    #[serde(default)]
    pub variables: Vec<EggVariable>,
}

/// Custom deserializer for version that can be string, integer, or boolean
fn deserialize_flexible_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value;

    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Null => Ok(String::new()),
        _ => Ok(String::new()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    #[serde(deserialize_with = "deserialize_flexible_string")]
    pub version: String,
    pub update_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(
        deserialize_with = "deserialize_json_string_or_object",
        default = "default_files"
    )]
    pub files: HashMap<String, FileConfig>,
    #[serde(
        deserialize_with = "deserialize_json_string_or_object",
        default = "default_startup_config"
    )]
    pub startup: StartupConfig,
    #[serde(default)]
    pub stop: String,
    #[serde(
        deserialize_with = "deserialize_json_string_or_object",
        default = "default_logs_config"
    )]
    pub logs: LogsConfig,
    #[serde(default)]
    pub file_denylist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileConfig {
    #[serde(default)]
    pub parser: String,
    #[serde(default)]
    pub find: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StartupConfig {
    #[serde(deserialize_with = "deserialize_string_or_vec", default)]
    pub done: Vec<String>,
    #[serde(deserialize_with = "deserialize_string_or_vec", default)]
    pub user_interaction: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LogsConfig {
    #[serde(default)]
    pub custom: bool,
    #[serde(default = "default_log_location")]
    pub location: String,
}

fn default_log_location() -> String {
    "logs/latest.log".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Scripts {
    #[serde(default)]
    pub installation: InstallationScript,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallationScript {
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub container: String,
    #[serde(default)]
    pub entrypoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EggVariable {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub env_variable: String,
    #[serde(default)]
    pub default_value: String,
    #[serde(default)]
    pub user_viewable: bool,
    #[serde(default)]
    pub user_editable: bool,
    #[serde(default)]
    pub rules: String,
    pub field_type: Option<String>,
}

impl PterodactylEgg {
    /// Parse from JSON string
    /// Handles some malformed JSON like duplicate keys by preprocessing
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        // Parse to Value first (handles duplicate keys by keeping last)
        // then convert to our struct
        let value: serde_json::Value = serde_json::from_str(json)?;
        Ok(serde_json::from_value(value)?)
    }

    /// Load from file
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_json(&content)
    }

    /// Get the default docker image
    pub fn default_image(&self) -> Option<String> {
        // Try to find "latest" or first image
        self.docker_images
            .get("latest")
            .or_else(|| self.docker_images.values().next())
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_egg() {
        let json = r##"{
            "_comment": "Test egg",
            "meta": {
                "version": "PTDL_v2",
                "update_url": null
            },
            "exported_at": "2024-01-01T00:00:00+00:00",
            "name": "Test Server",
            "author": "test@example.com",
            "description": "A test server",
            "features": null,
            "docker_images": {
                "latest": "ghcr.io/pterodactyl/yolks:test"
            },
            "file_denylist": [],
            "startup": "./start.sh",
            "config": {
                "files": {},
                "startup": {
                    "done": ["Server started"],
                    "user_interaction": []
                },
                "stop": "stop",
                "logs": {
                    "custom": false,
                    "location": "logs/latest.log"
                },
                "file_denylist": []
            },
            "scripts": {
                "installation": {
                    "script": "#!/bin/bash\necho test",
                    "container": "alpine:latest",
                    "entrypoint": "bash"
                }
            },
            "variables": []
        }"##;

        let egg = PterodactylEgg::from_json(json).unwrap();
        assert_eq!(egg.name, "Test Server");
        assert_eq!(egg.author, "test@example.com");
    }
}
