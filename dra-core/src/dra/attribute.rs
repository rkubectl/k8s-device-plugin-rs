/// A single typed attribute value, as used by DRA's CEL-based device
/// selectors (`DeviceClass`/`ResourceClaim` selector expressions match
/// against these).
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    String(String),
    Int(i64),
    Bool(bool),
    /// A semver-style version string, per the DRA API's `version` attribute
    /// kind.
    Version(String),
}

impl From<String> for AttributeValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for AttributeValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<i64> for AttributeValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<bool> for AttributeValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl AttributeValue {
    /// Creates a version attribute explicitly; ordinary strings stay string attributes.
    pub fn version(version: impl Into<String>) -> Self {
        Self::Version(version.into())
    }
}
