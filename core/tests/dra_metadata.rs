use async_trait::async_trait;
use k8s_device_plugin_core::{
    AttributeValue, ClaimPreparer, ClaimRef, PrepareError, PreparedDevice,
    PreparedDeviceWithMetadata, ResolvedClaim,
};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Debug, Default)]
struct Legacy(AtomicUsize);
#[async_trait]
impl ClaimPreparer for Legacy {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        claims
            .iter()
            .map(|c| {
                (
                    c.claim.clone(),
                    Ok(vec![PreparedDevice {
                        request_names: vec!["a".into()],
                        pool_name: "pool".into(),
                        device_name: "device".into(),
                        cdi_device_ids: vec![],
                    }]),
                )
            })
            .collect()
    }
    async fn unprepare(&self, _: &ClaimRef) -> Result<(), PrepareError> {
        Ok(())
    }
}
#[derive(Debug, Default)]
struct Rich(Legacy);
#[async_trait]
impl ClaimPreparer for Rich {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        self.0.prepare(claims).await
    }
    async fn prepare_with_metadata(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDeviceWithMetadata>, PrepareError>> {
        self.prepare(claims)
            .await
            .into_iter()
            .map(|(c, r)| {
                (
                    c,
                    r.map(|ds| {
                        ds.into_iter()
                            .map(|d| {
                                PreparedDeviceWithMetadata::new(d).with_attribute(
                                    "model",
                                    AttributeValue::String("widget".into()),
                                )
                            })
                            .collect()
                    }),
                )
            })
            .collect()
    }
    async fn unprepare(&self, _: &ClaimRef) -> Result<(), PrepareError> {
        Ok(())
    }
}
#[test]
fn default_hook_batches_once_and_arc_trait_object_forwards_overrides() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let claims: Vec<_> = ["one", "two"]
                .into_iter()
                .map(|name| ResolvedClaim {
                    claim: ClaimRef {
                        namespace: "default".into(),
                        name: name.into(),
                        uid: name.into(),
                    },
                    devices: vec![],
                })
                .collect();
            let legacy = Legacy::default();
            let result = legacy.prepare_with_metadata(&claims).await;
            assert_eq!(legacy.0.load(Ordering::SeqCst), 1);
            assert_eq!(result.len(), 2);
            assert!(
                result
                    .values()
                    .all(|r| r.as_ref().unwrap()[0].attributes.is_empty())
            );
            let rich = Arc::new(Rich::default());
            let erased: Arc<dyn ClaimPreparer> = rich.clone();
            let result = ClaimPreparer::prepare_with_metadata(&erased, &claims).await;
            assert_eq!(rich.0.0.load(Ordering::SeqCst), 1);
            assert!(
                result
                    .values()
                    .all(|r| r.as_ref().unwrap()[0].attributes.contains_key("model"))
            );
        });
}
