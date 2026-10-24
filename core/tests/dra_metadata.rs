use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use async_trait::async_trait;

use k8s_device_plugin_core::AttributeValue;
use k8s_device_plugin_core::ClaimPreparer;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::PoolDevice;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::PreparedDevice;
use k8s_device_plugin_core::PreparedDeviceWithMetadata;
use k8s_device_plugin_core::ResolvedClaim;

#[test]
fn attribute_builders_preserve_scalar_types_and_explicit_versions() {
    let pool_device = PoolDevice::new("device")
        .attribute("model", "widget")
        .attribute("owned", String::from("owned"))
        .attribute("count", 7)
        .attribute("enabled", true)
        .attribute("string_version", "1.2.3")
        .attribute("version", AttributeValue::version("1.2.3"));
    let prepared = PreparedDeviceWithMetadata::new(PreparedDevice {
        request_names: vec!["a".into()],
        pool_name: "pool".into(),
        device_name: "device".into(),
        cdi_device_ids: vec![],
    })
    .with_attribute("model", "widget")
    .with_attribute("owned", String::from("owned"))
    .with_attribute("count", 7)
    .with_attribute("enabled", true)
    .with_attribute("string_version", "1.2.3")
    .with_attribute("version", AttributeValue::version(String::from("1.2.3")));

    let expected = BTreeMap::from([
        ("model".into(), AttributeValue::String("widget".into())),
        ("owned".into(), AttributeValue::String("owned".into())),
        ("count".into(), AttributeValue::Int(7)),
        ("enabled".into(), AttributeValue::Bool(true)),
        (
            "string_version".into(),
            AttributeValue::String("1.2.3".into()),
        ),
        ("version".into(), AttributeValue::Version("1.2.3".into())),
    ]);
    assert_eq!(pool_device.attributes, expected);
    assert_eq!(prepared.attributes, expected);
}

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
                                PreparedDeviceWithMetadata::new(d).with_attribute("model", "widget")
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
            let claims = ["one", "two"]
                .into_iter()
                .map(|name| ResolvedClaim {
                    claim: ClaimRef {
                        namespace: "default".into(),
                        name: name.into(),
                        uid: name.into(),
                    },
                    devices: vec![],
                })
                .collect::<Vec<_>>();
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
