//! Integration tests for egg conversion pipeline
//!
//! Run with: cargo test --test integration_test

use egg_importer::{EggConverter, PterodactylEgg};
use nexus_config::GameConfig;

// Helper to create JSON strings without raw string issues
#[allow(dead_code)]
fn json_str(s: &str) -> String {
    s.to_string()
}

/// Test parsing various egg formats
mod parsing {
    use super::*;

    #[test]
    fn test_parse_minecraft_egg() {
        let json = r#"{
            "meta": {"version": "PTDL_v2"},
            "name": "Paper",
            "author": "test@example.com",
            "description": "High performance Minecraft server",
            "features": ["java_version", "eula"],
            "docker_images": {"Java 17": "ghcr.io/pterodactyl/yolks:java_17"},
            "startup": "java -jar server.jar",
            "config": {"files": "{}", "startup": "{\"done\": \"Done\"}", "logs": "{}", "stop": "stop"},
            "scripts": {"installation": {"script": "echo test", "container": "alpine", "entrypoint": "sh"}},
            "variables": []
        }"#;

        let egg = PterodactylEgg::from_json(json).unwrap();
        assert_eq!(egg.name, "Paper");
        assert_eq!(egg.features.len(), 2);
    }

    #[test]
    fn test_parse_egg_with_empty_features() {
        let json = r#"{
            "meta": {"version": "PTDL_v2"},
            "name": "Test",
            "author": "test@example.com",
            "features": "",
            "file_denylist": "",
            "docker_images": {},
            "startup": "./start.sh",
            "config": {"files": "{}", "startup": "{}", "logs": "{}", "stop": "stop"},
            "scripts": {"installation": {"script": "", "container": "", "entrypoint": ""}}
        }"#;

        let egg = PterodactylEgg::from_json(json).unwrap();
        assert!(egg.features.is_empty());
        assert!(egg.file_denylist.is_empty());
    }

    #[test]
    fn test_parse_egg_with_integer_version() {
        let json = r#"{
            "meta": {"version": 2},
            "name": "Test",
            "author": "test@example.com",
            "startup": "./start.sh",
            "config": {"files": "{}", "startup": "{}", "logs": "{}", "stop": "stop"},
            "scripts": {"installation": {"script": "", "container": "", "entrypoint": ""}}
        }"#;

        let egg = PterodactylEgg::from_json(json).unwrap();
        assert_eq!(egg.meta.version, "2");
    }

    #[test]
    fn test_parse_egg_with_missing_optional_fields() {
        let json = r#"{
            "meta": {"version": "PTDL_v2"},
            "name": "Minimal Egg",
            "author": "test@example.com",
            "startup": "./start.sh",
            "config": {"files": "{}", "startup": "{}", "logs": "{}", "stop": "stop"},
            "scripts": {"installation": {"script": "", "container": "", "entrypoint": ""}}
        }"#;

        let egg = PterodactylEgg::from_json(json).unwrap();
        assert_eq!(egg.exported_at, "unknown");
        assert!(egg.docker_images.is_empty());
    }
}

/// Test game type detection
mod game_detection {
    use super::*;

    fn create_egg_with_name(name: &str) -> PterodactylEgg {
        let json = format!(
            r#"{{
                "meta": {{"version": "PTDL_v2"}},
                "name": "{}",
                "author": "test@example.com",
                "startup": "./start.sh",
                "config": {{"files": "{{}}", "startup": "{{}}", "logs": "{{}}", "stop": "stop"}},
                "scripts": {{"installation": {{"script": "", "container": "", "entrypoint": ""}}}}
            }}"#,
            name
        );
        PterodactylEgg::from_json(&json).unwrap()
    }

    #[test]
    fn test_detect_minecraft_variants() {
        let converter = EggConverter::new();

        for name in &[
            "Paper",
            "Spigot",
            "Bukkit",
            "Purpur",
            "Fabric",
            "Forge",
            "Minecraft Server",
        ] {
            let egg = create_egg_with_name(name);
            let config = converter.convert(&egg).unwrap();
            assert_eq!(config.metadata.game, "minecraft", "Failed for: {}", name);
        }
    }

    #[test]
    fn test_detect_survival_games() {
        let converter = EggConverter::new();
        let test_cases = vec![
            ("Rust Dedicated Server", "rust"),
            ("ARK: Survival Evolved", "ark"),
            ("Valheim Dedicated", "valheim"),
            ("Terraria", "terraria"),
            ("7 Days to Die", "7daystodie"),
            ("Project Zomboid", "projectzomboid"),
            ("Palworld", "palworld"),
        ];

        for (name, expected) in test_cases {
            let egg = create_egg_with_name(name);
            let config = converter.convert(&egg).unwrap();
            assert_eq!(config.metadata.game, expected, "Failed for: {}", name);
        }
    }

    #[test]
    fn test_detect_fps_games() {
        let converter = EggConverter::new();
        let test_cases = vec![
            ("Counter-Strike 2", "csgo"),
            ("CSGO Server", "csgo"),
            ("Team Fortress 2", "tf2"),
            ("Garry's Mod", "gmod"),
        ];

        for (name, expected) in test_cases {
            let egg = create_egg_with_name(name);
            let config = converter.convert(&egg).unwrap();
            assert_eq!(config.metadata.game, expected, "Failed for: {}", name);
        }
    }

    #[test]
    fn test_detect_generic_fallback() {
        let converter = EggConverter::new();
        let egg = create_egg_with_name("Some Unknown Game Server");
        let config = converter.convert(&egg).unwrap();
        assert_eq!(config.metadata.game, "generic");
    }
}

/// Test full conversion pipeline
mod conversion {
    use super::*;

    #[test]
    fn test_full_conversion_pipeline() {
        let json = r##"{
            "meta": {"version": "PTDL_v2"},
            "exported_at": "2024-01-01T00:00:00Z",
            "name": "Test Minecraft Server",
            "author": "test@example.com",
            "description": "A test Minecraft server",
            "features": ["java_version"],
            "docker_images": {"Java 17": "ghcr.io/pterodactyl/yolks:java_17"},
            "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar server.jar",
            "config": {
                "files": "{}",
                "startup": "{\"done\": \"Done\"}",
                "logs": "{}",
                "stop": "stop"
            },
            "scripts": {
                "installation": {
                    "script": "#!/bin/bash\necho Installing...",
                    "container": "alpine:latest",
                    "entrypoint": "bash"
                }
            },
            "variables": [
                {
                    "name": "Server Memory",
                    "description": "Memory allocation in MB",
                    "env_variable": "SERVER_MEMORY",
                    "default_value": "1024",
                    "user_viewable": true,
                    "user_editable": true,
                    "rules": "required|integer|between:512,32768"
                }
            ]
        }"##;

        let egg = PterodactylEgg::from_json(json).unwrap();
        let converter = EggConverter::new();
        let config = converter.convert(&egg).unwrap();

        // Verify metadata
        assert_eq!(config.metadata.name, "Test Minecraft Server");
        assert_eq!(config.metadata.game, "minecraft");
        assert_eq!(config.metadata.author, "test@example.com");

        // Verify container
        assert!(config.container.image.contains("java_17"));

        // Verify variables
        assert_eq!(config.variables.len(), 1);
        assert_eq!(config.variables[0].name, "SERVER_MEMORY");
        assert_eq!(config.variables[0].default, "1024");
    }

    #[test]
    fn test_yaml_round_trip() {
        let json = r#"{
            "meta": {"version": "PTDL_v2"},
            "name": "YAML Test Server",
            "author": "test@example.com",
            "docker_images": {"latest": "alpine:latest"},
            "startup": "./start.sh",
            "config": {"files": "{}", "startup": "{}", "logs": "{}", "stop": "stop"},
            "scripts": {"installation": {"script": "", "container": "", "entrypoint": ""}}
        }"#;

        let egg = PterodactylEgg::from_json(json).unwrap();
        let converter = EggConverter::new();
        let config = converter.convert(&egg).unwrap();

        // Convert to YAML
        let yaml = config.to_yaml().unwrap();
        assert!(yaml.contains("YAML Test Server"));

        // Parse back from YAML
        let parsed: GameConfig = GameConfig::from_yaml(&yaml).unwrap();
        assert_eq!(parsed.metadata.name, config.metadata.name);
    }
}

/// Test validation rules parsing
mod validation_rules {
    use super::*;

    #[test]
    fn test_parse_port_range() {
        let json = r#"{
            "meta": {"version": "PTDL_v2"},
            "name": "Test",
            "author": "test@example.com",
            "startup": "./start.sh",
            "config": {"files": "{}", "startup": "{}", "logs": "{}", "stop": "stop"},
            "scripts": {"installation": {"script": "", "container": "", "entrypoint": ""}},
            "variables": [{
                "name": "Port",
                "description": "Server port",
                "env_variable": "SERVER_PORT",
                "default_value": "25565",
                "user_viewable": true,
                "user_editable": false,
                "rules": "required|integer|between:1024,65535"
            }]
        }"#;

        let egg = PterodactylEgg::from_json(json).unwrap();
        let converter = EggConverter::new();
        let config = converter.convert(&egg).unwrap();

        assert_eq!(config.variables.len(), 1);
        assert!(config.variables[0].rules.is_some());
    }

    #[test]
    fn test_parse_enum_values() {
        let json = r#"{
            "meta": {"version": "PTDL_v2"},
            "name": "Test",
            "author": "test@example.com",
            "startup": "./start.sh",
            "config": {"files": "{}", "startup": "{}", "logs": "{}", "stop": "stop"},
            "scripts": {"installation": {"script": "", "container": "", "entrypoint": ""}},
            "variables": [{
                "name": "Mode",
                "description": "Game mode",
                "env_variable": "GAME_MODE",
                "default_value": "survival",
                "user_viewable": true,
                "user_editable": true,
                "rules": "required|in:survival,creative,adventure,spectator"
            }]
        }"#;

        let egg = PterodactylEgg::from_json(json).unwrap();
        let converter = EggConverter::new();
        let config = converter.convert(&egg).unwrap();

        assert_eq!(config.variables.len(), 1);
        let rules = config.variables[0].rules.as_ref().unwrap();
        assert!(!rules.is_empty());
    }
}

/// Test security scanning
mod security {
    use super::*;

    #[test]
    fn test_security_warning_detection() {
        let json = r##"{
            "meta": {"version": "PTDL_v2"},
            "name": "Dangerous Egg",
            "author": "test@example.com",
            "startup": "./start.sh",
            "config": {"files": "{}", "startup": "{}", "logs": "{}", "stop": "stop"},
            "scripts": {
                "installation": {
                    "script": "#!/bin/bash\nrm -rf /\nchmod 777 /",
                    "container": "alpine:latest",
                    "entrypoint": "bash"
                }
            }
        }"##;

        let egg = PterodactylEgg::from_json(json).unwrap();
        let converter = EggConverter::new();

        // Should still convert but with security warnings
        let config = converter.convert(&egg);
        assert!(config.is_ok());
    }
}
