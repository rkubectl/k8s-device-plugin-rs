//! One explicit pool-status snapshot, then UID-safe request cleanup.
use std::env;
use std::io;
use std::time::Duration;

use serde_json as json;

use k8s_openapi::api::resource::v1alpha3::ResourcePoolStatusRequestSpec;

use k8s_device_plugin_dra::PoolStatusClient;

#[tokio::main]
async fn main() -> io::Result<()> {
    let driver = env::var("DRIVER_NAME").unwrap_or_else(|_| "dra.example.com".to_string());
    let pool_name = env::var("POOL_NAME").ok().filter(|name| !name.is_empty());
    let mut config = if let Some(context) = env::var("KUBE_CONTEXT")
        .ok()
        .filter(|context| !context.is_empty())
    {
        kube::Config::from_kubeconfig(&kube::config::KubeConfigOptions {
            context: Some(context),
            ..Default::default()
        })
        .await
        .map_err(io::Error::other)?
    } else {
        kube::Config::infer().await.map_err(io::Error::other)?
    };
    config.read_timeout = Some(Duration::from_secs(15));
    config.connect_timeout = Some(Duration::from_secs(10));
    let client = kube::Client::try_from(config).map_err(io::Error::other)?;
    let query = PoolStatusClient::new(client)
        .create(ResourcePoolStatusRequestSpec {
            driver,
            pool_name,
            limit: Some(100),
        })
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    eprintln!("created pool-status request {}", query.name());
    let result = query.wait(Duration::from_secs(60)).await;
    let cleanup = query.delete().await;
    // Always attempt cleanup, including when the controller failed or timed out.
    // On dual failure retain the original error and report the cleanup error.
    if let Err(error) = cleanup {
        eprintln!("request {} needs cleanup: {error}", query.name());
        if result.is_ok() {
            return Err(io::Error::other(error.to_string()));
        }
    }
    let status = result.map_err(|error| io::Error::other(error.to_string()))?;
    println!("{}", json::to_string(&status).map_err(io::Error::other)?);
    Ok(())
}
