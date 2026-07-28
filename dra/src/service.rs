//! `DRAPlugin` gRPC service (`NodePrepareResources`/`NodeUnprepareResources`)
//! — see beads issue 9uf.7.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

use k8s_device_plugin_core::ClaimPreparer;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::PreparedDevice;
use k8s_device_plugin_proto::dra::KUBELET_PLUGINS_PATH;
use k8s_device_plugin_proto::dra::v1;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport;

use crate::claim::ClaimResolver;

fn claim_from_wire(claim: &v1::Claim) -> ClaimRef {
    ClaimRef {
        namespace: claim.namespace.clone(),
        uid: claim.uid.clone(),
        name: claim.name.clone(),
    }
}

fn prepared_device_to_wire(device: PreparedDevice) -> v1::Device {
    v1::Device {
        request_names: device.request_names,
        pool_name: device.pool_name,
        device_name: device.device_name,
        cdi_device_ids: device.cdi_device_ids,
        share_id: None,
    }
}

fn node_prepare_response(
    result: Result<Vec<PreparedDevice>, PrepareError>,
) -> v1::NodePrepareResourceResponse {
    match result {
        Ok(devices) => v1::NodePrepareResourceResponse {
            devices: devices.into_iter().map(prepared_device_to_wire).collect(),
            error: String::new(),
        },
        Err(err) => v1::NodePrepareResourceResponse {
            devices: vec![],
            error: err.to_string(),
        },
    }
}

async fn setup_listener(socket_path: &Path) -> io::Result<UnixListenerStream> {
    if tokio::fs::try_exists(socket_path).await? {
        tokio::fs::remove_file(socket_path).await?;
    }
    UnixListener::bind(socket_path).map(UnixListenerStream::new)
}

/// Adapter between the wire-level `dra.v1.DRAPlugin` gRPC service and a
/// backend's [`ClaimPreparer`]. The DRA analog of
/// [`k8s_device_plugin_lib`]'s `DevicePluginService` -- referenced here only
/// for orientation, not a dependency of this crate.
pub struct DraPluginService {
    resolver: ClaimResolver,
    preparer: Arc<dyn ClaimPreparer>,
}

impl fmt::Debug for DraPluginService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DraPluginService").finish_non_exhaustive()
    }
}

impl DraPluginService {
    pub fn new<P: ClaimPreparer + 'static>(resolver: ClaimResolver, preparer: P) -> Self {
        Self {
            resolver,
            preparer: Arc::new(preparer),
        }
    }

    /// Binds `/var/lib/kubelet/plugins/<driver_name>/plugin.sock` and spawns
    /// the gRPC server.
    ///
    /// Does not create the parent directory: per the crate-boundary split
    /// in `docs/dra-design.md`, that (and the registration socket's
    /// directory) is the lifecycle harness's job -- this fails clearly via
    /// `UnixListener::bind` if the directory is missing, rather than
    /// silently creating it here.
    pub async fn spawn(
        self,
        driver_name: &str,
    ) -> io::Result<JoinHandle<Result<(), transport::Error>>> {
        let socket_path = Path::new(KUBELET_PLUGINS_PATH)
            .join(driver_name)
            .join("plugin.sock");
        self.spawn_at(&socket_path).await
    }

    async fn spawn_at(
        self,
        socket_path: &Path,
    ) -> io::Result<JoinHandle<Result<(), transport::Error>>> {
        let incoming = setup_listener(socket_path).await?;
        let router = transport::Server::builder().add_service(v1::DraPluginServer::new(self));
        Ok(tokio::spawn(router.serve_with_incoming(incoming)))
    }
}

#[tonic::async_trait]
impl v1::DraPlugin for DraPluginService {
    #[tracing::instrument(skip(self, request))]
    async fn node_prepare_resources(
        &self,
        request: tonic::Request<v1::NodePrepareResourcesRequest>,
    ) -> tonic::Result<tonic::Response<v1::NodePrepareResourcesResponse>> {
        let claim_refs: Vec<ClaimRef> = request
            .into_inner()
            .claims
            .iter()
            .map(claim_from_wire)
            .collect();

        // `ClaimResolver::resolve_all` already resolves a batch
        // concurrently via `futures::future::join_all` -- reused directly
        // rather than re-deriving the "one task per item" pattern the
        // classic `allocate` RPC uses, since the claim-resolver already
        // built exactly this for exactly this purpose.
        let resolved = self.resolver.resolve_all(&claim_refs).await;

        let mut resolve_errors = HashMap::new();
        let mut resolved_claims = Vec::new();
        for (claim_ref, result) in resolved {
            match result {
                Ok(resolved_claim) => resolved_claims.push(resolved_claim),
                Err(err) => {
                    resolve_errors.insert(claim_ref, err);
                }
            }
        }

        // Unlike the classic `allocate` RPC (which aborts every in-flight
        // container task the instant one fails, since a single container's
        // failure fails the whole pod), the DRA wire protocol requires an
        // entry for *every* requested claim regardless of sibling
        // failures -- nothing here may short-circuit another claim's
        // resolution or preparation, and a per-claim failure must be
        // reported inside the response rather than as an RPC-level error
        // (kubelet ignores the entire response if one is returned).
        let mut prepare_results = self.preparer.prepare(&resolved_claims).await;

        let claims = claim_refs
            .into_iter()
            .map(|claim_ref| {
                // Iterating the original request's claims (rather than
                // unioning whatever the maps above happen to contain) is
                // what guarantees every requested claim UID gets a
                // response entry, including the defensive fallback below
                // if a `ClaimPreparer` impl violates its own contract and
                // drops a claim from its result map.
                let result = resolve_errors
                    .remove(&claim_ref)
                    .map(Err)
                    .or_else(|| prepare_results.remove(&claim_ref))
                    .unwrap_or_else(|| {
                        Err(PrepareError::HookFailed(
                            "claim preparer returned no result for this claim".to_string(),
                        ))
                    });
                (claim_ref.uid, node_prepare_response(result))
            })
            .collect();

        Ok(tonic::Response::new(v1::NodePrepareResourcesResponse {
            claims,
        }))
    }

    #[tracing::instrument(skip(self, request))]
    async fn node_unprepare_resources(
        &self,
        request: tonic::Request<v1::NodeUnprepareResourcesRequest>,
    ) -> tonic::Result<tonic::Response<v1::NodeUnprepareResourcesResponse>> {
        let claim_refs: Vec<ClaimRef> = request
            .into_inner()
            .claims
            .iter()
            .map(claim_from_wire)
            .collect();

        let claims = futures::future::join_all(claim_refs.into_iter().map(|claim_ref| {
            let preparer = Arc::clone(&self.preparer);
            async move {
                let error = preparer
                    .unprepare(&claim_ref)
                    .await
                    .err()
                    .map(|err| err.to_string())
                    .unwrap_or_default();
                (claim_ref.uid, v1::NodeUnprepareResourceResponse { error })
            }
        }))
        .await
        .into_iter()
        .collect();

        Ok(tonic::Response::new(v1::NodeUnprepareResourcesResponse {
            claims,
        }))
    }
}

#[cfg(test)]
mod tests;
