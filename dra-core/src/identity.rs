//! Validated identities used by DRA. Values are preserved byte-for-byte.
//!
//! ```compile_fail
//! use k8s_device_plugin_dra_core::{DeviceName, PoolName};
//! let pool: PoolName = "pool".parse().unwrap();
//! let _: DeviceName = pool;
//! ```
//! ```compile_fail
//! use k8s_device_plugin_dra_core::DeviceName;
//! let _ = DeviceName("unchecked".to_string());
//! ```
//! ```compile_fail
//! use k8s_device_plugin_dra_core::DeviceName;
//! let mut name: DeviceName = "card-0".parse().unwrap();
//! name.as_str().push_str("invalid");
//! ```
use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::Serialize;

fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

fn dns_subdomain(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && value.split('.').all(dns_label)
}

macro_rules! identity {
    ($name:ident, $error:ident, $description:literal, $validate:expr) => {
        #[doc = $description]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        #[doc = concat!("Invalid ", stringify!($name), ".")]
        #[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
        #[error("invalid {} {value:?}: {}", stringify!($name), $description)]
        pub struct $error {
            /// The rejected, unmodified input.
            pub value: String,
        }

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
                if ($validate)(&value) {
                    Ok(Self(value))
                } else {
                    Err($error { value })
                }
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

// https://github.com/kubernetes/kubernetes/blob/v1.36.1/pkg/apis/resource/validation/validation.go
// https://github.com/kubernetes/kubernetes/blob/v1.37.0/pkg/apis/resource/validation/validation.go
// Kubernetes v1.36.1 and v1.37.0 resource validation use DNS1123 labels
// for device and counter names; ResourcePool allows slash-separated DNS
// subdomains. Existing workspace subdomain checks additionally restrict each
// component to 63 bytes; preserve that behavior during this migration.
identity!(
    DriverName,
    DriverNameError,
    "DNS subdomain of at most 63 bytes",
    |s: &str| dns_subdomain(s, 63)
);
identity!(
    NodeName,
    NodeNameError,
    "DNS subdomain of at most 253 bytes",
    |s: &str| dns_subdomain(s, 253)
);
identity!(
    PoolName,
    PoolNameError,
    "slash-separated DNS subdomains of at most 253 bytes",
    |s: &str| !s.is_empty() && s.len() <= 253 && s.split('/').all(|part| dns_subdomain(part, 253))
);
identity!(
    DeviceName,
    DeviceNameError,
    "DNS label of at most 63 bytes",
    dns_label
);
identity!(
    CounterSetName,
    CounterSetNameError,
    "DNS label of at most 63 bytes",
    dns_label
);
identity!(
    CounterName,
    CounterNameError,
    "DNS label of at most 63 bytes",
    dns_label
);

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn dns_label_identity_boundaries() {
        for valid in ["a", "0", "a-0", &"a".repeat(63)] {
            assert!(DeviceName::try_from(valid).is_ok());
            assert!(CounterName::try_from(valid).is_ok());
            assert!(CounterSetName::try_from(valid).is_ok());
        }
        for invalid in [
            "",
            "-a",
            "a-",
            "A",
            "a.b",
            "a_b",
            "a\0b",
            "é",
            &"a".repeat(64),
        ] {
            assert!(DeviceName::try_from(invalid).is_err());
            assert!(CounterName::try_from(invalid).is_err());
            assert!(CounterSetName::try_from(invalid).is_err());
        }
    }

    #[test]
    fn subdomain_and_pool_boundaries() {
        assert!(DriverName::try_from("a".repeat(63)).is_ok());
        assert!(DriverName::try_from("a".repeat(64)).is_err());
        let max = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61),
        ]
        .join(".");
        assert_eq!(max.len(), 253);
        assert!(NodeName::try_from(max.clone()).is_ok());
        assert!(PoolName::try_from(max.clone()).is_ok());
        assert!(PoolName::try_from(format!("{max}x")).is_err());
        assert!(NodeName::try_from(format!("{max}x")).is_err());
        assert!(NodeName::try_from("a".repeat(64)).is_err());
        assert!(PoolName::try_from("pool.example/tenant-a.example").is_ok());
        for invalid in [
            "",
            ".",
            "..",
            "a/b",
            "UPPER.example",
            "a-",
            "-a",
            "a\0b",
            "é",
        ] {
            assert!(DriverName::try_from(invalid).is_err(), "{invalid:?}");
            assert!(NodeName::try_from(invalid).is_err(), "{invalid:?}");
        }
        for invalid in [
            "",
            "/pool",
            "pool/",
            "pool//a",
            "pool/..",
            "pool/UPPER",
            "pool/a\0b",
            "pool/é",
        ] {
            assert!(PoolName::try_from(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn serde_checks_and_preserves_string_values_and_keys() {
        let name: DeviceName = serde_json::from_str("\"card-0\"").unwrap();
        assert_eq!(serde_json::to_string(&name).unwrap(), "\"card-0\"");
        assert!(serde_json::from_str::<DeviceName>("\"INVALID\"").is_err());
        assert!(serde_json::from_str::<DriverName>("\"INVALID\"").is_err());
        assert!(serde_json::from_str::<NodeName>("\"INVALID\"").is_err());
        assert!(serde_json::from_str::<PoolName>("\"bad//pool\"").is_err());
        assert!(serde_json::from_str::<CounterName>("\"INVALID\"").is_err());
        assert!(serde_json::from_str::<CounterSetName>("\"INVALID\"").is_err());
        let map = BTreeMap::from([(name, 1)]);
        assert_eq!(map.get("card-0"), Some(&1));
        assert_eq!(serde_json::to_string(&map).unwrap(), "{\"card-0\":1}");
        assert!(serde_json::from_str::<BTreeMap<DeviceName, i32>>("{\"bad.name\":1}").is_err());
    }
}
