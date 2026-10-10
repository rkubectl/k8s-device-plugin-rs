use super::*;

pub trait KubeClientExt {
    fn nodes(&self) -> api::Api<corev1::Node>;
    fn resource_claims(&self, claim: &ClaimRef) -> api::Api<resourcev1::ResourceClaim>;
    fn resource_slices(&self) -> api::Api<resourcev1::ResourceSlice>;
}

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
}
