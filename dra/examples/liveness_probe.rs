//! Kubernetes `exec` probe for the validation DRA driver image.

use std::env;
use std::io;

use k8s_device_plugin_dra::DraPluginLivenessProbe;
use k8s_device_plugin_dra_core::DriverName;

const DEFAULT_DRIVER_NAME: &str = "dra.example.com";

#[tokio::main]
async fn main() -> io::Result<()> {
    let driver_name = env::var("DRIVER_NAME").unwrap_or_else(|_| DEFAULT_DRIVER_NAME.to_string());
    let driver_name: DriverName = driver_name.parse().map_err(io::Error::other)?;
    DraPluginLivenessProbe::new(&driver_name).check().await
}
