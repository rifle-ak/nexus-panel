use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use egg_importer::{EggConverter, PterodactylEgg};
use game_config::{Diagnostics, GameConfig, GamePanelError};
use std::path::{Path, PathBuf};
use tracing_subscriber;

#[derive(Parser)]
#[command(name = "egg-importer")]
#[command(about = "Import Pterodactyl eggs to native game configs", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run system diagnostics
    Diagnose {
        /// Output format: text, json
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Convert a single egg file
    Convert {
        /// Path to the Pterodactyl egg JSON file
        #[arg(short, long)]
        input: PathBuf,

        /// Output path for the native YAML config
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Skip security scanning
        #[arg(long)]
        no_security_scan: bool,

        /// Don't add default firewall rules
        #[arg(long)]
        no_firewall_rules: bool,
    },

    /// Bulk import eggs from a directory
    Import {
        /// Directory containing Pterodactyl egg JSON files
        #[arg(short, long)]
        input_dir: PathBuf,

        /// Output directory for native YAML configs
        #[arg(short, long)]
        output_dir: PathBuf,

        /// Skip security scanning
        #[arg(long)]
        no_security_scan: bool,

        /// Don't add default firewall rules
        #[arg(long)]
        no_firewall_rules: bool,

        /// Continue on errors
        #[arg(long)]
        continue_on_error: bool,
    },

    /// Clone and import eggs from a git repository
    Clone {
        /// Git repository URL (e.g., https://github.com/parkervcp/eggs)
        #[arg(short, long)]
        repo: String,

        /// Output directory for native YAML configs
        #[arg(short, long)]
        output_dir: PathBuf,

        /// Skip security scanning
        #[arg(long)]
        no_security_scan: bool,

        /// Don't add default firewall rules
        #[arg(long)]
        no_firewall_rules: bool,
    },

    /// Validate a native game config
    Validate {
        /// Path to the native YAML config
        #[arg(short, long)]
        input: PathBuf,
    },
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Diagnose { format } => {
            run_diagnostics(&format)?;
        }
        Commands::Convert {
            input,
            output,
            no_security_scan,
            no_firewall_rules,
        } => {
            convert_single_egg(&input, output.as_deref(), !no_security_scan, !no_firewall_rules)?;
        }
        Commands::Import {
            input_dir,
            output_dir,
            no_security_scan,
            no_firewall_rules,
            continue_on_error,
        } => {
            import_directory(
                &input_dir,
                &output_dir,
                !no_security_scan,
                !no_firewall_rules,
                continue_on_error,
            )?;
        }
        Commands::Clone {
            repo,
            output_dir,
            no_security_scan,
            no_firewall_rules,
        } => {
            clone_and_import(&repo, &output_dir, !no_security_scan, !no_firewall_rules)?;
        }
        Commands::Validate { input } => {
            validate_config(&input)?;
        }
    }

    Ok(())
}

fn run_diagnostics(format: &str) -> Result<()> {
    let results = Diagnostics::run_all();

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(&results)?;
            println!("{}", json);
        }
        _ => {
            Diagnostics::print_results(&results);
        }
    }

    // Exit with error code if any checks failed
    let has_failures = results
        .iter()
        .flat_map(|r| &r.checks)
        .any(|c| matches!(c.status, game_config::diagnostics::Status::Fail));

    if has_failures {
        std::process::exit(1);
    }

    Ok(())
}

fn convert_single_egg(
    input: &Path,
    output: Option<&Path>,
    security_scan: bool,
    add_firewall_rules: bool,
) -> Result<()> {
    println!("📦 Converting egg: {}", input.display());

    // Load egg
    let egg = PterodactylEgg::from_file(input).map_err(|e| {
        GamePanelError::InvalidEggJson {
            path: input.display().to_string(),
            error: e.to_string(),
        }
    })?;

    // Convert
    let converter = EggConverter::new()
        .security_scan(security_scan)
        .add_firewall_rules(add_firewall_rules);

    let config = converter.convert(&egg).map_err(|e| {
        GamePanelError::ConversionError {
            egg_name: egg.name.clone(),
            egg_path: input.display().to_string(),
            reason: e.to_string(),
            solution: "Check the egg format and ensure all required fields are present. Run with RUST_LOG=debug for more details.".to_string(),
        }
    })?;

    // Determine output path
    let output_path = match output {
        Some(p) => p.to_path_buf(),
        None => {
            let stem = input.file_stem().unwrap().to_str().unwrap();
            PathBuf::from(format!("{}.yaml", stem))
        }
    };

    // Write to file
    let yaml = config.to_yaml()?;
    std::fs::write(&output_path, yaml).map_err(|e| {
        let dir = output_path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".to_string());
        GamePanelError::FileWriteError {
            path: output_path.display().to_string(),
            dir,
            error: e.to_string(),
        }
    })?;

    println!("✅ Converted to: {}", output_path.display());
    println!("   Game: {}", config.metadata.game);
    println!("   Variables: {}", config.variables.len());
    println!("   Ports: {}", config.networking.ports.len());
    
    if security_scan {
        println!("   🔒 Security: Scanned and hardened");
    }

    Ok(())
}

fn import_directory(
    input_dir: &Path,
    output_dir: &Path,
    security_scan: bool,
    add_firewall_rules: bool,
    continue_on_error: bool,
) -> Result<()> {
    println!("📂 Importing eggs from: {}", input_dir.display());
    println!("📤 Output directory: {}", output_dir.display());

    // Create output directory
    std::fs::create_dir_all(output_dir)?;

    let converter = EggConverter::new()
        .security_scan(security_scan)
        .add_firewall_rules(add_firewall_rules);

    let mut success_count = 0;
    let mut error_count = 0;

    // Find all JSON files recursively
    let egg_files = find_egg_files(input_dir)?;
    println!("🔍 Found {} egg files\n", egg_files.len());

    for egg_path in egg_files {
        let relative_path = egg_path.strip_prefix(input_dir).unwrap();
        print!("Converting {}... ", relative_path.display());

        match process_egg_file(&egg_path, output_dir, &converter) {
            Ok(output_path) => {
                println!("✅ -> {}", output_path.file_name().unwrap().to_str().unwrap());
                success_count += 1;
            }
            Err(e) => {
                println!("❌ {}", e);
                error_count += 1;
                if !continue_on_error {
                    return Err(e);
                }
            }
        }
    }

    println!("\n📊 Summary:");
    println!("   ✅ Success: {}", success_count);
    println!("   ❌ Errors: {}", error_count);

    Ok(())
}

fn find_egg_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut egg_files = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Recursively search subdirectories
            egg_files.extend(find_egg_files(&path)?);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            egg_files.push(path);
        }
    }

    Ok(egg_files)
}

fn process_egg_file(
    egg_path: &Path,
    output_dir: &Path,
    converter: &EggConverter,
) -> Result<PathBuf> {
    // Load and convert egg
    let egg = PterodactylEgg::from_file(egg_path)?;
    let config = converter.convert(&egg)?;

    // Create output path maintaining directory structure
    let stem = egg_path.file_stem().unwrap().to_str().unwrap();
    let output_path = output_dir.join(format!("{}.yaml", stem));

    // Write config
    let yaml = config.to_yaml()?;
    std::fs::write(&output_path, yaml)?;

    Ok(output_path)
}

fn clone_and_import(
    repo: &str,
    output_dir: &Path,
    security_scan: bool,
    add_firewall_rules: bool,
) -> Result<()> {
    println!("🌐 Cloning repository: {}", repo);

    // Create temp directory for clone
    let temp_dir = std::env::temp_dir().join("egg-importer-clone");
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }

    // Clone using git command
    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", repo, temp_dir.to_str().unwrap()])
        .status()?;

    if !status.success() {
        anyhow::bail!("Git clone failed");
    }

    println!("✅ Repository cloned\n");

    // Import from cloned directory
    import_directory(&temp_dir, output_dir, security_scan, add_firewall_rules, true)?;

    // Clean up temp directory
    std::fs::remove_dir_all(&temp_dir)?;

    Ok(())
}

fn validate_config(input: &Path) -> Result<()> {
    println!("🔍 Validating config: {}", input.display());

    let yaml = std::fs::read_to_string(input).map_err(|e| {
        GamePanelError::FileReadError {
            path: input.display().to_string(),
            error: e.to_string(),
        }
    })?;

    let config = GameConfig::from_yaml(&yaml).map_err(|e| {
        if let Some(panel_err) = e.downcast_ref::<GamePanelError>() {
            // Already a GamePanelError, just return it
            anyhow::Error::new(panel_err.clone())
        } else {
            // Wrap as InvalidYaml
            anyhow::Error::new(GamePanelError::InvalidYaml {
                path: input.display().to_string(),
                error: e.to_string(),
            })
        }
    })?;

    println!("✅ Config is valid!");
    println!("\n📋 Details:");
    println!("   Name: {}", config.metadata.name);
    println!("   Game: {}", config.metadata.game);
    println!("   Image: {}", config.container.image);
    println!("   Variables: {}", config.variables.len());
    println!("   Ports: {}", config.networking.ports.len());
    println!("   Security rules: {}", config.security.firewall_rules.len());

    // Show warnings if any
    let warnings = check_config_warnings(&config);
    if !warnings.is_empty() {
        println!("\n⚠️  Warnings:");
        for warning in warnings {
            println!("   • {}", warning);
        }
    }

    Ok(())
}

fn check_config_warnings(config: &GameConfig) -> Vec<String> {
    let mut warnings = Vec::new();

    // Check for missing monitoring
    if config.monitoring.is_none() {
        warnings.push("No health check configured".to_string());
    }

    // Check for secrets without proper marking
    for var in &config.variables {
        if (var.name.to_lowercase().contains("password")
            || var.name.to_lowercase().contains("token")
            || var.name.to_lowercase().contains("secret"))
            && !var.secret
        {
            warnings.push(format!(
                "Variable '{}' looks like a secret but is not marked as secret",
                var.name
            ));
        }
    }

    // Check for overly permissive capabilities
    if config.security.capabilities.add.iter().any(|c| c == "ALL") {
        warnings.push("Security: Adding ALL capabilities is dangerous".to_string());
    }

    // Check for missing firewall rules
    if config.security.firewall_rules.is_empty() {
        warnings.push("No firewall rules configured (consider adding rate limiting)".to_string());
    }

    warnings
}
