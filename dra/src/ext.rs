use super::*;

/// Typed API factories that centralize Kubernetes resource scope selection.
pub trait KubeClientExt {
    fn nodes(&self) -> api::Api<corev1::Node>;
    fn resource_claims(&self, claim: &ClaimRef) -> api::Api<resourcev1::ResourceClaim>;
    fn resource_slices(&self) -> api::Api<resourcev1::ResourceSlice>;
    fn resource_pool_status_requests(
        &self,
    ) -> api::Api<resourcev1alpha3::ResourcePoolStatusRequest>;
}

#[allow(
    clippy::disallowed_methods,
    reason = "Kubernetes API construction is centralized in this extension trait"
)]
impl KubeClientExt for kube::Client {
    fn nodes(&self) -> api::Api<corev1::Node> {
        api::Api::all(self.clone())
    }

    fn resource_claims(&self, claim: &ClaimRef) -> api::Api<resourcev1::ResourceClaim> {
        api::Api::namespaced(self.clone(), &claim.namespace)
    }

    fn resource_slices(&self) -> api::Api<resourcev1::ResourceSlice> {
        api::Api::all(self.clone())
    }

    fn resource_pool_status_requests(
        &self,
    ) -> api::Api<resourcev1alpha3::ResourcePoolStatusRequest> {
        api::Api::all(self.clone())
    }
}
