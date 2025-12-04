use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// Custom deserializer for JSON-encoded strings
/// Pterodactyl stores some fields as JSON strings that need to be parsed
fn deserialize_json_string<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: for<'a> Deserialize<'a>,
{
    use serde::de::Error;
    let s = String::deserialize(deserializer)?;
    serde_json::from_str(&s).map_err(Error::custom)
}

/// Custom deserializer that handles both string and array of strings
fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    use serde_json::Value;

    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => Ok(vec![s]),
        Value::Array(arr) => {
            arr.into_iter()
                .map(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .ok_or_else(|| Error::custom("Array element is not a string"))
                })
                .collect()
        }
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

/// Pterodactyl Egg JSON structure
/// Based on: https://github.com/pterodactyl/panel/wiki/Egg-JSON-Format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PterodactylEgg {
    #[serde(rename = "_comment")]
    pub comment: Option<String>,
    pub meta: Meta,
    pub exported_at: String,
    pub name: String,
    pub author: String,
    pub description: Option<String>,
    pub features: Option<Vec<String>>,
    pub docker_images: HashMap<String, String>,
    #[serde(default)]
    pub file_denylist: Vec<String>,
    pub startup: String,
    pub config: Config,
    pub scripts: Scripts,
    pub variables: Vec<EggVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub version: String,
    pub update_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(deserialize_with = "deserialize_json_string", default = "default_files")]
    pub files: HashMap<String, FileConfig>,
    #[serde(deserialize_with = "deserialize_json_string", default = "default_startup_config")]
    pub startup: StartupConfig,
    pub stop: String,
    #[serde(deserialize_with = "deserialize_json_string", default = "default_logs_config")]
    pub logs: LogsConfig,
    #[serde(default)]
    pub file_denylist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConfig {
    pub parser: String,
    pub find: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupConfig {
    #[serde(deserialize_with = "deserialize_string_or_vec", default)]
    pub done: Vec<String>,
    #[serde(deserialize_with = "deserialize_string_or_vec", default)]
    pub user_interaction: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsConfig {
    #[serde(default)]
    pub custom: bool,
    #[serde(default = "default_log_location")]
    pub location: String,
}

fn default_log_location() -> String {
    "logs/latest.log".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scripts {
    pub installation: InstallationScript,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationScript {
    pub script: String,
    pub container: String,
    pub entrypoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EggVariable {
    pub name: String,
    pub description: String,
    pub env_variable: String,
    pub default_value: String,
    pub user_viewable: bool,
    pub user_editable: bool,
    pub rules: String,
    pub field_type: Option<String>,
}

impl PterodactylEgg {
    /// Parse from JSON string
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(json)?)
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
