//! Checked classic device-plugin identities.
//!
//! ```compile_fail
//! use k8s_device_plugin_core::{DeviceId, ExtendedResourceName};
//! let id: DeviceId = "gpu/0".parse().unwrap();
//! let _: ExtendedResourceName = id;
//! ```
//! ```compile_fail
//! use k8s_device_plugin_core::DeviceId;
//! let _ = DeviceId("unchecked".to_string());
//! ```
//! ```compile_fail
//! use k8s_device_plugin_core::DeviceId;
//! let mut id: DeviceId = "device".parse().unwrap();
//! id.as_str().push_str("invalid");
//! ```
use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

/// Invalid device ID. IDs must be nonempty and no more than 63 bytes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeviceIdError {
    #[error("device id must not be empty")]
    Empty,
    #[error("device id {0:?} exceeds the 63-byte limit")]
    TooLong(String),
}

/// Invalid extended resource name.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExtendedResourceNameError {
    #[error("resource name must not be empty")]
    Empty,
    #[error("resource name {0:?} must have the form \"<domain>/<name>\"")]
    Malformed(String),
    #[error("resource name domain {0:?} is not a valid DNS subdomain")]
    InvalidDomain(String),
    #[error("resource name suffix {0:?} is not a valid DNS label")]
    InvalidSuffix(String),
    #[error("resource name domain {0:?} is reserved for kubernetes.io")]
    ReservedDomain(String),
}

fn valid_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}
fn valid_subdomain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 253 && s.split('.').all(valid_label)
}
fn validate_resource(value: &str) -> Result<(), ExtendedResourceNameError> {
    use ExtendedResourceNameError as Error;
    if value.is_empty() {
        return Err(Error::Empty);
    }
    let Some((domain, suffix)) = value.split_once('/') else {
        return Err(Error::Malformed(value.into()));
    };
    if suffix.contains('/') {
        return Err(Error::Malformed(value.into()));
    }
    if !valid_subdomain(domain) {
        return Err(Error::InvalidDomain(domain.into()));
    }
    if !valid_label(suffix) {
        return Err(Error::InvalidSuffix(suffix.into()));
    }
    if domain == "kubernetes.io"
        || domain
            .strip_suffix("kubernetes.io")
            .is_some_and(|prefix| prefix.ends_with('.'))
    {
        return Err(Error::ReservedDomain(domain.into()));
    }
    Ok(())
}
fn validate_id(value: &str) -> Result<(), DeviceIdError> {
    if value.is_empty() {
        Err(DeviceIdError::Empty)
    } else if value.len() > 63 {
        Err(DeviceIdError::TooLong(value.into()))
    } else {
        Ok(())
    }
}

macro_rules! identity {
    ($name:ident, $error:ty, $validate:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);
        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = $error;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                $validate(&value)?;
                Ok(Self(value))
            }
        }
        impl TryFrom<&str> for $name {
            type Error = $error;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from(value.to_owned())
            }
        }
        impl FromStr for $name {
            type Err = $error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_from(value)
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
// kubelet deviceplugin/v1beta1/api.proto Device.ID specifies a maximum of 63
// characters. Preserve this workspace's nonempty, 63-byte contract; unlike
// DRA device names, classic IDs have no DNS-label requirement.
identity!(
    DeviceId,
    DeviceIdError,
    validate_id,
    "Nonempty classic device ID of at most 63 bytes."
);
identity!(
    ExtendedResourceName,
    ExtendedResourceNameError,
    validate_resource,
    "Validated classic extended resource name."
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn device_id_preserves_non_dns_and_byte_boundaries() {
        for value in ["GPU/0", "with space", "é", "a\0b", &"a".repeat(63)] {
            let id = DeviceId::try_from(value).unwrap();
            assert_eq!(id.as_str(), value);
        }
        assert!(matches!(DeviceId::try_from(""), Err(DeviceIdError::Empty)));
        assert!(matches!(
            DeviceId::try_from("a".repeat(64)),
            Err(DeviceIdError::TooLong(_))
        ));
        assert!(DeviceId::try_from("é".repeat(32)).is_err());
        let map = BTreeMap::from([("GPU/0".parse::<DeviceId>().unwrap(), 1)]);
        assert_eq!(map.get("GPU/0"), Some(&1));
    }

    #[test]
    fn extended_resource_names_preserve_existing_grammar() {
        for value in ["example.com/widget", "a.b.example.com/gpu-v100"] {
            assert_eq!(
                ExtendedResourceName::try_from(value).unwrap().as_str(),
                value
            );
        }
        assert!(matches!(
            ExtendedResourceName::try_from(""),
            Err(ExtendedResourceNameError::Empty)
        ));
        for value in ["widget", "example.com/widget/extra"] {
            assert!(matches!(
                ExtendedResourceName::try_from(value),
                Err(ExtendedResourceNameError::Malformed(_))
            ));
        }
        assert!(matches!(
            ExtendedResourceName::try_from("Example.com/widget"),
            Err(ExtendedResourceNameError::InvalidDomain(_))
        ));
        assert!(matches!(
            ExtendedResourceName::try_from("example.com/my_widget"),
            Err(ExtendedResourceNameError::InvalidSuffix(_))
        ));
        for value in [
            "kubernetes.io/widget",
            "device-plugins.kubernetes.io/widget",
        ] {
            assert!(matches!(
                ExtendedResourceName::try_from(value),
                Err(ExtendedResourceNameError::ReservedDomain(_))
            ));
        }
    }
}
