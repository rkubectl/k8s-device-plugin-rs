//! A real, deployable device plugin built on `StaticDevicePlugin` --
//! configured entirely through environment variables so consumers can fork
//! this crate, point it at their own hardware via `example/k8s/daemonset.yaml`,
//! and go. See `example/README.md` for the build/deploy walkthrough.

use std::collections::BTreeSet;
use std::env;
use std::io;

use camino::Utf8Path;

use k8s_device_plugin_lib::Device;
use k8s_device_plugin_lib::DevicePlugin;
use k8s_device_plugin_lib::DevicePluginService;
use k8s_device_plugin_lib::StaticDevicePlugin;

const DEFAULT_RESOURCE_NAME: &str = "example.com/widget";

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let resource_name =
        env::var("RESOURCE_NAME").unwrap_or_else(|_| DEFAULT_RESOURCE_NAME.to_string());
    let device_paths = env::var("DEVICE_PATHS").unwrap_or_default();
    let devices = parse_devices(&device_paths)?;

    if devices.is_empty() {
        tracing::warn!("no DEVICE_PATHS configured -- registering with an empty device list");
    }

    tracing::info!(
        resource_name,
        device_count = devices.len(),
        "starting device plugin"
    );

    let service = DevicePluginService::new(StaticDevicePlugin::new(devices));
    let plugin = DevicePlugin::new(&resource_name, service).map_err(io::Error::other)?;
    plugin.run().await
}

fn parse_devices(paths: &str) -> io::Result<Vec<Device>> {
    let mut ids = BTreeSet::new();
    paths
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let id = Utf8Path::new(path)
                .file_name()
                .map_or_else(|| path.to_string(), |name| name.to_string());
            if !ids.insert(id.clone()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("duplicate DEVICE_PATHS device ID {id}"),
                ));
            }
            Ok(Device::rdwr(id, path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_devices_splits_trims_and_derives_ids_from_basenames() {
        let devices = parse_devices(" /dev/widget0 , /dev/nested/widget1,/dev/widget2 ")
            .expect("unique device paths");

        let ids = devices.iter().map(|d| d.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids, ["widget0", "widget1", "widget2"]);
    }

    #[test]
    fn parse_devices_drops_empty_entries() {
        let devices = parse_devices("/dev/widget0,,  ,/dev/widget1").expect("unique device paths");

        assert_eq!(devices.len(), 2);
    }

    #[test]
    fn parse_devices_on_empty_string_returns_no_devices() {
        let devices = parse_devices("").expect("unique device paths");

        assert!(devices.is_empty());
    }

    #[test]
    fn parse_devices_falls_back_to_full_path_when_there_is_no_file_name() {
        let devices = parse_devices("/").expect("unique device paths");

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "/");
    }
}

#[cfg(test)]
mod duplicate_tests {
    use super::*;
    #[test]
    fn ambiguous_basename_ids_are_rejected() {
        let error = parse_devices("/dev/widget,/other/widget").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            error
                .to_string()
                .contains("duplicate DEVICE_PATHS device ID widget")
        );
    }
}
