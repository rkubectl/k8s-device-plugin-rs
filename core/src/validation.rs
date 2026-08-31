use super::*;

const MAX_DEVICE_ID_LEN: usize = 63;
const MAX_DNS_LABEL_LEN: usize = 63;
const MAX_DNS_SUBDOMAIN_LEN: usize = 253;
const RESERVED_RESOURCE_DOMAIN: &str = "kubernetes.io";

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("resource name must not be empty")]
    EmptyResourceName,
    #[error("resource name {0:?} must have the form \"<domain>/<name>\"")]
    MalformedResourceName(String),
    #[error("resource name domain {0:?} is not a valid DNS subdomain")]
    InvalidResourceDomain(String),
    #[error("resource name suffix {0:?} is not a valid DNS label")]
    InvalidResourceNameSuffix(String),
    #[error("resource name domain {0:?} is reserved for kubernetes.io")]
    ReservedResourceDomain(String),
    #[error("device id must not be empty")]
    EmptyDeviceId,
    #[error("device id {0:?} exceeds the {MAX_DEVICE_ID_LEN}-character limit")]
    DeviceIdTooLong(String),
    #[error("path must not be empty")]
    EmptyPath,
    #[error("path {0:?} must be absolute")]
    RelativePath(PathBuf),
}

/// Validates a Kubernetes extended resource name, e.g. `"example.com/widget"`.
///
/// Requires exactly one `/` separating a DNS-subdomain vendor domain from a
/// DNS-label resource name, and rejects the `kubernetes.io` domain (and its
/// subdomains), which is reserved for kubelet-internal resources.
pub fn validate_resource_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() {
        return Err(ValidationError::EmptyResourceName);
    }
    let Some((domain, suffix)) = name.split_once('/') else {
        return Err(ValidationError::MalformedResourceName(name.to_string()));
    };
    if suffix.contains('/') {
        return Err(ValidationError::MalformedResourceName(name.to_string()));
    }
    if !is_valid_dns_subdomain(domain) {
        return Err(ValidationError::InvalidResourceDomain(domain.to_string()));
    }
    if !is_valid_dns_label(suffix) {
        return Err(ValidationError::InvalidResourceNameSuffix(
            suffix.to_string(),
        ));
    }
    if domain == RESERVED_RESOURCE_DOMAIN
        || domain
            .strip_suffix(RESERVED_RESOURCE_DOMAIN)
            .is_some_and(|prefix| prefix.ends_with('.'))
    {
        return Err(ValidationError::ReservedResourceDomain(domain.to_string()));
    }
    Ok(())
}

/// Validates a device ID against the 63-character limit documented on
/// `deviceplugin.v1beta1.Device.ID`.
pub fn validate_device_id(id: &str) -> Result<(), ValidationError> {
    if id.is_empty() {
        return Err(ValidationError::EmptyDeviceId);
    }
    if id.len() > MAX_DEVICE_ID_LEN {
        return Err(ValidationError::DeviceIdTooLong(id.to_string()));
    }
    Ok(())
}

/// Validates that a host or container path is non-empty and absolute.
pub fn validate_absolute_path(path: &Path) -> Result<(), ValidationError> {
    if path.as_os_str().is_empty() {
        return Err(ValidationError::EmptyPath);
    }
    if !path.is_absolute() {
        return Err(ValidationError::RelativePath(path.to_path_buf()));
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
mod tests {
    use super::*;

    #[test]
    fn resource_name_accepts_valid_names() {
        assert!(validate_resource_name("example.com/widget").is_ok());
        assert!(validate_resource_name("a.b.example.com/gpu-v100").is_ok());
    }

    #[test]
    fn resource_name_rejects_empty() {
        assert_eq!(
            validate_resource_name(""),
            Err(ValidationError::EmptyResourceName)
        );
    }

    #[test]
    fn resource_name_rejects_missing_domain() {
        assert_eq!(
            validate_resource_name("widget"),
            Err(ValidationError::MalformedResourceName("widget".to_string()))
        );
    }

    #[test]
    fn resource_name_rejects_extra_slash() {
        assert_eq!(
            validate_resource_name("example.com/widget/extra"),
            Err(ValidationError::MalformedResourceName(
                "example.com/widget/extra".to_string()
            ))
        );
    }

    #[test]
    fn resource_name_rejects_invalid_domain_chars() {
        assert_eq!(
            validate_resource_name("Example.com/widget"),
            Err(ValidationError::InvalidResourceDomain(
                "Example.com".to_string()
            ))
        );
    }

    #[test]
    fn resource_name_rejects_invalid_suffix_chars() {
        assert_eq!(
            validate_resource_name("example.com/my_widget"),
            Err(ValidationError::InvalidResourceNameSuffix(
                "my_widget".to_string()
            ))
        );
    }

    #[test]
    fn resource_name_rejects_reserved_domain() {
        assert_eq!(
            validate_resource_name("kubernetes.io/widget"),
            Err(ValidationError::ReservedResourceDomain(
                "kubernetes.io".to_string()
            ))
        );
    }

    #[test]
    fn resource_name_rejects_reserved_subdomain() {
        assert_eq!(
            validate_resource_name("device-plugins.kubernetes.io/widget"),
            Err(ValidationError::ReservedResourceDomain(
                "device-plugins.kubernetes.io".to_string()
            ))
        );
    }

    #[test]
    fn device_id_rejects_empty() {
        assert_eq!(validate_device_id(""), Err(ValidationError::EmptyDeviceId));
    }

    #[test]
    fn device_id_rejects_too_long() {
        let id = "a".repeat(64);
        assert_eq!(
            validate_device_id(&id),
            Err(ValidationError::DeviceIdTooLong(id))
        );
    }

    #[test]
    fn device_id_accepts_max_length() {
        let id = "a".repeat(63);
        assert!(validate_device_id(&id).is_ok());
    }

    #[test]
    fn device_id_accepts_normal_id() {
        assert!(validate_device_id("widget-0").is_ok());
    }

    #[test]
    fn path_rejects_empty() {
        assert_eq!(
            validate_absolute_path(Path::new("")),
            Err(ValidationError::EmptyPath)
        );
    }

    #[test]
    fn path_rejects_relative() {
        assert_eq!(
            validate_absolute_path(Path::new("dev/widget0")),
            Err(ValidationError::RelativePath(PathBuf::from("dev/widget0")))
        );
    }

    #[test]
    fn path_accepts_absolute() {
        assert!(validate_absolute_path(Path::new("/dev/widget0")).is_ok());
    }
}
