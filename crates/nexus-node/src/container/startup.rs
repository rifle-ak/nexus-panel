//! Turning a blueprint's `startup` block into the argv a container runs.
//!
//! Blueprints write their command lines the way people write them: one
//! string per flag, with `{{VARIABLE}}` placeholders, so a CS2 blueprint says
//! `-port {{SERVER_PORT}}` rather than two separate tokens. The runtime wants
//! an argv. This module renders the placeholders with the server's variables
//! and splits the result with shell rules, so `+hostname "{{SERVER_NAME}}"`
//! becomes two tokens and a name with spaces stays one.
//!
//! Java servers additionally get their JVM flags from `performance.jvm`: the
//! heap sizes and Aikar's collector settings the blueprint declares, placed
//! before `-jar` where the JVM expects them.

use std::collections::HashMap;

use nexus_config::GameConfig;

use crate::error::{NodeError, Result};
use crate::install::{blueprint_vars, substitute};

/// Split a string into arguments the way a POSIX shell would, without any
/// expansion: whitespace separates, single quotes are literal, double quotes
/// allow backslash escapes, and a backslash outside quotes escapes the next
/// character.
pub fn shell_split(input: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                        None => {
                            return Err(NodeError::InvalidConfig {
                                reason: format!("unterminated single quote in {:?}", input),
                            })
                        }
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(esc @ ('"' | '\\' | '$' | '`')) => current.push(esc),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => {
                                return Err(NodeError::InvalidConfig {
                                    reason: format!("unterminated double quote in {:?}", input),
                                })
                            }
                        },
                        Some(ch) => current.push(ch),
                        None => {
                            return Err(NodeError::InvalidConfig {
                                reason: format!("unterminated double quote in {:?}", input),
                            })
                        }
                    }
                }
            }
            '\\' => {
                in_token = true;
                match chars.next() {
                    Some(ch) => current.push(ch),
                    None => current.push('\\'),
                }
            }
            c if c.is_whitespace() => {
                if in_token {
                    args.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            c => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        args.push(current);
    }
    Ok(args)
}

/// The variables a server's command line is rendered with.
pub fn startup_vars(config: &GameConfig) -> HashMap<String, String> {
    blueprint_vars(config)
}

/// Render `startup.command` + `startup.args` into the argv the container runs.
///
/// Every element is substituted then shell-split, so a blueprint may write
/// its flags as one token or several. An empty variable disappears with its
/// flag's value (`+sv_setsteamaccount {{GSLT_TOKEN}}` with no token yields
/// just `+sv_setsteamaccount`), matching what a shell would do — and what
/// the game engines expect.
pub fn render_argv(config: &GameConfig) -> Result<Vec<String>> {
    let vars = startup_vars(config);

    let mut argv = shell_split(&substitute(&config.startup.command, &vars))?;
    if argv.is_empty() {
        return Err(NodeError::InvalidConfig {
            reason: "startup.command renders to nothing".to_string(),
        });
    }

    let mut rest = Vec::new();
    for arg in &config.startup.args {
        rest.extend(shell_split(&substitute(arg, &vars))?);
    }

    if let Some(flags) = jvm_flags(config, &vars, &rest) {
        argv.extend(flags);
    }
    argv.extend(rest);
    Ok(argv)
}

/// JVM flags for a Java server, rendered and de-duplicated against what the
/// blueprint's own args already set.
fn jvm_flags(
    config: &GameConfig,
    vars: &HashMap<String, String>,
    args: &[String],
) -> Option<Vec<String>> {
    let command = config.startup.command.trim();
    let is_java = std::path::Path::new(command).file_name().map(|f| f == "java").unwrap_or(false);
    if !is_java {
        return None;
    }
    let generated = config.generate_jvm_flags()?;

    let has_xmx = args.iter().any(|a| a.starts_with("-Xmx"));
    let has_xms = args.iter().any(|a| a.starts_with("-Xms"));

    let mut flags = Vec::new();
    for flag in generated {
        let flag = substitute(&flag, vars);
        // A heap size the blueprint left as an unset placeholder, or one the
        // args already carry, is skipped rather than passed as garbage.
        if flag.contains("{{") || flag == "-Xmx" || flag == "-Xms" {
            continue;
        }
        if (flag.starts_with("-Xmx") && has_xmx) || (flag.starts_with("-Xms") && has_xms) {
            continue;
        }
        if args.contains(&flag) {
            continue;
        }
        flags.push(flag);
    }
    Some(flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml_startup: &str, vars: &[(&str, &str)], perf: &str) -> GameConfig {
        let variables: String = vars
            .iter()
            .map(|(n, v)| {
                format!(
                    "  - {{ name: {}, description: x, default: \"{}\" }}\n",
                    n, v
                )
            })
            .collect();
        let yaml = format!(
            r#"
metadata: {{ id: t, name: T, version: "1", game: t, author: t }}
container: {{ image: img }}
resources:
  cpu: {{ min: 500, max: 1000, shares: 1024 }}
  memory: {{ min: 1Gi, max: 2Gi }}
  disk: {{ min: 1Gi }}
startup:
{}
variables:
{}
networking: {{ ports: [] }}
security: {{ capabilities: {{ drop: [], add: [] }} }}
{}
"#,
            yaml_startup, variables, perf
        );
        serde_yaml::from_str(&yaml).expect("test yaml")
    }

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(shell_split("a b  c").unwrap(), vec!["a", "b", "c"]);
        assert_eq!(
            shell_split("+hostname \"My Server\"").unwrap(),
            vec!["+hostname", "My Server"]
        );
        assert_eq!(shell_split("say 'it''s'").unwrap(), vec!["say", "its"]);
        assert_eq!(
            shell_split(r#"x "a\"b" c\ d"#).unwrap(),
            vec!["x", "a\"b", "c d"]
        );
        assert_eq!(shell_split("  ").unwrap(), Vec::<String>::new());
        assert_eq!(shell_split("\"\"").unwrap(), vec![""]);
        assert!(shell_split("unterminated \"quote").is_err());
        assert!(shell_split("unterminated 'quote").is_err());
    }

    #[test]
    fn renders_placeholders_and_splits_flag_values() {
        let c = config(
            "  command: ./cs2\n  args:\n    - -dedicated\n    - -port {{SERVER_PORT}}\n    - +hostname \"{{SERVER_NAME}}\"\n    - +sv_setsteamaccount {{GSLT_TOKEN}}\n  working_dir: /home/container\n",
            &[("SERVER_PORT", "27015"), ("SERVER_NAME", "Acme Deathmatch"), ("GSLT_TOKEN", "")],
            "",
        );
        assert_eq!(
            render_argv(&c).unwrap(),
            vec![
                "./cs2",
                "-dedicated",
                "-port",
                "27015",
                "+hostname",
                "Acme Deathmatch",
                "+sv_setsteamaccount"
            ]
        );
    }

    #[test]
    fn unknown_placeholders_are_left_visible() {
        let c = config(
            "  command: run\n  args: ['{{NOPE}}']\n  working_dir: /x\n",
            &[],
            "",
        );
        assert_eq!(render_argv(&c).unwrap(), vec!["run", "{{NOPE}}"]);
    }

    #[test]
    fn java_gets_its_jvm_flags_before_the_jar() {
        let c = config(
            "  command: java\n  args: ['-jar', 'paper.jar', '--nogui']\n  working_dir: /home/container\n",
            &[("MEMORY", "3072M")],
            "performance:\n  jvm:\n    gc: g1gc\n    initial_heap: \"{{MEMORY}}\"\n    max_heap: \"{{MEMORY}}\"\n    aikar_flags: true\n    flags: ['-Dfoo=bar']\n",
        );
        let argv = render_argv(&c).unwrap();
        assert_eq!(argv[0], "java");
        assert_eq!(argv[1], "-Xms3072M");
        assert_eq!(argv[2], "-Xmx3072M");
        assert!(argv.contains(&"-XX:+UseG1GC".to_string()));
        assert!(argv.contains(&"-XX:+ParallelRefProcEnabled".to_string()));
        assert!(argv.contains(&"-Dfoo=bar".to_string()));
        let jar = argv.iter().position(|a| a == "-jar").unwrap();
        assert!(argv[..jar].iter().all(|a| a.starts_with('-') || a == "java"));
        assert_eq!(&argv[jar..], &["-jar", "paper.jar", "--nogui"]);
    }

    #[test]
    fn jvm_heap_from_args_is_not_duplicated() {
        let c = config(
            "  command: /usr/bin/java\n  args: ['-Xmx1G', '-jar', 'x.jar']\n  working_dir: /h\n",
            &[("MEMORY", "2G")],
            "performance:\n  jvm:\n    gc: zgc\n    max_heap: \"{{MEMORY}}\"\n",
        );
        let argv = render_argv(&c).unwrap();
        assert_eq!(argv.iter().filter(|a| a.starts_with("-Xmx")).count(), 1);
        assert!(argv.contains(&"-Xmx1G".to_string()));
        assert!(argv.contains(&"-XX:+UseZGC".to_string()));
    }

    #[test]
    fn unset_heap_placeholder_is_dropped_not_passed() {
        let c = config(
            "  command: java\n  args: ['-jar', 'x.jar']\n  working_dir: /h\n",
            &[],
            "performance:\n  jvm:\n    gc: g1gc\n    max_heap: \"{{MEMORY}}\"\n",
        );
        let argv = render_argv(&c).unwrap();
        assert!(!argv.iter().any(|a| a.contains("{{")));
        assert!(!argv.iter().any(|a| a.starts_with("-Xmx")));
    }

    #[test]
    fn every_shipped_blueprint_renders_without_placeholders() {
        for entry in
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../blueprints")).unwrap()
        {
            let path = entry.unwrap().path();
            let yaml = std::fs::read_to_string(&path).unwrap();
            let config: GameConfig = serde_yaml::from_str(&yaml).unwrap();
            let argv = render_argv(&config).unwrap_or_else(|e| panic!("{:?}: {}", path, e));
            assert!(!argv.is_empty());
            for arg in &argv {
                assert!(
                    !arg.contains("{{"),
                    "{:?} renders an unresolved placeholder in {:?}",
                    path,
                    arg
                );
                assert!(
                    !arg.contains(char::is_whitespace) || arg.chars().any(|c| !c.is_whitespace()),
                    "{:?} renders a blank argument",
                    path
                );
            }
        }
    }
}
