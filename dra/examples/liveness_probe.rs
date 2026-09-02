//! Kubernetes `exec` probe for the validation DRA driver image.

use std::env;
use std::io;

use k8s_device_plugin_dra::DraPluginLivenessProbe;

const DEFAULT_DRIVER_NAME: &str = "dra.example.com";

#[tokio::main]
async fn main() -> io::Result<()> {
    let driver_name = env::var("DRIVER_NAME").unwrap_or_else(|_| DEFAULT_DRIVER_NAME.to_string());
    DraPluginLivenessProbe::new(&driver_name).check().await
}
