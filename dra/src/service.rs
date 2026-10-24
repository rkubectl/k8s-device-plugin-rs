//! `DRAPlugin` gRPC service (`NodePrepareResources`/`NodeUnprepareResources`)
//! — see beads issue 9uf.7.

use tokio_stream::wrappers::UnixListenerStream;

use super::*;

fn claim_from_wire(claim: dra::v1::Claim) -> ClaimRef {
    ClaimRef {
        namespace: claim.namespace,
        uid: claim.uid,
        name: claim.name,
    }
}

fn prepared_device_to_wire(device: PreparedDevice) -> dra::v1::Device {
    dra::v1::Device {
        request_names: device.request_names,
        pool_name: device.pool_name,
        device_name: device.device_name,
        cdi_device_ids: device.cdi_device_ids,
        share_id: None,
    }
}

fn node_prepare_response(
    result: Result<Vec<PreparedDevice>, PrepareError>,
) -> dra::v1::NodePrepareResourceResponse {
    match result {
        Ok(devices) => dra::v1::NodePrepareResourceResponse {
            devices: devices.into_iter().map(prepared_device_to_wire).collect(),
            error: String::new(),
        },
        Err(err) => dra::v1::NodePrepareResourceResponse {
            devices: vec![],
            error: err.to_string(),
        },
    }
}

/// The `DRAPlugin` gRPC service's own socket path -- shared with the
/// lifecycle harness (`DraPlugin::new`, crate root), which needs the same
/// path to advertise as the registration server's `GetInfo.endpoint`.
pub(crate) fn plugin_socket_path(driver_name: &str) -> PathBuf {
    Path::new(dra::KUBELET_PLUGINS_PATH)
        .join(driver_name)
        .join("plugin.sock")
}

async fn setup_listener(socket_path: &Path) -> io::Result<UnixListenerStream> {
    let socket_path = socket_path.to_path_buf();
    let listener =
        task::spawn_blocking(move || k8s_device_plugin_core::bind_unix_listener(&socket_path))
            .await
            .map_err(io::Error::other)??;
    net::UnixListener::from_std(listener).map(UnixListenerStream::new)
}

/// Adapter between the wire-level `dra.v1.DRAPlugin` gRPC service and a
/// backend's [`ClaimPreparer`]. The DRA analog of
/// `k8s_device_plugin_lib`'s `DevicePluginService` -- referenced here only
/// for orientation, not a dependency of this crate.
pub struct DraPluginService {
    resolver: ClaimResolver,
    preparer: Arc<dyn ClaimPreparer>,
    metadata: Option<Arc<metadata::MetadataRuntime>>,
    #[cfg(feature = "resource-health")]
    health: DraResourceHealthService,
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
            metadata: None,
            #[cfg(feature = "resource-health")]
            health: DraResourceHealthService::unsupported(),
        }
    }

    /// Enables standard beta container metadata. No files are written until a
    /// claim is prepared. Existing health configuration remains in effect.
    pub fn with_device_metadata(
        mut self,
        driver_name: impl Into<String>,
        options: DeviceMetadataOptions,
    ) -> io::Result<Self> {
        self.metadata = Some(Arc::new(metadata::MetadataRuntime::new(
            driver_name.into(),
            options,
        )?));
        Ok(self)
    }

    async fn prepare_metadata(
        &self,
        refs: Vec<ClaimRef>,
        metadata: &Arc<metadata::MetadataRuntime>,
    ) -> BTreeMap<String, dra::v1::NodePrepareResourceResponse> {
        let guards = Arc::new(metadata.lock_claims(&refs).await);
        let mut errors = BTreeMap::new();
        let mut objects = BTreeMap::new();
        let mut resolved = Vec::new();
        for (claim, result) in self.resolver.resolve_all_context(&refs).await {
            match result {
                Ok(context) => {
                    objects.insert(claim, context.object);
                    resolved.push(context.resolved);
                }
                Err(error) => {
                    errors.insert(claim, error);
                }
            }
        }
        let mut prepared = self.preparer.prepare_with_metadata(&resolved).await;
        let mut results = BTreeMap::new();
        for claim in refs {
            let result = if let Some(error) = errors.remove(&claim) {
                Err(error)
            } else {
                match (prepared.remove(&claim), objects.remove(&claim)) {
                    (Some(Ok(devices)), Some(object)) => metadata
                        .publish(object, devices, Arc::clone(&guards))
                        .await
                        .map_err(|error| {
                            PrepareError::HookFailed(format!("container metadata: {error}"))
                        }),
                    (Some(Err(error)), _) => Err(error),
                    _ => Err(PrepareError::HookFailed(
                        "claim preparer returned no result for this claim".into(),
                    )),
                }
            };
            results.insert(claim.uid, node_prepare_response(result));
        }
        results
    }

    #[cfg(feature = "resource-health")]
    /// Adds the optional `DRAResourceHealth` service to this plugin socket.
    pub fn with_resource_health<R: ResourceHealthReporter + 'static>(
        mut self,
        reporter: R,
    ) -> Self {
        self.health = DraResourceHealthService::new(reporter);
        self
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
    ) -> io::Result<task::JoinHandle<Result<(), transport::Error>>> {
        self.spawn_at(&plugin_socket_path(driver_name)).await
    }

    pub(crate) async fn spawn_at(
        self,
        socket_path: &Path,
    ) -> io::Result<task::JoinHandle<Result<(), transport::Error>>> {
        let incoming = setup_listener(socket_path).await?;
        let mut router = transport::Server::builder();
        #[cfg(feature = "resource-health")]
        let router = router.add_service(dra::health::v1alpha1::DraResourceHealthServer::new(
            self.health.clone(),
        ));
        let router = router.add_service(dra::v1::DraPluginServer::new(self));
        Ok(tokio::spawn(router.serve_with_incoming(incoming)))
    }
}

#[tonic::async_trait]
impl dra::v1::DraPlugin for DraPluginService {
    #[tracing::instrument(skip(self, request))]
    async fn node_prepare_resources(
        &self,
        request: tonic::Request<dra::v1::NodePrepareResourcesRequest>,
    ) -> tonic::Result<tonic::Response<dra::v1::NodePrepareResourcesResponse>> {
        let claim_refs = request
            .into_inner()
            .claims
            .into_iter()
            .map(claim_from_wire)
            .collect::<Vec<_>>();

        if let Some(metadata) = &self.metadata {
            let claims = self.prepare_metadata(claim_refs, metadata).await;
            return Ok(tonic::Response::new(
                dra::v1::NodePrepareResourcesResponse {
                    claims: claims.into_iter().collect(),
                },
            ));
        }

        // `ClaimResolver::resolve_all` already resolves a batch
        // concurrently via `futures::future::join_all` -- reused directly
        // rather than re-deriving the "one task per item" pattern the
        // classic `allocate` RPC uses, since the claim-resolver already
        // built exactly this for exactly this purpose.
        let resolved = self.resolver.resolve_all(&claim_refs).await;

        let mut resolve_errors = BTreeMap::new();
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

        Ok(tonic::Response::new(
            dra::v1::NodePrepareResourcesResponse { claims },
        ))
    }

    #[tracing::instrument(skip(self, request))]
    async fn node_unprepare_resources(
        &self,
        request: tonic::Request<dra::v1::NodeUnprepareResourcesRequest>,
    ) -> tonic::Result<tonic::Response<dra::v1::NodeUnprepareResourcesResponse>> {
        let claim_refs = request
            .into_inner()
            .claims
            .into_iter()
            .map(claim_from_wire)
            .collect::<Vec<_>>();

        let claims = futures::future::join_all(claim_refs.into_iter().map(|claim_ref| {
            let preparer = Arc::clone(&self.preparer);
            let metadata = self.metadata.clone();
            async move {
                let result = async {
                    let guards = if let Some(metadata) = &metadata {
                        Some(Arc::new(
                            metadata.lock_claims(std::slice::from_ref(&claim_ref)).await,
                        ))
                    } else {
                        None
                    };
                    preparer.unprepare(&claim_ref).await?;
                    if let (Some(metadata), Some(guards)) = (metadata, guards) {
                        metadata
                            .cleanup(claim_ref.clone(), guards)
                            .await
                            .map_err(|e| {
                                PrepareError::HookFailed(format!("container metadata cleanup: {e}"))
                            })?;
                    }
                    Ok::<(), PrepareError>(())
                }
                .await;
                let error = result.err().map_or_default(|error| error.to_string());
                (
                    claim_ref.uid,
                    dra::v1::NodeUnprepareResourceResponse { error },
                )
            }
        }))
        .await
        .into_iter()
        .collect();

        Ok(tonic::Response::new(
            dra::v1::NodeUnprepareResourcesResponse { claims },
        ))
    }
}

#[cfg(test)]
mod tests;
