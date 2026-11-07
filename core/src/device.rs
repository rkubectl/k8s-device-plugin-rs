use super::*;

/// Device-node paths represented losslessly in the kubelet protocol.
///
/// Paths are UTF-8 by construction. Convert arbitrary OS paths explicitly:
///
/// ```
/// use camino::Utf8PathBuf;
/// use k8s_device_plugin_core::DevicePath;
///
/// # fn main() -> Result<(), camino::FromPathBufError> {
/// let os_path = std::path::PathBuf::from("/dev/widget-0");
/// let device = DevicePath::rdwr(Utf8PathBuf::try_from(os_path)?);
/// assert_eq!(device.host_path.as_str(), "/dev/widget-0");
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevicePath {
    pub host_path: Utf8PathBuf,
    pub container_path: Utf8PathBuf,
    pub permissions: DevicePermissions,
}

/// A host directory or file to bind-mount into the container, as opposed to a
/// device special file (see [`DevicePath`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostMount {
    pub host_path: Utf8PathBuf,
    pub container_path: Utf8PathBuf,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: DeviceId,
    pub health: Health,
    pub paths: Vec<DevicePath>,
}

impl Device {
    /// Validates the device paths, without filesystem access.
    ///
    /// # Errors
    /// Returns a validation error for an invalid path.
    pub fn validate(&self) -> Result<(), ValidationError> {
        for path in &self.paths {
            path.validate()?;
        }
        Ok(())
    }

    pub fn new(id: DeviceId) -> Self {
        let health = Health::Healthy;
        let paths = vec![];
        Self { id, health, paths }
    }

    pub fn rdwr(id: DeviceId, path: impl Into<Utf8PathBuf>) -> Self {
        let health = Health::Healthy;
        let paths = vec![DevicePath::rdwr(path)];
        Self { id, health, paths }
    }

    pub fn health(self, health: Health) -> Self {
        Self { health, ..self }
    }
}

impl DevicePath {
    /// Validates both protocol paths without checking whether they exist.
    ///
    /// # Errors
    /// Returns a validation error for an empty, relative or NUL-containing path.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_absolute_path(&self.host_path)?;
        validate_absolute_path(&self.container_path)
    }

    pub fn rdwr(path: impl Into<Utf8PathBuf>) -> Self {
        let host_path = path.into();
        let container_path = host_path.clone();
        let permissions = DevicePermissions::rdwr();
        Self {
            host_path,
            container_path,
            permissions,
        }
    }
}

impl HostMount {
    /// Validates both mount paths without checking whether they exist.
    ///
    /// # Errors
    /// Returns a validation error for an empty, relative or NUL-containing path.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_absolute_path(&self.host_path)?;
        validate_absolute_path(&self.container_path)
    }
}
