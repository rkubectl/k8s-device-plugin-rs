const MAX_DRIVER_NAME_LEN: usize = 63;
const MAX_DNS_LABEL_LEN: usize = 63;
const MAX_DNS_SUBDOMAIN_LEN: usize = 253;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error(
        "DRA driver name {0:?} must be a DNS subdomain of at most {MAX_DRIVER_NAME_LEN} characters"
    )]
    InvalidDriverName(String),
    #[error("node name {0:?} must be a DNS subdomain of at most 253 characters")]
    InvalidNodeName(String),
    #[error(
        "pool name {0:?} must be slash-separated DNS subdomains of at most 253 characters in total"
    )]
    InvalidPoolName(String),
    #[error("runtime driver name {runtime:?} differs from backend driver name {backend:?}")]
    DriverNameMismatch { runtime: String, backend: String },
}

/// Whether a shared counter or counter-set name is a DNS label (1–63 bytes).
pub fn is_valid_counter_name(name: &str) -> bool {
    is_valid_dns_label(name)
}

/// Validates the Kubernetes DRA driver identity, also used in plugin paths.
pub fn validate_driver_name(name: &str) -> Result<(), ValidationError> {
    if name.len() > MAX_DRIVER_NAME_LEN || !is_valid_dns_subdomain(name) {
        return Err(ValidationError::InvalidDriverName(name.to_string()));
    }
    Ok(())
}

/// Validates a Kubernetes Node name.
pub fn validate_node_name(name: &str) -> Result<(), ValidationError> {
    if !is_valid_dns_subdomain(name) {
        return Err(ValidationError::InvalidNodeName(name.to_string()));
    }
    Ok(())
}

/// Validates the slash-separated DNS subdomains permitted for DRA pools.
pub fn validate_pool_name(name: &str) -> Result<(), ValidationError> {
    if name.len() > MAX_DNS_SUBDOMAIN_LEN || !name.split('/').all(is_valid_dns_subdomain) {
        return Err(ValidationError::InvalidPoolName(name.to_string()));
    }
    Ok(())
}

fn is_valid_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_DNS_LABEL_LEN
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

fn is_valid_dns_subdomain(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_DNS_SUBDOMAIN_LEN && s.split('.').all(is_valid_dns_label)
}

#[cfg(test)]
mod dra_validation_tests {
    use super::*;

    #[test]
    fn counter_names_are_dns_labels() {
        for name in ["cards", "pair-a", "0", &"a".repeat(63)] {
            assert!(is_valid_counter_name(name));
        }
        for name in ["", "-a", "a-", "A", "a.b", "a/b", &"a".repeat(64)] {
            assert!(!is_valid_counter_name(name));
        }
    }

    #[test]
    fn validates_driver_node_and_slash_separated_pool_names() {
        for name in ["", ".", "..", "a/b", "UPPER.example", "a-", "-a"] {
            assert!(validate_driver_name(name).is_err(), "{name:?}");
            assert!(validate_node_name(name).is_err(), "{name:?}");
        }
        assert!(validate_driver_name(&"a".repeat(63)).is_ok());
        assert!(validate_driver_name(&"a".repeat(64)).is_err());
        let longest = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61),
        ]
        .join(".");
        assert_eq!(longest.len(), 253);
        assert!(validate_node_name(&longest).is_ok());
        assert!(validate_node_name(&(longest + "x")).is_err());
        assert!(validate_pool_name("pool.example/tenant-a.example").is_ok());
        for name in [
            "",
            "/pool",
            "pool/",
            "pool//tenant",
            "pool/..",
            "pool/UPPER",
        ] {
            assert!(validate_pool_name(name).is_err(), "{name:?}");
        }
    }
}
