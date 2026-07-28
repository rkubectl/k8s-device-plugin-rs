//! `ResourceClaim` resolver (claim reference -> `ResolvedClaim`) — see
//! beads issue 9uf.6.

use std::fmt;

use k8s_device_plugin_core::AllocatedDevice;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::ResolvedClaim;
use k8s_openapi::api::resource::v1::ResourceClaim;
use kube::Api;
use kube::Client;

/// Resolves the bare `dra.v1.Claim` reference `NodePrepareResources` gives
/// the driver into what the scheduler actually allocated, by reading the
/// real `ResourceClaim` object's `status.allocation` from the API server.
#[derive(Clone)]
pub struct ClaimResolver {
    client: Client,
}

impl fmt::Debug for ClaimResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimResolver").finish_non_exhaustive()
    }
}

impl ClaimResolver {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Resolves one claim reference to what the scheduler actually allocated.
    pub async fn resolve(&self, claim_ref: &ClaimRef) -> Result<ResolvedClaim, PrepareError> {
        let api: Api<ResourceClaim> = Api::namespaced(self.client.clone(), &claim_ref.namespace);
        let claim = api.get(&claim_ref.name).await.map_err(|err| {
            PrepareError::ResolutionFailed(format!(
                "failed to fetch ResourceClaim {}/{}: {err}",
                claim_ref.namespace, claim_ref.name
            ))
        })?;

        // The UID is the authoritative identity per the wire protocol's own
        // doc comments -- a stale/reused name+namespace could otherwise
        // point at a different object than the one kubelet meant.
        let found_uid = claim.metadata.uid.as_deref().unwrap_or_default();
        if found_uid != claim_ref.uid {
            return Err(PrepareError::ResolutionFailed(format!(
                "ResourceClaim {}/{} uid mismatch: expected {}, found {found_uid}",
                claim_ref.namespace, claim_ref.name, claim_ref.uid
            )));
        }

        let allocation = claim
            .status
            .as_ref()
            .and_then(|status| status.allocation.as_ref())
            .ok_or_else(|| PrepareError::ClaimNotAllocated(claim_ref.clone()))?;

        let devices = allocation
            .devices
            .as_ref()
            .and_then(|devices| devices.results.as_ref())
            .into_iter()
            .flatten()
            .map(|result| AllocatedDevice {
                pool_name: result.pool.clone(),
                device_name: result.device.clone(),
                request_name: (!result.request.is_empty()).then(|| result.request.clone()),
            })
            .collect();

        Ok(ResolvedClaim {
            claim: claim_ref.clone(),
            devices,
        })
    }

    /// Resolves every claim in one `NodePrepareResourcesRequest` batch
    /// concurrently. Phase 1 deliberately uses a direct `Api::get` per claim
    /// rather than a shared informer/reflector cache -- `NodePrepareResources`
    /// calls aren't high-frequency, and a reflector is more machinery than
    /// this phase needs (mirrors the same simplification `docs/dra-design.md`
    /// calls for on the `ResourceSlice` publisher side; both can be
    /// revisited in Phase 2 if API-server load becomes a concern).
    pub async fn resolve_all(
        &self,
        claim_refs: &[ClaimRef],
    ) -> Vec<(ClaimRef, Result<ResolvedClaim, PrepareError>)> {
        futures::future::join_all(
            claim_refs
                .iter()
                .map(|claim_ref| async move { (claim_ref.clone(), self.resolve(claim_ref).await) }),
        )
        .await
    }
}

#[cfg(test)]
mod tests;
