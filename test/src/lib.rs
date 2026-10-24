use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use hyper_util::rt::TokioIo;
use tempfile::TempDir;
use tokio::net;
// Public mock state exposes async lock acquisition to consumer test tasks.
use tokio::sync::Mutex;
use tokio::task;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport;
use tower::service_fn;

use k8s_device_plugin_proto::v1beta1;

pub mod device_plugin;
pub mod registration;

#[cfg(feature = "dra")]
pub mod dra_plugin;
#[cfg(feature = "dra")]
pub mod dra_registration;
#[cfg(feature = "dra")]
pub mod kube_mock;

fn default<T: Default>() -> T {
    T::default()
}
