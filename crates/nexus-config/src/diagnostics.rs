use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticResult {
    pub category: String,
    pub checks: Vec<Check>,
    pub overall_status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub message: String,
    pub solution: Option<String>,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Info,
}

pub struct Diagnostics;

impl Diagnostics {
    /// Run all diagnostic checks
    pub fn run_all() -> Vec<DiagnosticResult> {
        vec![
            Self::check_system_info(),
            Self::check_required_commands(),
            Self::check_rust_toolchain(),
            Self::check_file_system(),
            Self::check_network(),
            Self::check_container_runtime(),
        ]
    }

    fn check_system_info() -> DiagnosticResult {
        let mut checks = Vec::new();

        // OS info
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        checks.push(Check {
            name: "Operating System".to_string(),
            status: Status::Info,
            message: format!("{} ({})", os, arch),
            solution: None,
            details: Some(format!("OS: {}, Architecture: {}", os, arch)),
        });

        // Memory
        if let Ok(output) = Command::new("free").arg("-h").output() {
            let mem_info = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = mem_info.lines().nth(1) {
                checks.push(Check {
                    name: "System Memory".to_string(),
                    status: Status::Info,
                    message: format!("Memory info: {}", line.trim()),
                    solution: None,
                    details: Some(mem_info.to_string()),
                });
            }
        }

        // CPU info
        if let Ok(output) = Command::new("nproc").output() {
            let cpus = String::from_utf8_lossy(&output.stdout).trim().to_string();
            checks.push(Check {
                name: "CPU Cores".to_string(),
                status: Status::Info,
                message: format!("{} cores available", cpus),
                solution: None,
                details: None,
            });
        }

        DiagnosticResult {
            category: "System Information".to_string(),
            checks,
            overall_status: Status::Info,
        }
    }

    fn check_required_commands() -> DiagnosticResult {
        let mut checks = Vec::new();
        let required = vec![
            ("git", "Git version control", "apt-get install git (Ubuntu) or brew install git (macOS)"),
            ("curl", "HTTP client for downloads", "apt-get install curl (Ubuntu) or brew install curl (macOS)"),
        ];

        let optional = vec![
            ("docker", "Docker container runtime", "See https://docs.docker.com/engine/install/"),
            ("jq", "JSON processor for validation", "apt-get install jq (Ubuntu) or brew install jq (macOS)"),
            ("yamllint", "YAML validator", "pip install yamllint"),
        ];

        for (cmd, desc, install) in required {
            let check = Self::check_command_exists(cmd, desc, install, true);
            checks.push(check);
        }

        for (cmd, desc, install) in optional {
            let check = Self::check_command_exists(cmd, desc, install, false);
            checks.push(check);
        }

        let overall_status = if checks.iter().any(|c| c.status == Status::Fail) {
            Status::Fail
        } else if checks.iter().any(|c| c.status == Status::Warn) {
            Status::Warn
        } else {
            Status::Pass
        };

        DiagnosticResult {
            category: "Required Commands".to_string(),
            checks,
            overall_status,
        }
    }

    fn check_command_exists(cmd: &str, desc: &str, install: &str, required: bool) -> Check {
        match Command::new("which").arg(cmd).output() {
            Ok(output) if output.status.success() => {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                // Get version if possible
                let version = Command::new(cmd)
                    .arg("--version")
                    .output()
                    .ok()
                    .and_then(|v| {
                        let version_str = String::from_utf8_lossy(&v.stdout);
                        version_str.lines().next().map(|s| s.to_string())
                    });

                Check {
                    name: format!("{} ({})", cmd, desc),
                    status: Status::Pass,
                    message: format!("✓ Found at {}", path),
                    solution: None,
                    details: version,
                }
            }
            _ => Check {
                name: format!("{} ({})", cmd, desc),
                status: if required { Status::Fail } else { Status::Warn },
                message: format!("✗ Not found"),
                solution: Some(format!("Install {}: {}", cmd, install)),
                details: None,
            },
        }
    }

    fn check_rust_toolchain() -> DiagnosticResult {
        let mut checks = Vec::new();

        // Check rustc
        match Command::new("rustc").arg("--version").output() {
            Ok(output) => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                checks.push(Check {
                    name: "Rust Compiler".to_string(),
                    status: Status::Pass,
                    message: format!("✓ {}", version),
                    solution: None,
                    details: None,
                });
            }
            Err(_) => {
                checks.push(Check {
                    name: "Rust Compiler".to_string(),
                    status: Status::Fail,
                    message: "✗ Rust not installed".to_string(),
                    solution: Some(
                        "Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh".to_string()
                    ),
                    details: None,
                });
            }
        }

        // Check cargo
        match Command::new("cargo").arg("--version").output() {
            Ok(output) => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                checks.push(Check {
                    name: "Cargo Package Manager".to_string(),
                    status: Status::Pass,
                    message: format!("✓ {}", version),
                    solution: None,
                    details: None,
                });
            }
            Err(_) => {
                checks.push(Check {
                    name: "Cargo Package Manager".to_string(),
                    status: Status::Warn,
                    message: "✗ Cargo not found".to_string(),
                    solution: Some("Cargo should be installed with Rust".to_string()),
                    details: None,
                });
            }
        }

        let overall_status = if checks.iter().any(|c| c.status == Status::Fail) {
            Status::Fail
        } else {
            Status::Pass
        };

        DiagnosticResult {
            category: "Rust Toolchain".to_string(),
            checks,
            overall_status,
        }
    }

    fn check_file_system() -> DiagnosticResult {
        let mut checks = Vec::new();

        // Check current directory permissions
        let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let can_read = current_dir.read_dir().is_ok();
        checks.push(Check {
            name: "Current Directory Access".to_string(),
            status: if can_read { Status::Pass } else { Status::Fail },
            message: if can_read {
                format!("✓ Can read {}", current_dir.display())
            } else {
                format!("✗ Cannot read {}", current_dir.display())
            },
            solution: if !can_read {
                Some("Check directory permissions: ls -ld .".to_string())
            } else {
                None
            },
            details: None,
        });

        // Check write permissions
        let test_file = current_dir.join(".nexus-panel-test");
        let can_write = std::fs::write(&test_file, "test").is_ok();
        if can_write {
            let _ = std::fs::remove_file(&test_file);
        }
        checks.push(Check {
            name: "Write Permissions".to_string(),
            status: if can_write { Status::Pass } else { Status::Fail },
            message: if can_write {
                "✓ Can write to current directory".to_string()
            } else {
                "✗ Cannot write to current directory".to_string()
            },
            solution: if !can_write {
                Some("Check write permissions: touch test.txt".to_string())
            } else {
                None
            },
            details: None,
        });

        // Check disk space
        if let Ok(output) = Command::new("df").arg("-h").arg(".").output() {
            let df_output = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = df_output.lines().nth(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 {
                    let available = parts[3];
                    let used_percent = parts[4];
                    checks.push(Check {
                        name: "Disk Space".to_string(),
                        status: Status::Info,
                        message: format!("{} available ({}used)", available, used_percent),
                        solution: None,
                        details: Some(df_output.to_string()),
                    });
                }
            }
        }

        let overall_status = if checks.iter().any(|c| c.status == Status::Fail) {
            Status::Fail
        } else {
            Status::Pass
        };

        DiagnosticResult {
            category: "File System".to_string(),
            checks,
            overall_status,
        }
    }

    fn check_network() -> DiagnosticResult {
        let mut checks = Vec::new();

        // Check DNS resolution
        let dns_check = Command::new("ping")
            .args(["-c", "1", "-W", "2", "1.1.1.1"])
            .output();

        let dns_ok = dns_check.as_ref().map(|o| o.status.success()).unwrap_or(false);

        checks.push(Check {
            name: "Network Connectivity".to_string(),
            status: if dns_ok {
                Status::Pass
            } else {
                Status::Warn
            },
            message: if dns_ok {
                "✓ Network is accessible".to_string()
            } else {
                "⚠ Network may not be accessible".to_string()
            },
            solution: Some("Check your internet connection".to_string()),
            details: None,
        });

        // Check GitHub access
        let github_check = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-I", "https://github.com"])
            .output();

        if let Ok(output) = github_check {
            let status_code = String::from_utf8_lossy(&output.stdout).trim().to_string();
            checks.push(Check {
                name: "GitHub Access".to_string(),
                status: if status_code.starts_with('2') || status_code.starts_with('3') {
                    Status::Pass
                } else {
                    Status::Warn
                },
                message: format!("HTTP {}", status_code),
                solution: if !status_code.starts_with('2') && !status_code.starts_with('3') {
                    Some("Check firewall or proxy settings".to_string())
                } else {
                    None
                },
                details: None,
            });
        }

        let overall_status = if checks.iter().any(|c| c.status == Status::Fail) {
            Status::Fail
        } else if checks.iter().any(|c| c.status == Status::Warn) {
            Status::Warn
        } else {
            Status::Pass
        };

        DiagnosticResult {
            category: "Network".to_string(),
            checks,
            overall_status,
        }
    }

    fn check_container_runtime() -> DiagnosticResult {
        let mut checks = Vec::new();

        // Check Docker
        match Command::new("docker").arg("--version").output() {
            Ok(output) => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                checks.push(Check {
                    name: "Docker".to_string(),
                    status: Status::Pass,
                    message: format!("✓ {}", version),
                    solution: None,
                    details: None,
                });

                // Check if Docker daemon is running
                match Command::new("docker").arg("ps").output() {
                    Ok(ps_output) if ps_output.status.success() => {
                        checks.push(Check {
                            name: "Docker Daemon".to_string(),
                            status: Status::Pass,
                            message: "✓ Running".to_string(),
                            solution: None,
                            details: None,
                        });
                    }
                    _ => {
                        checks.push(Check {
                            name: "Docker Daemon".to_string(),
                            status: Status::Warn,
                            message: "⚠ Not running or not accessible".to_string(),
                            solution: Some("Start Docker: sudo systemctl start docker".to_string()),
                            details: None,
                        });
                    }
                }
            }
            Err(_) => {
                checks.push(Check {
                    name: "Docker".to_string(),
                    status: Status::Info,
                    message: "Not installed (optional for development)".to_string(),
                    solution: Some("Install Docker: https://docs.docker.com/engine/install/".to_string()),
                    details: None,
                });
            }
        }

        DiagnosticResult {
            category: "Container Runtime".to_string(),
            checks,
            overall_status: Status::Info, // Optional for now
        }
    }

    /// Print diagnostics results in a human-readable format
    pub fn print_results(results: &[DiagnosticResult]) {
        println!("\n🔍 System Diagnostics\n");
        println!("{}", "=".repeat(80));

        for result in results {
            // Category header
            let status_icon = match result.overall_status {
                Status::Pass => "✅",
                Status::Warn => "⚠️",
                Status::Fail => "❌",
                Status::Info => "ℹ️",
            };
            println!("\n{} {}\n", status_icon, result.category);

            // Individual checks
            for check in &result.checks {
                let indent = "  ";
                let status_icon = match check.status {
                    Status::Pass => "✓",
                    Status::Warn => "⚠",
                    Status::Fail => "✗",
                    Status::Info => "ℹ",
                };

                println!("{}{} {}: {}", indent, status_icon, check.name, check.message);

                if let Some(solution) = &check.solution {
                    println!("{}  💡 {}", indent, solution);
                }

                if let Some(details) = &check.details {
                    println!("{}  📝 {}", indent, details);
                }
            }
        }

        println!("\n{}", "=".repeat(80));

        // Summary
        let total_checks: usize = results.iter().map(|r| r.checks.len()).sum();
        let failed = results
            .iter()
            .flat_map(|r| &r.checks)
            .filter(|c| c.status == Status::Fail)
            .count();
        let warnings = results
            .iter()
            .flat_map(|r| &r.checks)
            .filter(|c| c.status == Status::Warn)
            .count();

        println!("\n📊 Summary:");
        println!("   Total checks: {}", total_checks);
        if failed > 0 {
            println!("   ❌ Failed: {}", failed);
        }
        if warnings > 0 {
            println!("   ⚠️  Warnings: {}", warnings);
        }
        if failed == 0 && warnings == 0 {
            println!("   ✅ All checks passed!");
        }

        if failed > 0 {
            println!("\n⚠️  Please fix the failed checks before proceeding.");
        }

        println!();
    }
}
