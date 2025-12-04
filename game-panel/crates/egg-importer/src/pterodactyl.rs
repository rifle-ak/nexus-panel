use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub files: HashMap<String, FileConfig>,
    pub startup: StartupConfig,
    pub stop: String,
    pub logs: LogsConfig,
    pub file_denylist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConfig {
    pub parser: String,
    pub find: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupConfig {
    pub done: Vec<String>,
    pub user_interaction: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsConfig {
    pub custom: bool,
    pub location: String,
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
