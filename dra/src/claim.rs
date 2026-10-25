//! `ResourceClaim` resolver (claim reference -> `ResolvedClaim`) — see
//! beads issue 9uf.6.

use futures::StreamExt;

use super::*;

const DEFAULT_MAX_CONCURRENT_RESOLVES: usize = 16;
const DEFAULT_MAX_ATTEMPTS: usize = 3;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Resolves the bare `dra.v1.Claim` reference `NodePrepareResources` gives
/// the driver into what the scheduler actually allocated, by reading the
/// real `ResourceClaim` object's `status.allocation` from the API server.
#[derive(Clone)]
pub struct ClaimResolver {
    driver_name: String,
    client: kube::Client,
    max_concurrent_resolves: usize,
    max_attempts: usize,
    retry_delay: Duration,
}

impl fmt::Debug for ClaimResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimResolver").finish_non_exhaustive()
    }
}

/// Metadata retains the original UID-checked object instead of fetching it again.
#[derive(Debug)]
pub(crate) struct ResolvedClaimContext {
    pub resolved: ResolvedClaim,
    pub object: resourcev1::ResourceClaim,
}

impl ClaimResolver {
    pub(crate) fn validate_identity(&self, driver_name: &str) -> Result<(), ValidationError> {
        validate_driver_name(driver_name)?;
        if driver_name != self.driver_name {
            return Err(ValidationError::DriverNameMismatch {
                runtime: driver_name.to_string(),
                backend: self.driver_name.clone(),
            });
        }
        Ok(())
    }

    /// Creates a resolver scoped to one DRA driver.
    ///
    /// # Errors
    ///
    /// Returns a validation error if the driver name is not a DNS subdomain
    /// of at most 63 bytes.
    pub fn new(
        client: kube::Client,
        driver_name: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let driver_name = driver_name.into();
        validate_driver_name(&driver_name)?;
        Ok(Self {
            driver_name,
            client,
            max_concurrent_resolves: DEFAULT_MAX_CONCURRENT_RESOLVES,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry_delay: DEFAULT_RETRY_DELAY,
        })
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
    /// Retries HTTP 408/429/5xx and recognized transport failures, not
    /// authorization, missing claims, decoding, or configuration errors.
    #[must_use]
    pub fn with_retry_policy(mut self, max_attempts: usize, retry_delay: Duration) -> Self {
        self.max_attempts = max_attempts.max(1);
        self.retry_delay = retry_delay;
        self
    }

    /// Resolves one claim reference to what the scheduler actually allocated.
    pub async fn resolve(&self, claim_ref: &ClaimRef) -> Result<ResolvedClaim, PrepareError> {
        self.resolve_context(claim_ref)
            .await
            .map(|context| context.resolved)
    }

    async fn resolve_context(
        &self,
        claim_ref: &ClaimRef,
    ) -> Result<ResolvedClaimContext, PrepareError> {
        let api = self.client.resource_claims(claim_ref);
        let mut attempt = 0;
        let claim = loop {
            attempt += 1;
            match api.get(&claim_ref.name).await {
                Ok(claim) => break claim,
                Err(err) if attempt < self.max_attempts && is_transient_read_error(&err) => {
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

        let local = allocation
            .devices
            .as_ref()
            .and_then(|devices| devices.results.as_ref())
            .into_iter()
            .flatten()
            .filter(|result| result.driver == self.driver_name)
            .collect::<Vec<_>>();
        if local.iter().any(|result| result.share_id.is_some()) {
            return Err(PrepareError::ResolutionFailed(
                "claim preparation does not yet support device share IDs".to_string(),
            ));
        }
        let devices = local
            .into_iter()
            .map(|result| AllocatedDevice {
                pool_name: result.pool.clone(),
                device_name: result.device.clone(),
                request_name: (!result.request.is_empty()).then(|| result.request.clone()),
            })
            .collect();

        Ok(ResolvedClaimContext {
            resolved: ResolvedClaim {
                claim: claim_ref.clone(),
                devices,
            },
            object: claim,
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
        self.resolve_all_context(claim_refs)
            .await
            .into_iter()
            .map(|(claim, result)| (claim, result.map(|context| context.resolved)))
            .collect()
    }

    pub(crate) async fn resolve_all_context(
        &self,
        claim_refs: &[ClaimRef],
    ) -> Vec<(ClaimRef, Result<ResolvedClaimContext, PrepareError>)> {
        let mut resolved = futures::stream::iter(claim_refs.iter().cloned().enumerate())
            .map(|(index, claim_ref)| async move {
                let result = self.resolve_context(&claim_ref).await;
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

fn is_transient_read_error(error: &kube::Error) -> bool {
    match error {
        kube::Error::Api(status) => matches!(status.code, 408 | 429 | 500..=599),
        kube::Error::HyperError(_) => true,
        kube::Error::Service(error) => {
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error.as_ref());
            while let Some(error) = source {
                if let Some(error) = error.downcast_ref::<kube::Error>() {
                    return is_transient_read_error(error);
                }
                if let Some(error) = error.downcast_ref::<io::Error>() {
                    return matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::NotConnected
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::UnexpectedEof
                    );
                }
                source = error.source();
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
