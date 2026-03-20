//! Input validation module for enterprise security.
//!
//! Provides comprehensive input validation for:
//! - Container IDs and names
//! - Configuration YAML
//! - API request parameters
//! - File paths and names
//!
//! # Security Features
//!
//! - Injection prevention (command, path traversal)
//! - Size limits enforcement
//! - Character whitelist validation
//! - Pattern matching validation
//!
//! # Example
//!
//! ```ignore
//! use nexus_node::validation::{Validator, ValidationError};
//!
//! let validator = Validator::default();
//! validator.validate_container_id("my-container-123")?;
//! ```

use std::collections::HashSet;
use thiserror::Error;
use tracing::warn;

/// Validation errors
#[derive(Error, Debug, Clone)]
pub enum ValidationError {
    #[error("Field '{field}' is required")]
    Required { field: String },

    #[error("Field '{field}' exceeds maximum length of {max} (actual: {actual})")]
    TooLong {
        field: String,
        max: usize,
        actual: usize,
    },

    #[error("Field '{field}' is below minimum length of {min} (actual: {actual})")]
    TooShort {
        field: String,
        min: usize,
        actual: usize,
    },

    #[error("Field '{field}' contains invalid characters: {chars}")]
    InvalidCharacters { field: String, chars: String },

    #[error("Field '{field}' does not match expected pattern: {pattern}")]
    PatternMismatch { field: String, pattern: String },

    #[error("Field '{field}' contains potentially dangerous content: {reason}")]
    DangerousContent { field: String, reason: String },

    #[error("Field '{field}' value '{value}' is not in allowed values: {allowed:?}")]
    InvalidValue {
        field: String,
        value: String,
        allowed: Vec<String>,
    },

    #[error("Field '{field}' exceeds maximum value of {max} (actual: {actual})")]
    ExceedsMax {
        field: String,
        max: i64,
        actual: i64,
    },

    #[error("Field '{field}' is below minimum value of {min} (actual: {actual})")]
    BelowMin {
        field: String,
        min: i64,
        actual: i64,
    },

    #[error("Configuration validation failed: {0}")]
    ConfigValidation(String),

    #[error("Multiple validation errors: {0:?}")]
    Multiple(Vec<ValidationError>),
}

impl ValidationError {
    /// Create a required field error
    pub fn required(field: impl Into<String>) -> Self {
        Self::Required {
            field: field.into(),
        }
    }

    /// Create a too long error
    pub fn too_long(field: impl Into<String>, max: usize, actual: usize) -> Self {
        Self::TooLong {
            field: field.into(),
            max,
            actual,
        }
    }

    /// Create an invalid characters error
    pub fn invalid_chars(field: impl Into<String>, chars: impl Into<String>) -> Self {
        Self::InvalidCharacters {
            field: field.into(),
            chars: chars.into(),
        }
    }

    /// Create a dangerous content error
    pub fn dangerous(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::DangerousContent {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

/// Validation result
pub type ValidationResult<T = ()> = Result<T, ValidationError>;

/// Validation rules for a field
#[derive(Debug, Clone)]
pub struct FieldRules {
    /// Field name
    pub name: String,
    /// Required field
    pub required: bool,
    /// Minimum length
    pub min_length: Option<usize>,
    /// Maximum length
    pub max_length: Option<usize>,
    /// Allowed characters pattern (regex-like description)
    pub allowed_chars: Option<String>,
    /// Disallowed patterns
    pub disallowed_patterns: Vec<String>,
    /// Allowed values (enum-like)
    pub allowed_values: Option<Vec<String>>,
    /// Minimum numeric value
    pub min_value: Option<i64>,
    /// Maximum numeric value
    pub max_value: Option<i64>,
}

impl FieldRules {
    /// Create rules for a required field
    pub fn required(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: true,
            min_length: None,
            max_length: None,
            allowed_chars: None,
            disallowed_patterns: Vec::new(),
            allowed_values: None,
            min_value: None,
            max_value: None,
        }
    }

    /// Create rules for an optional field
    pub fn optional(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: false,
            min_length: None,
            max_length: None,
            allowed_chars: None,
            disallowed_patterns: Vec::new(),
            allowed_values: None,
            min_value: None,
            max_value: None,
        }
    }

    /// Set length constraints
    pub fn length(mut self, min: usize, max: usize) -> Self {
        self.min_length = Some(min);
        self.max_length = Some(max);
        self
    }

    /// Set maximum length
    pub fn max_length(mut self, max: usize) -> Self {
        self.max_length = Some(max);
        self
    }

    /// Set allowed characters description
    pub fn chars(mut self, pattern: impl Into<String>) -> Self {
        self.allowed_chars = Some(pattern.into());
        self
    }

    /// Add disallowed pattern
    pub fn disallow(mut self, pattern: impl Into<String>) -> Self {
        self.disallowed_patterns.push(pattern.into());
        self
    }

    /// Set allowed values
    pub fn values(mut self, values: Vec<String>) -> Self {
        self.allowed_values = Some(values);
        self
    }

    /// Set numeric range
    pub fn range(mut self, min: i64, max: i64) -> Self {
        self.min_value = Some(min);
        self.max_value = Some(max);
        self
    }
}

/// Input validator
#[derive(Debug, Clone)]
pub struct Validator {
    /// Maximum container ID length
    pub max_container_id_length: usize,
    /// Maximum container name length
    pub max_container_name_length: usize,
    /// Maximum config YAML size
    pub max_config_size: usize,
    /// Dangerous command patterns
    pub dangerous_patterns: HashSet<String>,
    /// Path traversal patterns
    pub path_traversal_patterns: Vec<String>,
}

impl Default for Validator {
    fn default() -> Self {
        let mut dangerous_patterns = HashSet::new();
        dangerous_patterns.insert("rm -rf".to_string());
        dangerous_patterns.insert("chmod 777".to_string());
        dangerous_patterns.insert("curl | bash".to_string());
        dangerous_patterns.insert("wget | bash".to_string());
        dangerous_patterns.insert("| sh".to_string());
        dangerous_patterns.insert("| bash".to_string());
        dangerous_patterns.insert("eval $".to_string());
        dangerous_patterns.insert("`".to_string());
        dangerous_patterns.insert("$(".to_string());
        dangerous_patterns.insert("$((".to_string());
        dangerous_patterns.insert("--privileged".to_string());
        dangerous_patterns.insert(":z".to_string()); // SELinux relabel
        dangerous_patterns.insert(":Z".to_string());
        dangerous_patterns.insert("/dev/sd".to_string());
        dangerous_patterns.insert("/dev/nvme".to_string());

        Self {
            max_container_id_length: 64,
            max_container_name_length: 128,
            max_config_size: 1024 * 1024, // 1MB
            dangerous_patterns,
            path_traversal_patterns: vec![
                "..".to_string(),
                "~".to_string(),
                "/etc/passwd".to_string(),
                "/etc/shadow".to_string(),
                "/proc/".to_string(),
                "/sys/".to_string(),
            ],
        }
    }
}

impl Validator {
    /// Validate a container ID
    pub fn validate_container_id(&self, id: &str) -> ValidationResult {
        let field = "container_id";

        // Check required
        if id.is_empty() {
            return Err(ValidationError::required(field));
        }

        // Check length
        if id.len() > self.max_container_id_length {
            return Err(ValidationError::too_long(
                field,
                self.max_container_id_length,
                id.len(),
            ));
        }

        // Check characters - only allow alphanumeric, dash, underscore
        for c in id.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' && c != '_' {
                return Err(ValidationError::invalid_chars(field, c.to_string()));
            }
        }

        // Must start with alphanumeric
        if let Some(first) = id.chars().next() {
            if !first.is_ascii_alphanumeric() {
                return Err(ValidationError::PatternMismatch {
                    field: field.to_string(),
                    pattern: "must start with alphanumeric character".to_string(),
                });
            }
        }

        Ok(())
    }

    /// Validate a container name
    pub fn validate_container_name(&self, name: &str) -> ValidationResult {
        let field = "container_name";

        // Check required
        if name.is_empty() {
            return Err(ValidationError::required(field));
        }

        // Check length
        if name.len() > self.max_container_name_length {
            return Err(ValidationError::too_long(
                field,
                self.max_container_name_length,
                name.len(),
            ));
        }

        // Check characters
        for c in name.chars() {
            if !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.' && c != ' ' {
                return Err(ValidationError::invalid_chars(field, c.to_string()));
            }
        }

        // Check for dangerous patterns
        let lower = name.to_lowercase();
        for pattern in &self.dangerous_patterns {
            if lower.contains(&pattern.to_lowercase()) {
                warn!("Dangerous pattern '{}' detected in container name", pattern);
                return Err(ValidationError::dangerous(field, pattern.clone()));
            }
        }

        Ok(())
    }

    /// Validate configuration YAML
    pub fn validate_config_yaml(&self, yaml: &str) -> ValidationResult {
        let field = "config_yaml";

        // Check size
        if yaml.len() > self.max_config_size {
            return Err(ValidationError::too_long(
                field,
                self.max_config_size,
                yaml.len(),
            ));
        }

        // Check for dangerous patterns
        let lower = yaml.to_lowercase();
        for pattern in &self.dangerous_patterns {
            if lower.contains(&pattern.to_lowercase()) {
                warn!("Dangerous pattern '{}' detected in config", pattern);
                return Err(ValidationError::dangerous(field, pattern.clone()));
            }
        }

        // Check for path traversal
        for pattern in &self.path_traversal_patterns {
            if yaml.contains(pattern) {
                warn!("Path traversal pattern '{}' detected in config", pattern);
                return Err(ValidationError::dangerous(
                    field,
                    format!("path traversal: {}", pattern),
                ));
            }
        }

        // Validate YAML syntax
        if let Err(e) = serde_yaml::from_str::<serde_yaml::Value>(yaml) {
            return Err(ValidationError::ConfigValidation(format!(
                "Invalid YAML syntax: {}",
                e
            )));
        }

        Ok(())
    }

    /// Validate a port number
    pub fn validate_port(&self, port: u32, field_name: &str) -> ValidationResult {
        if port == 0 || port > 65535 {
            return Err(ValidationError::InvalidValue {
                field: field_name.to_string(),
                value: port.to_string(),
                allowed: vec!["1-65535".to_string()],
            });
        }

        // Warn about privileged ports
        if port < 1024 {
            warn!(
                "Privileged port {} requested for field '{}'",
                port, field_name
            );
        }

        Ok(())
    }

    /// Validate a timeout value
    pub fn validate_timeout(&self, timeout_secs: u32, field_name: &str) -> ValidationResult {
        // Maximum timeout of 1 hour
        if timeout_secs > 3600 {
            return Err(ValidationError::ExceedsMax {
                field: field_name.to_string(),
                max: 3600,
                actual: timeout_secs as i64,
            });
        }

        Ok(())
    }

    /// Validate a file path
    pub fn validate_path(&self, path: &str, field_name: &str) -> ValidationResult {
        // Check for path traversal
        for pattern in &self.path_traversal_patterns {
            if path.contains(pattern) {
                return Err(ValidationError::dangerous(
                    field_name,
                    format!("path traversal: {}", pattern),
                ));
            }
        }

        // Check for null bytes
        if path.contains('\0') {
            return Err(ValidationError::dangerous(field_name, "null byte in path"));
        }

        // Check for control characters
        for c in path.chars() {
            if c.is_control() && c != '\t' && c != '\n' && c != '\r' {
                return Err(ValidationError::invalid_chars(
                    field_name,
                    "control characters",
                ));
            }
        }

        Ok(())
    }

    /// Validate environment variable name
    pub fn validate_env_var_name(&self, name: &str) -> ValidationResult {
        let field = "environment_variable";

        if name.is_empty() {
            return Err(ValidationError::required(field));
        }

        // Must start with letter or underscore
        if let Some(first) = name.chars().next() {
            if !first.is_ascii_alphabetic() && first != '_' {
                return Err(ValidationError::PatternMismatch {
                    field: field.to_string(),
                    pattern: "must start with letter or underscore".to_string(),
                });
            }
        }

        // Only alphanumeric and underscore
        for c in name.chars() {
            if !c.is_ascii_alphanumeric() && c != '_' {
                return Err(ValidationError::invalid_chars(field, c.to_string()));
            }
        }

        // Max length
        if name.len() > 256 {
            return Err(ValidationError::too_long(field, 256, name.len()));
        }

        Ok(())
    }

    /// Validate environment variable value
    pub fn validate_env_var_value(&self, value: &str, name: &str) -> ValidationResult {
        let field = format!("env_value:{}", name);

        // Check for null bytes
        if value.contains('\0') {
            return Err(ValidationError::dangerous(&field, "null byte in value"));
        }

        // Check size
        if value.len() > 32768 {
            return Err(ValidationError::too_long(&field, 32768, value.len()));
        }

        // Warn about potentially sensitive values
        let lower_name = name.to_lowercase();
        if (lower_name.contains("password")
            || lower_name.contains("secret")
            || lower_name.contains("key")
            || lower_name.contains("token"))
            && !value.starts_with("secret:")
        {
            warn!(
                "Potentially sensitive environment variable '{}' with plaintext value",
                name
            );
        }

        Ok(())
    }

    /// Batch validate multiple fields
    pub fn validate_all(&self, validations: Vec<ValidationResult>) -> ValidationResult {
        let errors: Vec<ValidationError> =
            validations.into_iter().filter_map(|r| r.err()).collect();

        if errors.is_empty() {
            Ok(())
        } else if errors.len() == 1 {
            Err(errors.into_iter().next().unwrap())
        } else {
            Err(ValidationError::Multiple(errors))
        }
    }
}

/// Sanitize user input for logging
pub fn sanitize_for_log(input: &str, max_length: usize) -> String {
    let sanitized: String = input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(max_length)
        .collect();

    if input.len() > max_length {
        format!("{}...(truncated)", sanitized)
    } else {
        sanitized
    }
}

/// Mask sensitive fields in a string
pub fn mask_sensitive(input: &str, patterns: &[&str]) -> String {
    let mut result = input.to_string();
    for pattern in patterns {
        if let Some(pos) = result.to_lowercase().find(&pattern.to_lowercase()) {
            // Find the value after the pattern
            let start = pos + pattern.len();
            if let Some(value_start) =
                result[start..].find(|c: char| !c.is_whitespace() && c != ':' && c != '=')
            {
                let value_pos = start + value_start;
                // Find the end of the value
                let value_end = result[value_pos..]
                    .find(|c: char| {
                        c.is_whitespace() || c == ',' || c == '}' || c == '"' || c == '\''
                    })
                    .map(|e| value_pos + e)
                    .unwrap_or(result.len());

                if value_end > value_pos {
                    result.replace_range(value_pos..value_end, "***");
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_container_id_valid() {
        let validator = Validator::default();
        assert!(validator.validate_container_id("my-container-123").is_ok());
        assert!(validator.validate_container_id("container_name").is_ok());
        assert!(validator.validate_container_id("a").is_ok());
    }

    #[test]
    fn test_validate_container_id_invalid() {
        let validator = Validator::default();

        // Empty
        assert!(validator.validate_container_id("").is_err());

        // Invalid characters
        assert!(validator.validate_container_id("container/name").is_err());
        assert!(validator.validate_container_id("container:name").is_err());
        assert!(validator.validate_container_id("container;name").is_err());

        // Starts with non-alphanumeric
        assert!(validator.validate_container_id("-container").is_err());
        assert!(validator.validate_container_id("_container").is_err());
    }

    #[test]
    fn test_validate_config_yaml_dangerous_patterns() {
        let validator = Validator::default();

        // Dangerous command patterns
        assert!(validator.validate_config_yaml("command: rm -rf /").is_err());
        assert!(validator.validate_config_yaml("chmod 777 /").is_err());
        assert!(validator.validate_config_yaml("curl http://x | bash").is_err());

        // Path traversal
        assert!(validator.validate_config_yaml("path: ../../../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_config_yaml_valid() {
        let validator = Validator::default();

        let valid_yaml = r#"
metadata:
  name: test
  version: "1.0"
container:
  image: nginx:latest
"#;
        assert!(validator.validate_config_yaml(valid_yaml).is_ok());
    }

    #[test]
    fn test_validate_port() {
        let validator = Validator::default();

        assert!(validator.validate_port(80, "port").is_ok());
        assert!(validator.validate_port(443, "port").is_ok());
        assert!(validator.validate_port(8080, "port").is_ok());
        assert!(validator.validate_port(65535, "port").is_ok());

        assert!(validator.validate_port(0, "port").is_err());
        assert!(validator.validate_port(65536, "port").is_err());
    }

    #[test]
    fn test_sanitize_for_log() {
        let input = "hello\x00world\x01test";
        let sanitized = sanitize_for_log(input, 100);
        assert!(!sanitized.contains('\x00'));
        assert!(!sanitized.contains('\x01'));

        let long_input = "a".repeat(200);
        let truncated = sanitize_for_log(&long_input, 50);
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn test_mask_sensitive() {
        let input = "password: secret123, api_key: abcdef";
        let masked = mask_sensitive(input, &["password", "api_key"]);
        assert!(!masked.contains("secret123"));
        assert!(!masked.contains("abcdef"));
        assert!(masked.contains("***"));
    }

    #[test]
    fn test_validate_env_var_name() {
        let validator = Validator::default();

        assert!(validator.validate_env_var_name("MY_VAR").is_ok());
        assert!(validator.validate_env_var_name("_PRIVATE").is_ok());
        assert!(validator.validate_env_var_name("var123").is_ok());

        assert!(validator.validate_env_var_name("").is_err());
        assert!(validator.validate_env_var_name("123VAR").is_err());
        assert!(validator.validate_env_var_name("my-var").is_err());
    }
}
