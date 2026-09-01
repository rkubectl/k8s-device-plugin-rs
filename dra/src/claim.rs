//! `ResourceClaim` resolver (claim reference -> `ResolvedClaim`) — see
//! beads issue 9uf.6.

use std::fmt;
use std::time::Duration;

use futures::StreamExt;
use k8s_device_plugin_core::AllocatedDevice;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::ResolvedClaim;
use k8s_openapi::api::resource::v1::ResourceClaim;
use kube::Api;
use kube::Client;

const DEFAULT_MAX_CONCURRENT_RESOLVES: usize = 16;
const DEFAULT_MAX_ATTEMPTS: usize = 3;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Resolves the bare `dra.v1.Claim` reference `NodePrepareResources` gives
/// the driver into what the scheduler actually allocated, by reading the
/// real `ResourceClaim` object's `status.allocation` from the API server.
#[derive(Clone)]
pub struct ClaimResolver {
    client: Client,
    max_concurrent_resolves: usize,
    max_attempts: usize,
    retry_delay: Duration,
}

impl fmt::Debug for ClaimResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimResolver").finish_non_exhaustive()
    }
}

impl ClaimResolver {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            max_concurrent_resolves: DEFAULT_MAX_CONCURRENT_RESOLVES,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }

    /// Caps the number of concurrent API reads made for one kubelet request.
    /// A zero value is clamped to one read at a time.
    #[must_use]
    pub fn with_max_concurrent_resolves(mut self, max_concurrent_resolves: usize) -> Self {
        self.max_concurrent_resolves = max_concurrent_resolves.max(1);
        self
    }

    /// Configures bounded retries for transient API reads. A zero attempt
    /// count is clamped to one, preserving a prompt terminal result.
    #[must_use]
    pub fn with_retry_policy(mut self, max_attempts: usize, retry_delay: Duration) -> Self {
        self.max_attempts = max_attempts.max(1);
        self.retry_delay = retry_delay;
        self
    }

    /// Resolves one claim reference to what the scheduler actually allocated.
    pub async fn resolve(&self, claim_ref: &ClaimRef) -> Result<ResolvedClaim, PrepareError> {
        let api: Api<ResourceClaim> = Api::namespaced(self.client.clone(), &claim_ref.namespace);
        let mut attempt = 0;
        let claim = loop {
            attempt += 1;
            match api.get(&claim_ref.name).await {
                Ok(claim) => break claim,
                Err(err) if attempt < self.max_attempts => {
                    tracing::debug!(
                        attempt,
                        claim = %format_args!("{}/{}", claim_ref.namespace, claim_ref.name),
                        %err,
                        "retrying ResourceClaim resolution"
                    );
                    tokio::time::sleep(self.retry_delay).await;
                }
                Err(err) => {
                    return Err(PrepareError::ResolutionFailed(format!(
                        "failed to fetch ResourceClaim {}/{} after {attempt} attempt(s): {err}",
                        claim_ref.namespace, claim_ref.name
                    )));
                }
            }
        };

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
    /// concurrently, with an explicit upper bound to avoid one kubelet RPC
    /// opening an unbounded number of API requests. Results retain request
    /// order even though individual reads may finish out of order.
    pub async fn resolve_all(
        &self,
        claim_refs: &[ClaimRef],
    ) -> Vec<(ClaimRef, Result<ResolvedClaim, PrepareError>)> {
        let mut resolved = futures::stream::iter(claim_refs.iter().cloned().enumerate())
            .map(|(index, claim_ref)| async move {
                let result = self.resolve(&claim_ref).await;
                (index, claim_ref, result)
            })
            .buffer_unordered(self.max_concurrent_resolves)
            .collect::<Vec<_>>()
            .await;
        resolved.sort_by_key(|(index, _, _)| *index);
        resolved
            .into_iter()
            .map(|(_, claim_ref, result)| (claim_ref, result))
            .collect()
    }
}

#[cfg(test)]
mod tests;
