use super::*;

/// A [`K8sDevicePlugin`] backend for the simplest common case: a fixed device
/// list known at startup, with no ongoing discovery or health tracking.
///
/// `discover()` always returns the configured list unchanged. `allocate()`
/// checks that each requested device's host paths still exist on disk before
/// handing them out, failing with [`AllocationError::DeviceUnavailable`] if
/// one has disappeared since startup (e.g. hardware unplugged after the
/// plugin registered) -- so a stale path is never silently handed to
/// kubelet.
///
/// ```no_run
/// # use k8s_device_plugin_lib::{Device, StaticDevicePlugin};
/// let devices = vec![Device::rdwr("widget-0".parse().unwrap(), "/dev/widget0")];
/// let plugin = StaticDevicePlugin::new(devices);
/// ```
#[derive(Clone, Debug)]
pub struct StaticDevicePlugin {
    devices: Vec<Device>,
}

impl StaticDevicePlugin {
    pub fn new(devices: Vec<Device>) -> Self {
        Self { devices }
    }
}

#[tonic::async_trait]
impl DeviceDiscovery for StaticDevicePlugin {
    async fn discover(&self) -> Vec<Device> {
        self.devices.clone()
    }
}

#[tonic::async_trait]
impl DeviceAllocator for StaticDevicePlugin {
    async fn allocate(
        &self,
        device_ids: &[DeviceId],
    ) -> Result<ContainerAllocation, AllocationError> {
        let mut device_paths = Vec::new();
        for id in device_ids {
            let device = self
                .devices
                .iter()
                .find(|device| &device.id == id)
                .ok_or_else(|| AllocationError::DeviceNotFound(id.clone()))?;
            for path in &device.paths {
                if !tokio::fs::try_exists(&path.host_path)
                    .await
                    .unwrap_or(false)
                {
                    return Err(AllocationError::DeviceUnavailable(format!(
                        "{id}: host path {} does not exist",
                        path.host_path
                    )));
                }
            }
            device_paths.extend(device.paths.iter().cloned());
        }
        Ok(ContainerAllocation {
            device_paths,
            ..default()
        })
    }
}

impl K8sDevicePlugin for StaticDevicePlugin {}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn discover_returns_the_static_list_unchanged() {
        let devices = vec![Device::rdwr("dev-0".parse().unwrap(), "/dev/null")];
        let plugin = StaticDevicePlugin::new(devices.clone());

        let discovered = plugin.discover().await;
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].id, devices[0].id);
    }

    #[tokio::test]
    async fn allocate_succeeds_when_host_path_exists() {
        let temp_file = camino_tempfile::NamedUtf8TempFile::new().expect("create temp file");
        let plugin = StaticDevicePlugin::new(vec![Device::rdwr(
            "dev-0".parse().unwrap(),
            temp_file.path().to_path_buf(),
        )]);

        let allocation = plugin.allocate(&["dev-0".parse().unwrap()]).await.unwrap();
        assert_eq!(allocation.device_paths.len(), 1);
    }

    #[tokio::test]
    async fn allocate_fails_when_host_path_is_missing() {
        let plugin = StaticDevicePlugin::new(vec![Device::rdwr(
            "dev-0".parse().unwrap(),
            Utf8PathBuf::from("/nonexistent/path/for/testing"),
        )]);

        let err = plugin
            .allocate(&["dev-0".parse().unwrap()])
            .await
            .unwrap_err();
        assert!(matches!(err, AllocationError::DeviceUnavailable(_)));
    }

    #[tokio::test]
    async fn allocate_reports_unknown_device() {
        let plugin = StaticDevicePlugin::new(vec![]);

        let err = plugin
            .allocate(&["does-not-exist".parse().unwrap()])
            .await
            .unwrap_err();
        assert!(matches!(err, AllocationError::DeviceNotFound(_)));
    }
}
