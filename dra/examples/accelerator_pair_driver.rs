//! A topology-aware DRA example for accelerator cards that must be allocated
//! in connected pairs.
//!
//! The scheduler allocates *pair bundles*, not individual cards. Each bundle
//! is one DRA device with immutable member-card and interconnect attributes.
//! This prevents a request for two cards from being satisfied by an arbitrary,
//! disconnected combination. The prepared CDI device exposes both member IDs
//! to the workload. See `examples/accelerator-pairs/README.md` for deployment
//! and production-integration guidance.

use std::collections::BTreeMap;
use std::env;
use std::io;

use async_trait::async_trait;
use k8s_device_plugin_core::AllocatedDevice;
use k8s_device_plugin_core::AttributeValue;
use k8s_device_plugin_core::ClaimPreparer;
use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_core::DraDriver;
use k8s_device_plugin_core::PoolDevice;
use k8s_device_plugin_core::PrepareError;
use k8s_device_plugin_core::PreparedDevice;
use k8s_device_plugin_core::ResolvedClaim;
use k8s_device_plugin_core::ResourcePool;
use k8s_device_plugin_dra::DraPlugin;

const DEFAULT_DRIVER_NAME: &str = "accelerator.example.com";
const DEFAULT_POOL_NAME: &str = "accelerator-pairs";
const CDI_SPEC_DIR: &str = "/var/run/cdi";
const CDI_SPEC_PATH: &str = "/var/run/cdi/accelerator-pairs.json";
const CDI_KIND: &str = "accelerator.example.com/pair";

const ATTRIBUTE_KIND: &str = "accelerator.example.com/kind";
const ATTRIBUTE_INTERCONNECT: &str = "accelerator.example.com/interconnect";
const ATTRIBUTE_MEMBER_0: &str = "accelerator.example.com/member0";
const ATTRIBUTE_MEMBER_1: &str = "accelerator.example.com/member1";

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceleratorPair {
    name: &'static str,
    members: [&'static str; 2],
    interconnect: &'static str,
}

impl AcceleratorPair {
    fn pool_device(&self) -> PoolDevice {
        PoolDevice::new(self.name)
            .attribute(
                ATTRIBUTE_KIND,
                AttributeValue::String("connected-pair".to_string()),
            )
            .attribute(
                ATTRIBUTE_INTERCONNECT,
                AttributeValue::String(self.interconnect.to_string()),
            )
            .attribute(
                ATTRIBUTE_MEMBER_0,
                AttributeValue::String(self.members[0].to_string()),
            )
            .attribute(
                ATTRIBUTE_MEMBER_1,
                AttributeValue::String(self.members[1].to_string()),
            )
    }
}

fn example_pairs() -> [AcceleratorPair; 2] {
    [
        AcceleratorPair {
            name: "pair-0-1",
            members: ["accelerator-0", "accelerator-1"],
            interconnect: "nvlink",
        },
        AcceleratorPair {
            name: "pair-2-3",
            members: ["accelerator-2", "accelerator-3"],
            interconnect: "nvlink",
        },
    ]
}

fn cdi_device_id(pair_name: &str) -> String {
    format!("{CDI_KIND}={pair_name}")
}

async fn write_cdi_spec(pairs: &[AcceleratorPair]) -> io::Result<()> {
    let spec = serde_json::json!({
        "cdiVersion": "0.8.0",
        "kind": CDI_KIND,
        "devices": pairs.iter().map(|pair| serde_json::json!({
            "name": pair.name,
            "containerEdits": {
                "env": [
                    format!("ACCELERATOR_PAIR={}", pair.name),
                    format!("ACCELERATOR_PAIR_MEMBERS={}", pair.members.join(",")),
                    format!("ACCELERATOR_INTERCONNECT={}", pair.interconnect),
                ],
            },
        })).collect::<Vec<_>>(),
    });

    tokio::fs::create_dir_all(CDI_SPEC_DIR).await?;
    let encoded = serde_json::to_vec_pretty(&spec).map_err(io::Error::other)?;
    tokio::fs::write(CDI_SPEC_PATH, encoded).await
}

#[derive(Debug)]
struct AcceleratorPairDriver {
    name: String,
    pool: String,
    pairs: BTreeMap<&'static str, AcceleratorPair>,
}

impl AcceleratorPairDriver {
    fn new(name: String, pool: String, pairs: impl IntoIterator<Item = AcceleratorPair>) -> Self {
        Self {
            name,
            pool,
            pairs: pairs.into_iter().map(|pair| (pair.name, pair)).collect(),
        }
    }

    fn prepare_device(&self, device: &AllocatedDevice) -> Result<PreparedDevice, PrepareError> {
        if device.pool_name != self.pool {
            return Err(PrepareError::DeviceUnavailable(format!(
                "{}/{}",
                device.pool_name, device.device_name
            )));
        }

        let Some(pair) = self.pairs.get(device.device_name.as_str()) else {
            return Err(PrepareError::DeviceUnavailable(format!(
                "{}/{} is not an advertised connected pair",
                device.pool_name, device.device_name
            )));
        };

        Ok(PreparedDevice {
            request_names: device.request_name.clone().into_iter().collect(),
            pool_name: device.pool_name.clone(),
            device_name: pair.name.to_string(),
            cdi_device_ids: vec![cdi_device_id(pair.name)],
        })
    }
}

#[async_trait]
impl ResourcePool for AcceleratorPairDriver {
    async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
        BTreeMap::from([(
            self.pool.clone(),
            self.pairs
                .values()
                .map(AcceleratorPair::pool_device)
                .collect(),
        )])
    }
}

#[async_trait]
impl ClaimPreparer for AcceleratorPairDriver {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        claims
            .iter()
            .map(|resolved| {
                tracing::info!(
                    claim_namespace = %resolved.claim.namespace,
                    claim_name = %resolved.claim.name,
                    claim_uid = %resolved.claim.uid,
                    pair_count = resolved.devices.len(),
                    "preparing connected accelerator pair claim"
                );
                let prepared = resolved
                    .devices
                    .iter()
                    .map(|device| self.prepare_device(device))
                    .collect();
                (resolved.claim.clone(), prepared)
            })
            .collect()
    }

    async fn unprepare(&self, claim: &ClaimRef) -> Result<(), PrepareError> {
        tracing::info!(
            claim_namespace = %claim.namespace,
            claim_name = %claim.name,
            claim_uid = %claim.uid,
            "unpreparing connected accelerator pair claim"
        );
        Ok(())
    }
}

impl DraDriver for AcceleratorPairDriver {
    fn driver_name(&self) -> &str {
        &self.name
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let driver_name = env_or_default("DRIVER_NAME", DEFAULT_DRIVER_NAME);
    let pool_name = env_or_default("POOL_NAME", DEFAULT_POOL_NAME);
    let node_name = env::var("NODE_NAME")
        .map_err(|error| io::Error::other(format!("NODE_NAME must be set: {error}")))?;
    let pairs = example_pairs();

    write_cdi_spec(&pairs).await?;
    tracing::info!(
        %driver_name,
        %pool_name,
        %node_name,
        pair_count = pairs.len(),
        "starting topology-aware accelerator-pair DRA driver"
    );

    let client = kube::Client::try_default()
        .await
        .map_err(io::Error::other)?;
    let driver = AcceleratorPairDriver::new(driver_name.clone(), pool_name, pairs);
    DraPlugin::new(client, driver_name, node_name, driver)
        .run()
        .await
}

fn env_or_default(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver() -> AcceleratorPairDriver {
        AcceleratorPairDriver::new(
            DEFAULT_DRIVER_NAME.to_string(),
            DEFAULT_POOL_NAME.to_string(),
            example_pairs(),
        )
    }

    fn allocated_device(name: &str) -> AllocatedDevice {
        AllocatedDevice {
            pool_name: DEFAULT_POOL_NAME.to_string(),
            device_name: name.to_string(),
            request_name: Some("accelerator-pair".to_string()),
        }
    }

    #[test]
    fn inventory_advertises_only_connected_pairs() {
        let pairs = example_pairs();
        let devices: Vec<_> = pairs.iter().map(AcceleratorPair::pool_device).collect();

        assert_eq!(
            devices
                .iter()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>(),
            ["pair-0-1", "pair-2-3"]
        );
        assert_eq!(
            devices[0].attributes[ATTRIBUTE_MEMBER_0],
            AttributeValue::String("accelerator-0".to_string())
        );
        assert_eq!(
            devices[0].attributes[ATTRIBUTE_MEMBER_1],
            AttributeValue::String("accelerator-1".to_string())
        );
    }

    #[test]
    fn preparation_returns_one_bundle_cdi_device_for_the_pair() {
        let prepared = driver()
            .prepare_device(&allocated_device("pair-0-1"))
            .expect("advertised pair must be prepared");

        assert_eq!(prepared.device_name, "pair-0-1");
        assert_eq!(
            prepared.cdi_device_ids,
            ["accelerator.example.com/pair=pair-0-1"]
        );
    }

    #[test]
    fn preparation_rejects_an_individual_card() {
        let error = driver()
            .prepare_device(&allocated_device("accelerator-0"))
            .expect_err("individual cards must never be accepted as pair bundles");

        assert!(matches!(error, PrepareError::DeviceUnavailable(_)));
    }

    #[test]
    fn cdi_device_id_uses_the_pair_kind() {
        assert_eq!(
            cdi_device_id("pair-2-3"),
            "accelerator.example.com/pair=pair-2-3"
        );
    }
}
