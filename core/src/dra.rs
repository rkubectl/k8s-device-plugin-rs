use super::*;

/// A single typed attribute value, as used by DRA's CEL-based device
/// selectors (`DeviceClass`/`ResourceClaim` selector expressions match
/// against these).
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    String(String),
    Int(i64),
    Bool(bool),
    /// A semver-style version string, per the DRA API's `version` attribute
    /// kind.
    Version(String),
}

/// A device capacity quantity, e.g. memory or bandwidth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quantity(pub i64);

/// A device this driver can offer, with the attributes and capacity DRA's
/// device selectors match against — richer than the classic plugin's
/// [`crate::Device`], which is just an id, health, and host paths.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolDevice {
    pub name: String,
    pub attributes: HashMap<String, AttributeValue>,
    pub capacity: HashMap<String, Quantity>,
    pub health: Health,
}

impl PoolDevice {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: HashMap::new(),
            capacity: HashMap::new(),
            health: Health::Healthy,
        }
    }

    pub fn health(self, health: Health) -> Self {
        Self { health, ..self }
    }

    pub fn attribute(mut self, key: impl Into<String>, value: AttributeValue) -> Self {
        self.attributes.insert(key.into(), value);
        self
    }

    pub fn capacity(mut self, key: impl Into<String>, quantity: Quantity) -> Self {
        self.capacity.insert(key.into(), quantity);
        self
    }
}

/// Enumerates the devices a DRA driver backend currently offers, grouped by
/// pool name. The DRA analog of [`crate::DeviceDiscovery`].
#[async_trait]
pub trait ResourcePool: Send + Sync {
    /// Devices this driver currently offers, keyed by pool name.
    async fn devices(&self) -> HashMap<String, Vec<PoolDevice>>;
}

/// Identifies a `ResourceClaim` API object, matching the wire-level
/// `dra.v1.Claim` message fields exactly.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClaimRef {
    pub namespace: String,
    pub uid: String,
    pub name: String,
}

/// One device the scheduler allocated to a claim, as recorded in
/// `ResourceClaim.status.allocation.devices`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocatedDevice {
    pub pool_name: String,
    pub device_name: String,
    /// The claim request this device satisfies. `None` if the device is
    /// associated with every request in the claim.
    pub request_name: Option<String>,
}

/// A claim resolved from a bare [`ClaimRef`] to what the scheduler actually
/// allocated to it, ready for [`ClaimPreparer::prepare`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedClaim {
    pub claim: ClaimRef,
    pub devices: Vec<AllocatedDevice>,
}

/// Artifacts to attach to a container as a result of preparing one device
/// for a claim. The DRA analog of [`crate::ContainerAllocation`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreparedDevice {
    pub request_names: Vec<String>,
    /// Fully qualified CDI device names, e.g. `"vendor.com/gpu=gpudevice1"`.
    pub cdi_device_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrepareError {
    #[error("claim is not allocated: {0:?}")]
    ClaimNotAllocated(ClaimRef),
    #[error("device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("hook failed: {0}")]
    HookFailed(String),
}

/// Prepares and unprepares the devices a scheduler has allocated to
/// `ResourceClaim`s. The DRA analog of [`crate::DeviceAllocator`].
#[async_trait]
pub trait ClaimPreparer: Send + Sync {
    /// Prepares every claim needed by a pod in one batch, matching the
    /// batch shape of the wire-level `NodePrepareResourcesRequest` — do not
    /// call this once per claim.
    ///
    /// Must be idempotent: kubelet may call this again for an
    /// already-prepared claim, e.g. after the driver restarts, and must
    /// get back the same result rather than an error or a
    /// double-provisioned device.
    ///
    /// The returned map must have exactly one entry per claim in `claims`,
    /// keyed by [`ClaimRef`].
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> HashMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>>;

    async fn unprepare(&self, claim: &ClaimRef) -> Result<(), PrepareError>;
}

/// Full framework abstraction a DRA driver backend implements. The DRA
/// analog of [`crate::K8sDevicePlugin`].
pub trait DraDriver: ResourcePool + ClaimPreparer {
    fn driver_name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticDriver {
        name: String,
        pool: String,
        devices: Vec<PoolDevice>,
    }

    #[async_trait]
    impl ResourcePool for StaticDriver {
        async fn devices(&self) -> HashMap<String, Vec<PoolDevice>> {
            HashMap::from([(self.pool.clone(), self.devices.clone())])
        }
    }

    #[async_trait]
    impl ClaimPreparer for StaticDriver {
        async fn prepare(
            &self,
            claims: &[ResolvedClaim],
        ) -> HashMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
            claims
                .iter()
                .map(|resolved| {
                    let prepared = resolved
                        .devices
                        .iter()
                        .map(|device| PreparedDevice {
                            request_names: device.request_name.clone().into_iter().collect(),
                            cdi_device_ids: vec![format!(
                                "example.com/{}={}",
                                self.pool, device.device_name
                            )],
                        })
                        .collect();
                    (resolved.claim.clone(), Ok(prepared))
                })
                .collect()
        }

        async fn unprepare(&self, _claim: &ClaimRef) -> Result<(), PrepareError> {
            Ok(())
        }
    }

    impl DraDriver for StaticDriver {
        fn driver_name(&self) -> &str {
            &self.name
        }
    }

    fn make_driver() -> StaticDriver {
        StaticDriver {
            name: "example.com/widget".to_string(),
            pool: "node-0".to_string(),
            devices: vec![
                PoolDevice::new("widget-0")
                    .attribute("model", AttributeValue::String("x1".to_string())),
            ],
        }
    }

    fn make_claim(name: &str, device_name: &str) -> ResolvedClaim {
        ResolvedClaim {
            claim: ClaimRef {
                namespace: "default".to_string(),
                uid: format!("uid-{name}"),
                name: name.to_string(),
            },
            devices: vec![AllocatedDevice {
                pool_name: "node-0".to_string(),
                device_name: device_name.to_string(),
                request_name: Some("req-0".to_string()),
            }],
        }
    }

    #[tokio::test]
    async fn devices_reports_pool_map() {
        let driver = make_driver();
        let devices = driver.devices().await;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices["node-0"].len(), 1);
        assert_eq!(devices["node-0"][0].name, "widget-0");
    }

    #[tokio::test]
    async fn prepare_returns_per_claim_results_for_a_batch() {
        let driver = make_driver();
        let claims = vec![
            make_claim("claim-a", "widget-0"),
            make_claim("claim-b", "widget-1"),
        ];

        let results = driver.prepare(&claims).await;

        assert_eq!(results.len(), 2);
        let prepared_a = results[&claims[0].claim].as_ref().unwrap();
        assert_eq!(
            prepared_a[0].cdi_device_ids,
            vec!["example.com/node-0=widget-0"]
        );
        let prepared_b = results[&claims[1].claim].as_ref().unwrap();
        assert_eq!(
            prepared_b[0].cdi_device_ids,
            vec!["example.com/node-0=widget-1"]
        );
    }

    #[tokio::test]
    async fn unprepare_unknown_claim_does_not_panic() {
        let driver = make_driver();
        let claim = ClaimRef {
            namespace: "default".to_string(),
            uid: "missing".to_string(),
            name: "missing".to_string(),
        };
        driver.unprepare(&claim).await.expect("no-op unprepare");
    }

    #[test]
    fn driver_name_is_exposed() {
        let driver = make_driver();
        assert_eq!(driver.driver_name(), "example.com/widget");
    }

    #[test]
    fn prepare_error_display() {
        let err = PrepareError::DeviceUnavailable("widget-0".to_string());
        assert_eq!(err.to_string(), "device unavailable: widget-0");
    }
}
