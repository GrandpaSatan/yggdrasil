//! Configuration validation utilities.
//!
//! Provides a [`Validate`] trait and common validators for network addresses,
//! URLs, port conflicts, and required fields.

use std::collections::HashSet;
use std::net::SocketAddr;

/// Validation error with context about what failed.
#[derive(Debug, thiserror::Error)]
#[error("{field}: {message}")]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Trait for validatable configuration structs.
pub trait Validate {
    /// Validate the configuration, returning all errors found.
    fn validate(&self) -> Vec<ValidationError>;

    /// Validate and return `Ok(())` or the first error.
    fn validate_or_err(&self) -> Result<(), ValidationError> {
        let errors = self.validate();
        if let Some(e) = errors.into_iter().next() {
            Err(e)
        } else {
            Ok(())
        }
    }
}

/// Validate that a string parses as a valid socket address (ip:port).
pub fn validate_listen_addr(field: &str, addr: &str) -> Option<ValidationError> {
    if addr.parse::<SocketAddr>().is_err() {
        Some(ValidationError::new(
            field,
            format!("invalid listen address: '{addr}' (expected ip:port)"),
        ))
    } else {
        None
    }
}

/// Validate that a string looks like a valid URL (starts with http:// or https://).
pub fn validate_url(field: &str, url: &str) -> Option<ValidationError> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        Some(ValidationError::new(
            field,
            format!("invalid URL: '{url}' (must start with http:// or https://)"),
        ))
    } else {
        None
    }
}

/// Validate that a string is not empty.
pub fn validate_not_empty(field: &str, value: &str) -> Option<ValidationError> {
    if value.trim().is_empty() {
        Some(ValidationError::new(field, "must not be empty"))
    } else {
        None
    }
}

/// Validate that a path string doesn't contain unexpanded env var placeholders.
pub fn validate_no_unexpanded_vars(field: &str, value: &str) -> Option<ValidationError> {
    if value.contains("${") {
        Some(ValidationError::new(
            field,
            format!("contains unexpanded env var placeholder in '{value}'"),
        ))
    } else {
        None
    }
}

/// Check a collection of listen addresses for port conflicts.
/// Returns errors for any duplicate port bindings.
pub fn validate_no_port_conflicts(
    addrs: &[(&str, &str)], // (field_name, addr_string)
) -> Vec<ValidationError> {
    let mut seen = HashSet::new();
    let mut errors = Vec::new();

    for (field, addr) in addrs {
        if let Ok(sa) = addr.parse::<SocketAddr>() {
            let port = sa.port();
            if !seen.insert(port) {
                errors.push(ValidationError::new(
                    *field,
                    format!("port {port} conflicts with another service"),
                ));
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_listen_addr() {
        assert!(validate_listen_addr("test", "0.0.0.0:8080").is_none());
        assert!(validate_listen_addr("test", "127.0.0.1:9090").is_none());
    }

    #[test]
    fn invalid_listen_addr() {
        assert!(validate_listen_addr("test", "localhost:8080").is_some());
        assert!(validate_listen_addr("test", "not-an-addr").is_some());
        assert!(validate_listen_addr("test", "").is_some());
    }

    #[test]
    fn valid_url() {
        assert!(validate_url("test", "http://localhost:8080").is_none());
        assert!(validate_url("test", "https://api.example.com").is_none());
    }

    #[test]
    fn invalid_url() {
        assert!(validate_url("test", "ftp://bad").is_some());
        assert!(validate_url("test", "localhost:8080").is_some());
    }

    #[test]
    fn not_empty() {
        assert!(validate_not_empty("test", "hello").is_none());
        assert!(validate_not_empty("test", "").is_some());
        assert!(validate_not_empty("test", "  ").is_some());
    }

    #[test]
    fn unexpanded_vars() {
        assert!(validate_no_unexpanded_vars("test", "hello").is_none());
        assert!(validate_no_unexpanded_vars("test", "${MISSING}").is_some());
    }

    #[test]
    fn port_conflicts() {
        let addrs = vec![
            ("svc_a", "0.0.0.0:8080"),
            ("svc_b", "0.0.0.0:9090"),
            ("svc_c", "0.0.0.0:8080"),
        ];
        let errors = validate_no_port_conflicts(&addrs);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].field == "svc_c");
    }

    #[test]
    fn no_port_conflicts() {
        let addrs = vec![("a", "0.0.0.0:8080"), ("b", "0.0.0.0:9090")];
        assert!(validate_no_port_conflicts(&addrs).is_empty());
    }
}
