use thiserror::Error;

/// Errors with built-in solutions - no guessing games
#[derive(Error, Debug, Clone)]
pub enum NexusPanelError {
    // ==================== File System Errors ====================
    #[error("Failed to read file: {path}\n\n💡 Solution:\n  • Check if the file exists: ls -la {path}\n  • Check file permissions: chmod 644 {path}\n  • Verify you have read access to the directory\n\nError: {error}")]
    FileReadError { path: String, error: String },

    #[error("Failed to write file: {path}\n\n💡 Solution:\n  • Check if the directory exists: mkdir -p {dir}\n  • Check disk space: df -h\n  • Verify write permissions: ls -ld {dir}\n\nError: {error}")]
    FileWriteError {
        path: String,
        dir: String,
        error: String,
    },

    #[error("Directory not found: {path}\n\n💡 Solution:\n  • Create the directory: mkdir -p {path}\n  • Check the path is correct\n  • Verify parent directory exists")]
    DirectoryNotFound { path: String },

    // ==================== Parsing Errors ====================
    #[error("Invalid Pterodactyl egg JSON: {path}\n\n❌ Parse Error:\n  {error}\n\n💡 Solution:\n  • Validate JSON syntax: cat {path} | jq .\n  • Check for trailing commas or missing quotes\n  • Ensure the file is a valid Pterodactyl egg (has 'meta', 'startup', 'variables' fields)\n  • Download a fresh copy from the source")]
    InvalidEggJson { path: String, error: String },

    #[error("Invalid YAML config: {path}\n\n❌ Parse Error:\n  {error}\n\n💡 Solution:\n  • Check YAML syntax with: yamllint {path}\n  • Common issues: incorrect indentation, missing colons, unquoted special characters\n  • Validate the schema matches GameConfig format")]
    InvalidYaml { path: String, error: String },

    // ==================== Validation Errors ====================
    #[error("Config validation failed: {config_name}\n\n❌ Validation Errors:\n{errors}\n\n💡 Solution:\n  Fix the above issues and re-run validation")]
    ValidationError { config_name: String, errors: String },

    #[error("Missing required field: {field} in {context}\n\n💡 Solution:\n  • Add the {field} field to your config\n  • Check example configs in ./examples/ for reference\n  • Required format: {expected_format}")]
    MissingRequiredField {
        field: String,
        context: String,
        expected_format: String,
    },

    #[error("Invalid value for {field}: got '{value}', expected {expected}\n\n💡 Solution:\n  • Change {field} to match: {expected}\n  • Valid examples: {examples}")]
    InvalidFieldValue {
        field: String,
        value: String,
        expected: String,
        examples: String,
    },

    // ==================== Security Errors ====================
    #[error("Security issue detected in {context}:\n\n⚠️  {issue}\n\n💡 Solution:\n  {solution}\n\n🔒 Security Best Practices:\n  • Avoid shell injection vectors (curl | bash, eval)\n  • Use minimal capabilities (drop ALL, add only what's needed)\n  • Enable seccomp profiles\n  • Set resource limits")]
    SecurityIssue {
        context: String,
        issue: String,
        solution: String,
    },

    // ==================== Docker/Container Errors ====================
    #[error("Invalid Docker image: {image}\n\n💡 Solution:\n  • Check image exists: docker pull {image}\n  • Verify image registry is accessible\n  • Use format: registry/repository:tag\n  • Example: ghcr.io/pterodactyl/yolks:rust")]
    InvalidDockerImage { image: String },

    // ==================== Resource Errors ====================
    #[error("Invalid resource specification: {resource}\n\n❌ Error: {error}\n\n💡 Solution:\n  • Use format: <number><unit>\n  • Memory units: Mi, Gi (e.g., 4Gi, 512Mi)\n  • CPU: millicores (e.g., 2000 = 2 cores)\n  • Disk: Mi, Gi, Ti\n  • Examples: memory: 4Gi, cpu: 2000, disk: 20Gi")]
    InvalidResourceSpec { resource: String, error: String },

    // ==================== Network Errors ====================
    #[error("Invalid port configuration: {port}\n\n💡 Solution:\n  • Port must be between 1024-65535\n  • Use {{{{VARIABLE_NAME}}}} for template values\n  • Specify protocol: tcp, udp, or both\n  • Example:\n    ports:\n      - name: game_port\n        internal: {{{{SERVER_PORT}}}}\n        protocol: udp")]
    InvalidPort { port: String },

    #[error("Git command failed\n\n❌ Error: {error}\n\n💡 Solution:\n  • Install git: apt-get install git (Ubuntu) or brew install git (macOS)\n  • Check git is in PATH: which git\n  • Verify repository URL is correct\n  • Check network connectivity: ping github.com")]
    GitError { error: String },

    // ==================== System Errors ====================
    #[error("System requirements not met:\n\n❌ Missing:\n{missing}\n\n💡 Solution:\n  Run diagnostics: nexus-panel diagnose\n  Install missing dependencies based on your OS")]
    SystemRequirementsNotMet { missing: String },

    #[error("Insufficient permissions\n\n💡 Solution:\n  • Run with sudo if needed: sudo nexus-panel ...\n  • Check file/directory ownership: ls -la\n  • Ensure your user is in required groups (docker, etc.)")]
    InsufficientPermissions,

    // ==================== Conversion Errors ====================
    #[error("Failed to convert egg: {egg_name}\n\n❌ Reason: {reason}\n\n💡 Solution:\n  {solution}\n\n📝 Debug Steps:\n  1. Check egg format: cat {egg_path} | jq .\n  2. Validate required fields exist\n  3. Run with verbose logging: RUST_LOG=debug nexus-panel convert ...\n  4. Report issue if egg is from official source")]
    ConversionError {
        egg_name: String,
        egg_path: String,
        reason: String,
        solution: String,
    },

    // ==================== Generic Fallback ====================
    #[error("Unexpected error occurred\n\n❌ Error: {error}\n\n💡 Solution:\n  • Run diagnostics: nexus-panel diagnose\n  • Check logs with: RUST_LOG=debug nexus-panel ...\n  • Report this error on GitHub if issue persists\n  • Include the full error message and command you ran")]
    Unexpected { error: String },
}

impl NexusPanelError {
    /// Get the error code for documentation lookup
    pub fn error_code(&self) -> &str {
        match self {
            Self::FileReadError { .. } => "E001",
            Self::FileWriteError { .. } => "E002",
            Self::DirectoryNotFound { .. } => "E003",
            Self::InvalidEggJson { .. } => "E101",
            Self::InvalidYaml { .. } => "E102",
            Self::ValidationError { .. } => "E201",
            Self::MissingRequiredField { .. } => "E202",
            Self::InvalidFieldValue { .. } => "E203",
            Self::SecurityIssue { .. } => "E301",
            Self::InvalidDockerImage { .. } => "E401",
            Self::InvalidResourceSpec { .. } => "E501",
            Self::InvalidPort { .. } => "E601",
            Self::GitError { .. } => "E701",
            Self::SystemRequirementsNotMet { .. } => "E801",
            Self::InsufficientPermissions => "E802",
            Self::ConversionError { .. } => "E901",
            Self::Unexpected { .. } => "E999",
        }
    }

    /// Get documentation URL for this error
    pub fn docs_url(&self) -> String {
        format!("https://docs.nexus-panel.io/errors/{}", self.error_code())
    }
}

// Helper to convert std errors to our rich errors
pub trait ResultExt<T> {
    fn with_file_context(
        self,
        path: impl Into<String>,
        operation: &str,
    ) -> Result<T, NexusPanelError>;
}

impl<T, E: std::error::Error + 'static> ResultExt<T> for Result<T, E> {
    fn with_file_context(
        self,
        path: impl Into<String>,
        operation: &str,
    ) -> Result<T, NexusPanelError> {
        let path = path.into();
        self.map_err(|e| {
            if operation == "read" {
                NexusPanelError::FileReadError {
                    path,
                    error: e.to_string(),
                }
            } else {
                let dir = std::path::Path::new(&path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string());
                NexusPanelError::FileWriteError {
                    path,
                    dir,
                    error: e.to_string(),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes_unique() {
        let errors = vec![
            NexusPanelError::FileReadError {
                path: "test".into(),
                error: "not found".into(),
            },
            NexusPanelError::InvalidEggJson {
                path: "test".into(),
                error: "test".into(),
            },
            NexusPanelError::SecurityIssue {
                context: "test".into(),
                issue: "test".into(),
                solution: "test".into(),
            },
        ];

        // Check all error codes are unique
        let mut codes = std::collections::HashSet::new();
        for error in &errors {
            assert!(codes.insert(error.error_code()));
        }
    }

    #[test]
    fn test_error_messages_have_solutions() {
        let error = NexusPanelError::FileReadError {
            path: "/test/file.json".into(),
            error: "not found".into(),
        };

        let msg = error.to_string();
        assert!(msg.contains("💡 Solution:"));
        assert!(msg.contains("Check if the file exists"));
    }
}
