//! Driver-owned `ResourceClaim.status.devices` publication.

use k8s_openapi::apimachinery::pkg::runtime;

use super::*;

const FIELD_MANAGER: &str = "k8s-device-plugin-rs";
const MAX_PATCH_ATTEMPTS: usize = 3;
const MAX_DATA_BYTES: usize = 10 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaimDeviceStatusKey {
    pub pool_name: String,
    pub device_name: String,
    pub share_id: Option<String>,
}

impl fmt::Display for ClaimDeviceStatusKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.pool_name,
            self.device_name,
            self.share_id.as_deref().unwrap_or("-")
        )
    }
}

/// Driver-specific status for one device allocated to a claim.
///
/// The driver name is intentionally not caller-controlled: a
/// [`ClaimDeviceStatusPublisher`] adds its own driver name before sending the
/// status update. `data` is an owned JSON value; it is serialized at publish
/// time into Kubernetes' raw `data` field and therefore must not exceed 10 KiB.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimDeviceStatus {
    pub pool_name: String,
    pub device_name: String,
    pub share_id: Option<String>,
    pub conditions: Option<Vec<metav1::Condition>>,
    pub data: Option<json::Value>,
    pub network_data: Option<resourcev1::NetworkDeviceData>,
}

impl ClaimDeviceStatus {
    #[must_use]
    pub fn new(pool_name: impl Into<String>, device_name: impl Into<String>) -> Self {
        Self {
            pool_name: pool_name.into(),
            device_name: device_name.into(),
            share_id: None,
            conditions: None,
            data: None,
            network_data: None,
        }
    }

    fn key(&self) -> ClaimDeviceStatusKey {
        ClaimDeviceStatusKey {
            pool_name: self.pool_name.clone(),
            device_name: self.device_name.clone(),
            share_id: self.share_id.clone(),
        }
    }

    fn into_kubernetes(
        self,
        driver_name: &str,
    ) -> Result<resourcev1::AllocatedDeviceStatus, ClaimDeviceStatusError> {
        let key = self.key();
        if self
            .conditions
            .as_ref()
            .is_some_and(|conditions| conditions.len() > 8)
        {
            return Err(ClaimDeviceStatusError::TooManyConditions(key));
        }

        let data = self
            .data
            .map(|data| {
                let raw = json::to_vec(&data).map_err(ClaimDeviceStatusError::SerializeData)?;
                if raw.len() > MAX_DATA_BYTES {
                    return Err(ClaimDeviceStatusError::DataTooLarge {
                        device: key.clone(),
                        bytes: raw.len(),
                    });
                }
                Ok(runtime::RawExtension(data))
            })
            .transpose()?;

        Ok(resourcev1::AllocatedDeviceStatus {
            conditions: self.conditions,
            data,
            device: self.device_name,
            driver: driver_name.to_string(),
            network_data: self.network_data,
            pool: self.pool_name,
            share_id: self.share_id,
        })
    }
}

/// Whether publishing changed the `ResourceClaim`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimDeviceStatusPublishOutcome {
    Updated,
    Unchanged,
}

/// A safe failure while validating or publishing a device-status report.
#[derive(Debug, thiserror::Error)]
pub enum ClaimDeviceStatusError {
    #[error("ResourceClaim {namespace}/{name} has no resourceVersion for a conditional update")]
    MissingResourceVersion { namespace: String, name: String },
    #[error(
        "ResourceClaim {namespace}/{name} uid mismatch: expected {expected_uid}, found {found_uid}"
    )]
    ClaimIdentityMismatch {
        namespace: String,
        name: String,
        expected_uid: String,
        found_uid: String,
    },
    #[error("device status {0} was reported more than once")]
    DuplicateDeviceStatus(ClaimDeviceStatusKey),
    #[error("device status {0} has more than eight conditions")]
    TooManyConditions(ClaimDeviceStatusKey),
    #[error(
        "device status {device} has {bytes} bytes of raw data; Kubernetes permits at most {MAX_DATA_BYTES}"
    )]
    DataTooLarge {
        device: ClaimDeviceStatusKey,
        bytes: usize,
    },
    #[error("failed to serialize device status data: {0}")]
    SerializeData(json::Error),
    #[error("ResourceClaim {namespace}/{name} has no allocation for driver {driver_name}")]
    DriverNotAllocated {
        namespace: String,
        name: String,
        driver_name: String,
    },
    #[error(
        "device status {device} is not allocated to driver {driver_name} in ResourceClaim {namespace}/{name}"
    )]
    DeviceNotAllocated {
        namespace: String,
        name: String,
        driver_name: String,
        device: Box<ClaimDeviceStatusKey>,
    },
    #[error("Kubernetes API error while publishing ResourceClaim device status: {0}")]
    Kubernetes(#[source] kube::Error),
}

/// Publishes the local driver's status for devices already allocated to a
/// `ResourceClaim`.
///
/// This type is independent of inventory publication and resource-health
/// streaming. A backend may keep it in its preparation path or in a separate
/// monitor and call [`Self::publish`] whenever its durable device information
/// changes. Publication is an upsert: entries omitted from one call are left
/// untouched. The publisher verifies the claim UID and every allocation key
/// before merging a status update. A resource-version precondition prevents
/// concurrent updates from being lost, including other drivers' entries.
/// Conflicts are reread and revalidated, with at most three patch attempts.
#[derive(Clone)]
pub struct ClaimDeviceStatusPublisher {
    client: kube::Client,
    driver_name: String,
    field_manager: String,
}

impl fmt::Debug for ClaimDeviceStatusPublisher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimDeviceStatusPublisher")
            .field("driver_name", &self.driver_name)
            .finish_non_exhaustive()
    }
}

impl ClaimDeviceStatusPublisher {
    #[must_use]
    pub fn new(client: kube::Client, driver_name: impl Into<String>) -> Self {
        Self {
            client,
            driver_name: driver_name.into(),
            field_manager: FIELD_MANAGER.to_string(),
        }
    }

    /// Changes the field manager recorded for status updates.
    ///
    /// This is attribution, not server-side apply ownership: publication uses
    /// a conditional merge patch and preserves entries omitted from the call.
    #[must_use]
    pub fn with_field_manager(mut self, field_manager: impl Into<String>) -> Self {
        self.field_manager = field_manager.into();
        self
    }

    /// Validates and applies the supplied status entries for `claim`.
    ///
    /// `Ok(Unchanged)` means that every supplied entry already exactly matches
    /// the API object, avoiding an unnecessary API write. API authorization
    /// failures are returned as [`ClaimDeviceStatusError::Kubernetes`] and are
    /// intentionally not retried: the caller must correct its RBAC or token.
    pub async fn publish(
        &self,
        claim: &ClaimRef,
        statuses: impl IntoIterator<Item = ClaimDeviceStatus>,
    ) -> Result<ClaimDeviceStatusPublishOutcome, ClaimDeviceStatusError> {
        self.publish_inner(claim, statuses, false).await
    }

    /// Updates only fields explicitly supplied as `Some` in each status.
    ///
    /// Omitted conditions/data/network data retain their current values. Use
    /// this when a pre-binding controller owns conditions and a node preparer
    /// owns data. An explicit empty condition list clears the conditions.
    /// Updates retain the same UID, allocation and resourceVersion checks as
    /// [`Self::publish`], including remerging current fields after a conflict.
    pub async fn publish_fields(
        &self,
        claim: &ClaimRef,
        statuses: impl IntoIterator<Item = ClaimDeviceStatus>,
    ) -> Result<ClaimDeviceStatusPublishOutcome, ClaimDeviceStatusError> {
        self.publish_inner(claim, statuses, true).await
    }

    async fn publish_inner(
        &self,
        claim: &ClaimRef,
        statuses: impl IntoIterator<Item = ClaimDeviceStatus>,
        preserve_unspecified_fields: bool,
    ) -> Result<ClaimDeviceStatusPublishOutcome, ClaimDeviceStatusError> {
        let mut desired = self.desired_statuses(statuses)?;
        if preserve_unspecified_fields {
            desired.retain(|_, status| {
                status.conditions.is_some()
                    || status.data.is_some()
                    || status.network_data.is_some()
            });
        }
        if desired.is_empty() {
            return Ok(ClaimDeviceStatusPublishOutcome::Unchanged);
        }

        let api = self.client.resource_claims(claim);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let resource_claim = api
                .get(&claim.name)
                .await
                .map_err(ClaimDeviceStatusError::Kubernetes)?;
            let found_uid = resource_claim.metadata.uid.as_deref().unwrap_or_default();
            if found_uid != claim.uid {
                return Err(ClaimDeviceStatusError::ClaimIdentityMismatch {
                    namespace: claim.namespace.clone(),
                    name: claim.name.clone(),
                    expected_uid: claim.uid.clone(),
                    found_uid: found_uid.to_string(),
                });
            }

            self.validate_allocations(claim, &resource_claim, &desired)?;
            let desired = if preserve_unspecified_fields {
                std::borrow::Cow::Owned(self.merge_existing_fields(&resource_claim, &desired))
            } else {
                std::borrow::Cow::Borrowed(&desired)
            };
            if self.statuses_are_current(&resource_claim, &desired) {
                return Ok(ClaimDeviceStatusPublishOutcome::Unchanged);
            }

            let resource_version = resource_claim
                .metadata
                .resource_version
                .as_deref()
                .filter(|version| !version.is_empty())
                .ok_or_else(|| ClaimDeviceStatusError::MissingResourceVersion {
                    namespace: claim.namespace.clone(),
                    name: claim.name.clone(),
                })?;
            let mut devices = resource_claim
                .status
                .as_ref()
                .and_then(|status| status.devices.clone())
                .unwrap_or_default();
            for desired_status in desired.values() {
                if let Some(current) = devices.iter_mut().find(|current| {
                    current.driver == desired_status.driver
                        && current.pool == desired_status.pool
                        && current.device == desired_status.device
                        && current.share_id == desired_status.share_id
                }) {
                    current.clone_from(desired_status);
                } else {
                    devices.push(desired_status.clone());
                }
            }

            // SSA omits previously owned entries when given only a partial upsert.
            // Use an optimistic-concurrency merge patch instead; the version check
            // protects the complete devices array from concurrent writers.
            // https://kubernetes.io/docs/reference/using-api/api-concepts/#updates-to-existing-resources
            let patch = json::json!({
                "apiVersion": "resource.k8s.io/v1",
                "kind": "ResourceClaim",
                "metadata": {
                    "name": claim.name,
                    "uid": claim.uid,
                    "resourceVersion": resource_version,
                },
                "status": {
                    "devices": devices,
                },
            });
            let patch_params = api::PatchParams::apply(&self.field_manager);
            match api
                .patch_status(&claim.name, &patch_params, &api::Patch::Merge(&patch))
                .await
            {
                Ok(_) => return Ok(ClaimDeviceStatusPublishOutcome::Updated),
                Err(kube::Error::Api(error))
                    if error.code == 409 && attempt < MAX_PATCH_ATTEMPTS => {}
                Err(error) => return Err(ClaimDeviceStatusError::Kubernetes(error)),
            }
        }
    }

    fn merge_existing_fields(
        &self,
        resource_claim: &resourcev1::ResourceClaim,
        desired: &BTreeMap<ClaimDeviceStatusKey, resourcev1::AllocatedDeviceStatus>,
    ) -> BTreeMap<ClaimDeviceStatusKey, resourcev1::AllocatedDeviceStatus> {
        let mut merged = desired.clone();
        for status in resource_claim
            .status
            .iter()
            .flat_map(|status| status.devices.iter().flatten())
        {
            if status.driver != self.driver_name {
                continue;
            }
            let key = ClaimDeviceStatusKey {
                pool_name: status.pool.clone(),
                device_name: status.device.clone(),
                share_id: status.share_id.clone(),
            };
            if let Some(update) = merged.get_mut(&key) {
                if update.conditions.is_none() {
                    update.conditions.clone_from(&status.conditions);
                }
                if update.data.is_none() {
                    update.data.clone_from(&status.data);
                }
                if update.network_data.is_none() {
                    update.network_data.clone_from(&status.network_data);
                }
            }
        }
        merged
    }

    fn desired_statuses(
        &self,
        statuses: impl IntoIterator<Item = ClaimDeviceStatus>,
    ) -> Result<
        BTreeMap<ClaimDeviceStatusKey, resourcev1::AllocatedDeviceStatus>,
        ClaimDeviceStatusError,
    > {
        let mut desired = BTreeMap::new();
        for status in statuses {
            let key = status.key();
            let kubernetes_status = status.into_kubernetes(&self.driver_name)?;
            if desired.insert(key.clone(), kubernetes_status).is_some() {
                return Err(ClaimDeviceStatusError::DuplicateDeviceStatus(key));
            }
        }
        Ok(desired)
    }

    fn validate_allocations(
        &self,
        claim_ref: &ClaimRef,
        resource_claim: &resourcev1::ResourceClaim,
        desired: &BTreeMap<ClaimDeviceStatusKey, resourcev1::AllocatedDeviceStatus>,
    ) -> Result<(), ClaimDeviceStatusError> {
        let allocated = resource_claim
            .status
            .as_ref()
            .and_then(|status| status.allocation.as_ref())
            .and_then(|allocation| allocation.devices.as_ref())
            .and_then(|devices| devices.results.as_ref())
            .into_iter()
            .flatten()
            .filter(|device| device.driver == self.driver_name)
            .map(|device| ClaimDeviceStatusKey {
                pool_name: device.pool.clone(),
                device_name: device.device.clone(),
                share_id: device.share_id.clone(),
            })
            .collect::<BTreeSet<_>>();

        if allocated.is_empty() {
            return Err(ClaimDeviceStatusError::DriverNotAllocated {
                namespace: claim_ref.namespace.clone(),
                name: claim_ref.name.clone(),
                driver_name: self.driver_name.clone(),
            });
        }

        for key in desired.keys() {
            if !allocated.contains(key) {
                return Err(ClaimDeviceStatusError::DeviceNotAllocated {
                    namespace: claim_ref.namespace.clone(),
                    name: claim_ref.name.clone(),
                    driver_name: self.driver_name.clone(),
                    device: Box::new(key.clone()),
                });
            }
        }
        Ok(())
    }

    fn statuses_are_current(
        &self,
        resource_claim: &resourcev1::ResourceClaim,
        desired: &BTreeMap<ClaimDeviceStatusKey, resourcev1::AllocatedDeviceStatus>,
    ) -> bool {
        let current = resource_claim
            .status
            .as_ref()
            .and_then(|status| status.devices.as_ref())
            .into_iter()
            .flatten()
            .filter(|status| status.driver == self.driver_name)
            .map(|status| {
                (
                    ClaimDeviceStatusKey {
                        pool_name: status.pool.clone(),
                        device_name: status.device.clone(),
                        share_id: status.share_id.clone(),
                    },
                    status,
                )
            })
            .collect::<BTreeMap<_, _>>();

        desired.iter().all(|(key, desired_status)| {
            current.get(key).is_some_and(|current_status| {
                current_status.driver == desired_status.driver
                    && current_status.pool == desired_status.pool
                    && current_status.device == desired_status.device
                    && current_status.share_id == desired_status.share_id
                    && current_status.data == desired_status.data
                    && current_status.network_data == desired_status.network_data
                    && current_status.conditions.as_deref().unwrap_or_default()
                        == desired_status.conditions.as_deref().unwrap_or_default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use kube::client;

    use k8s_device_plugin_test::kube_mock::allocated_claim_status;
    use k8s_device_plugin_test::kube_mock::mock_kube_client;
    use k8s_device_plugin_test::kube_mock::resource_claim;

    use super::*;

    const DRIVER: &str = "dra.example.com";

    fn claim_ref() -> ClaimRef {
        ClaimRef {
            namespace: "default".to_string(),
            uid: "claim-uid".to_string(),
            name: "claim".to_string(),
        }
    }

    fn allocated_claim(status_devices: json::Value) -> json::Value {
        // The complete object is typed; these tests deliberately vary the raw
        // devices field to exercise null/default and merge-patch wire semantics.
        let mut claim = resource_claim(
            "claim",
            "default",
            "claim-uid",
            Some(allocated_claim_status(
                DRIVER,
                "widget-pool",
                "widget-0",
                "widget",
            )),
        );
        claim.metadata.resource_version = Some("1".into());
        let mut claim = json::to_value(claim).unwrap();
        claim["status"]["devices"] = status_devices;
        claim
    }

    fn status() -> ClaimDeviceStatus {
        ClaimDeviceStatus {
            pool_name: "widget-pool".to_string(),
            device_name: "widget-0".to_string(),
            share_id: None,
            conditions: None,
            data: Some(json::json!({"phase": "prepared"})),
            network_data: None,
        }
    }

    fn response(status: u16, body: impl serde::Serialize) -> http::Response<client::Body> {
        http::Response::builder()
            .status(status)
            .body(client::Body::from(
                json::to_vec(&body).expect("serialize mock body"),
            ))
            .expect("build mock response")
    }

    #[tokio::test]
    async fn publish_applies_only_the_local_drivers_allocated_device_status() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let expected_claim = allocated_claim(json::Value::Null);
        let responder = tokio::spawn(async move {
            let (get, send) = handle.next_request().await.expect("claim get request");
            assert_eq!(get.method(), http::Method::GET);
            assert_eq!(
                get.uri().path(),
                "/apis/resource.k8s.io/v1/namespaces/default/resourceclaims/claim"
            );
            send.send_response(response(200, expected_claim.clone()));

            let (patch, send) = handle
                .next_request()
                .await
                .expect("claim status patch request");
            assert_eq!(patch.method(), http::Method::PATCH);
            assert_eq!(
                patch.uri().path(),
                "/apis/resource.k8s.io/v1/namespaces/default/resourceclaims/claim/status"
            );
            assert!(
                patch
                    .headers()
                    .get(http::header::CONTENT_TYPE)
                    .expect("patch content type")
                    .to_str()
                    .expect("content type text")
                    .starts_with("application/merge-patch+json")
            );
            let body = patch
                .into_body()
                .collect()
                .await
                .expect("collect patch body")
                .to_bytes();
            let patch = json::from_slice::<json::Value>(&body).expect("decode patch");
            // A merge patch must omit unrelated fields, rather than serialize
            // a full ResourceClaim with defaults or null values.
            assert!(patch.get("spec").is_none());
            assert!(patch["status"].get("allocation").is_none());
            assert!(patch["status"].get("reservedFor").is_none());
            assert_eq!(patch["metadata"]["uid"], "claim-uid");
            assert_eq!(patch["metadata"]["resourceVersion"], "1");
            assert_eq!(
                patch["status"]["devices"]
                    .as_array()
                    .expect("status devices")
                    .len(),
                1
            );
            assert_eq!(patch["status"]["devices"][0]["driver"], DRIVER);
            assert_eq!(patch["status"]["devices"][0]["pool"], "widget-pool");
            assert_eq!(patch["status"]["devices"][0]["device"], "widget-0");
            assert_eq!(patch["status"]["devices"][0]["data"]["phase"], "prepared");
            send.send_response(response(200, expected_claim));
        });

        assert_eq!(
            publisher
                .publish(&claim_ref(), [status()])
                .await
                .expect("publish status"),
            ClaimDeviceStatusPublishOutcome::Updated
        );
        responder.await.expect("mock responder completes");
    }

    #[tokio::test]
    async fn publish_is_idempotent_when_the_device_status_is_current() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let responder = tokio::spawn(async move {
            let (_get, send) = handle.next_request().await.expect("claim get request");
            send.send_response(response(
                200,
                allocated_claim(json::json!([{
                    "driver": DRIVER,
                    "pool": "widget-pool",
                    "device": "widget-0",
                    "data": {"phase": "prepared"}
                }])),
            ));
        });

        assert_eq!(
            publisher
                .publish(&claim_ref(), [status()])
                .await
                .expect("publish status"),
            ClaimDeviceStatusPublishOutcome::Unchanged
        );
        responder.await.expect("mock responder completes");
    }

    #[tokio::test]
    async fn publish_rejects_an_unallocated_or_foreign_device_before_patching() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let responder = tokio::spawn(async move {
            let (_get, send) = handle.next_request().await.expect("claim get request");
            send.send_response(response(200, allocated_claim(json::Value::Null)));
        });
        let foreign = ClaimDeviceStatus::new("widget-pool", "widget-1");

        assert!(matches!(
            publisher.publish(&claim_ref(), [foreign]).await,
            Err(ClaimDeviceStatusError::DeviceNotAllocated { .. })
        ));
        responder.await.expect("mock responder completes");
    }

    #[tokio::test]
    async fn publish_surfaces_authorization_failures() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let expected_claim = allocated_claim(json::Value::Null);
        let responder = tokio::spawn(async move {
            let (_get, send) = handle.next_request().await.expect("claim get request");
            send.send_response(response(200, expected_claim));
            let (_patch, send) = handle
                .next_request()
                .await
                .expect("claim status patch request");
            send.send_response(response(
                403,
                kube::core::Status::failure("test failure", "Forbidden").with_code(403),
            ));
        });

        let error = publisher
            .publish(&claim_ref(), [status()])
            .await
            .expect_err("forbidden patch must fail");
        assert!(
            matches!(error, ClaimDeviceStatusError::Kubernetes(kube::Error::Api(error)) if error.code == 403)
        );
        responder.await.expect("mock responder completes");
    }

    #[tokio::test]
    async fn publish_rejects_duplicate_status_keys_without_an_api_request() {
        let (client, _handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let duplicate = publisher.desired_statuses([status(), status()]);

        assert!(matches!(
            duplicate,
            Err(ClaimDeviceStatusError::DuplicateDeviceStatus(_))
        ));
    }

    async fn read_patch(request: http::Request<client::Body>) -> json::Value {
        assert_eq!(request.method(), http::Method::PATCH);
        assert_eq!(
            request.headers()[http::header::CONTENT_TYPE],
            "application/merge-patch+json"
        );
        let bytes = request
            .into_body()
            .collect()
            .await
            .expect("read patch")
            .to_bytes();
        json::from_slice(&bytes).expect("decode patch")
    }

    fn conflict_response() -> http::Response<client::Body> {
        response(
            409,
            kube::core::Status::failure("test failure", "Conflict").with_code(409),
        )
    }

    #[tokio::test]
    async fn successive_upserts_preserve_omitted_foreign_and_shared_entries() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let responder = tokio::spawn(async move {
            let preserved = json::json!([
                {"driver": DRIVER, "pool": "widget-pool", "device": "widget-1", "data": {"keep": true}},
                {"driver": "other.example.com", "pool": "widget-pool", "device": "widget-0", "data": {"foreign": true}},
                {"driver": DRIVER, "pool": "widget-pool", "device": "widget-0", "shareID": "00000000-0000-0000-0000-000000000001", "data": {"shared": true}}
            ]);
            let mut current = allocated_claim(preserved.clone());
            // Model the array-replacement semantics of JSON merge patch, not SSA.
            for version in 1..=2 {
                let (_get, send) = handle.next_request().await.expect("claim read");
                send.send_response(response(200, current.clone()));
                let (patch, send) = handle.next_request().await.expect("status patch");
                let patch = read_patch(patch).await;
                assert_eq!(patch["metadata"]["resourceVersion"], version.to_string());
                let devices = patch["status"]["devices"].as_array().expect("devices");
                assert_eq!(
                    &devices[..3],
                    preserved.as_array().expect("preserved entries")
                );
                assert_eq!(devices.len(), 4);
                assert_eq!(
                    devices[3]["data"]["phase"],
                    if version == 1 { "prepared" } else { "ready" }
                );
                current["status"]["devices"] = patch["status"]["devices"].clone();
                current["metadata"]["resourceVersion"] = json::json!((version + 1).to_string());
                send.send_response(response(200, current.clone()));
            }
        });
        publisher
            .publish(&claim_ref(), [status()])
            .await
            .expect("first upsert");
        let mut updated = status();
        updated.data = Some(json::json!({"phase": "ready"}));
        publisher
            .publish(&claim_ref(), [updated])
            .await
            .expect("second upsert");
        responder.await.expect("responder finishes");
    }

    #[tokio::test]
    async fn conflict_rereads_and_preserves_concurrent_foreign_update() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let responder = tokio::spawn(async move {
            let (_get, send) = handle.next_request().await.expect("initial read");
            send.send_response(response(200, allocated_claim(json::Value::Null)));
            let (patch, send) = handle.next_request().await.expect("initial patch");
            assert_eq!(read_patch(patch).await["metadata"]["resourceVersion"], "1");
            send.send_response(conflict_response());
            let foreign = json::json!({"driver": "other.example.com", "pool": "p", "device": "d", "data": {"new": true}});
            let mut current = allocated_claim(json::json!([foreign.clone()]));
            current["metadata"]["resourceVersion"] = json::json!("2");
            let (_get, send) = handle.next_request().await.expect("reread after conflict");
            send.send_response(response(200, current.clone()));
            let (patch, send) = handle.next_request().await.expect("retry patch");
            let patch = read_patch(patch).await;
            assert_eq!(patch["metadata"]["resourceVersion"], "2");
            assert_eq!(patch["status"]["devices"][0], foreign);
            assert_eq!(
                patch["status"]["devices"]
                    .as_array()
                    .expect("devices")
                    .len(),
                2
            );
            send.send_response(response(200, current));
        });
        publisher
            .publish(&claim_ref(), [status()])
            .await
            .expect("conflict retry succeeds");
        responder.await.expect("responder finishes");
    }

    #[tokio::test]
    async fn conflict_revalidates_uid_and_allocation_before_retry() {
        for replaced in [false, true] {
            let (client, mut handle) = mock_kube_client();
            let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
            let responder = tokio::spawn(async move {
                let (_get, send) = handle.next_request().await.expect("initial read");
                send.send_response(response(200, allocated_claim(json::Value::Null)));
                let (_patch, send) = handle.next_request().await.expect("initial patch");
                send.send_response(conflict_response());
                let mut current = allocated_claim(json::Value::Null);
                if replaced {
                    current["metadata"]["uid"] = json::json!("replacement-uid");
                } else {
                    current["status"]["allocation"] = json::Value::Null;
                }
                let (_get, send) = handle.next_request().await.expect("reread");
                send.send_response(response(200, current));
                handle
            });
            let error = publisher
                .publish(&claim_ref(), [status()])
                .await
                .expect_err("reject stale allocation");
            if replaced {
                assert!(matches!(
                    error,
                    ClaimDeviceStatusError::ClaimIdentityMismatch { .. }
                ));
            } else {
                assert!(matches!(
                    error,
                    ClaimDeviceStatusError::DriverNotAllocated { .. }
                ));
            }
            let mut handle = responder.await.expect("responder finishes");
            assert!(
                tokio::time::timeout(Duration::from_millis(20), handle.next_request())
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn conflict_retries_are_bounded() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let responder = tokio::spawn(async move {
            for _ in 0..MAX_PATCH_ATTEMPTS {
                let (_get, send) = handle.next_request().await.expect("claim read");
                send.send_response(response(200, allocated_claim(json::Value::Null)));
                let (_patch, send) = handle.next_request().await.expect("patch attempt");
                send.send_response(conflict_response());
            }
        });
        assert!(matches!(publisher.publish(&claim_ref(), [status()]).await,
            Err(ClaimDeviceStatusError::Kubernetes(kube::Error::Api(error))) if error.code == 409));
        responder.await.expect("responder finishes");
    }

    #[tokio::test]
    async fn missing_resource_version_refuses_unconditional_patch() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let responder = tokio::spawn(async move {
            let mut current = allocated_claim(json::Value::Null);
            current["metadata"]["resourceVersion"] = json::Value::Null;
            let (_get, send) = handle.next_request().await.expect("claim read");
            send.send_response(response(200, current));
        });
        assert!(matches!(
            publisher.publish(&claim_ref(), [status()]).await,
            Err(ClaimDeviceStatusError::MissingResourceVersion { .. })
        ));
        responder.await.expect("responder finishes");
    }

    #[tokio::test]
    async fn status_data_equality_ignores_json_whitespace_and_key_order() {
        let (client, _handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let mut desired = status();
        desired.data = Some(json::from_str(r#"{ "b": 2, "a": 1 }"#).expect("valid JSON"));
        let current = json::from_value(allocated_claim(json::json!([{
            "driver": DRIVER, "pool": "widget-pool", "device": "widget-0", "data": {"a": 1, "b": 2}
        }])))
        .expect("valid claim");
        assert!(publisher.statuses_are_current(
            &current,
            &publisher.desired_statuses([desired]).expect("valid status")
        ));
    }

    fn ready_device_status() -> json::Value {
        let mut status = ClaimDeviceStatus::new("widget-pool", "widget-0");
        status.conditions = Some(vec![metav1::Condition {
            type_: "dra.example.com/Attached".into(),
            status: "True".into(),
            reason: "Attached".into(),
            message: "ready".into(),
            last_transition_time: metav1::Time("2026-10-23T10:00:00Z".parse().unwrap()),
            observed_generation: None,
        }]);
        status.data = Some(json::json!({"phase": "old"}));
        status.network_data = Some(resourcev1::NetworkDeviceData {
            interface_name: Some("eth-test".into()),
            ..default()
        });
        // These tests mutate null/omitted fields and compare serialized merge
        // patches, so convert the valid typed fixture at the wire boundary.
        json::to_value(status.into_kubernetes(DRIVER).unwrap()).unwrap()
    }

    async fn patch_json(request: http::Request<client::Body>) -> json::Value {
        let bytes = request.into_body().collect().await.unwrap().to_bytes();
        json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn partial_data_update_retains_controller_conditions_network_and_other_drivers() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let current = ready_device_status();
        let foreign =
            json::json!({"driver":"other.example.com", "pool":"other", "device":"foreign"});
        let script = async {
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(response(
                200,
                allocated_claim(json::json!([current.clone(), foreign.clone()])),
            ));
            let (request, send) = handle.next_request().await.unwrap();
            let body = patch_json(request).await;
            assert_eq!(body["metadata"]["resourceVersion"], "1");
            assert_eq!(
                body["status"]["devices"][0]["conditions"],
                current["conditions"]
            );
            assert_eq!(
                body["status"]["devices"][0]["networkData"],
                current["networkData"]
            );
            assert_eq!(body["status"]["devices"][0]["data"]["phase"], "prepared");
            assert_eq!(body["status"]["devices"][1], foreign);
            send.send_response(response(
                200,
                allocated_claim(body["status"]["devices"].clone()),
            ));
        };
        let publication = async { publisher.publish_fields(&claim_ref(), [status()]).await };
        let (_, result) = tokio::join!(script, publication);
        assert_eq!(result.unwrap(), ClaimDeviceStatusPublishOutcome::Updated);
    }

    #[tokio::test]
    async fn complete_publication_still_replaces_unspecified_fields() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let script = async {
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(response(
                200,
                allocated_claim(json::json!([ready_device_status()])),
            ));
            let (request, send) = handle.next_request().await.unwrap();
            let body = patch_json(request).await;
            assert!(body["status"]["devices"][0].get("conditions").is_none());
            assert!(body["status"]["devices"][0].get("networkData").is_none());
            send.send_response(response(
                200,
                allocated_claim(body["status"]["devices"].clone()),
            ));
        };
        let publication = async { publisher.publish(&claim_ref(), [status()]).await };
        let (_, result) = tokio::join!(script, publication);
        assert_eq!(result.unwrap(), ClaimDeviceStatusPublishOutcome::Updated);
    }

    #[tokio::test]
    async fn partial_condition_update_remerges_data_after_a_conflict() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let mut desired = ClaimDeviceStatus::new("widget-pool", "widget-0");
        desired.conditions =
            Some(json::from_value(ready_device_status()["conditions"].clone()).unwrap());
        let script = async {
            for version in ["1", "2"] {
                let mut current = ready_device_status();
                current["conditions"] = json::Value::Null;
                current["data"]["phase"] = json::json!(format!("concurrent-{version}"));
                let mut claim = allocated_claim(json::json!([current]));
                claim["metadata"]["resourceVersion"] = json::json!(version);
                let (_, send) = handle.next_request().await.unwrap();
                send.send_response(response(200, claim.clone()));
                let (request, send) = handle.next_request().await.unwrap();
                let body = patch_json(request).await;
                assert_eq!(body["metadata"]["resourceVersion"], version);
                assert_eq!(
                    body["status"]["devices"][0]["data"]["phase"],
                    format!("concurrent-{version}")
                );
                assert_eq!(
                    body["status"]["devices"][0]["conditions"][0]["status"],
                    "True"
                );
                if version == "1" {
                    send.send_response(response(
                        409,
                        kube::core::Status::failure("raced", "Conflict").with_code(409),
                    ));
                } else {
                    send.send_response(response(200, claim));
                }
            }
        };
        let publication = async { publisher.publish_fields(&claim_ref(), [desired]).await };
        let (_, result) = tokio::join!(script, publication);
        assert_eq!(result.unwrap(), ClaimDeviceStatusPublishOutcome::Updated);
    }

    #[tokio::test]
    async fn empty_partial_conditions_clear_once_and_then_converge() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let mut desired = ClaimDeviceStatus::new("widget-pool", "widget-0");
        desired.conditions = Some(vec![]);
        let script = async {
            let mut current = ready_device_status();
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(response(
                200,
                allocated_claim(json::json!([current.clone()])),
            ));
            let (request, send) = handle.next_request().await.unwrap();
            let body = patch_json(request).await;
            assert_eq!(body["status"]["devices"][0]["conditions"], json::json!([]));
            assert_eq!(body["status"]["devices"][0]["data"], current["data"]);
            current.as_object_mut().unwrap().remove("conditions");
            send.send_response(response(
                200,
                allocated_claim(json::json!([current.clone()])),
            ));
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(response(200, allocated_claim(json::json!([current]))));
            assert!(
                tokio::time::timeout(Duration::from_millis(50), handle.next_request())
                    .await
                    .is_err()
            );
        };
        let publications = async {
            assert_eq!(
                publisher
                    .publish_fields(&claim_ref(), [desired.clone()])
                    .await
                    .unwrap(),
                ClaimDeviceStatusPublishOutcome::Updated
            );
            publisher.publish_fields(&claim_ref(), [desired]).await
        };
        let (_, result) = tokio::join!(script, publications);
        assert_eq!(result.unwrap(), ClaimDeviceStatusPublishOutcome::Unchanged);
    }

    #[tokio::test]
    async fn partial_publication_rechecks_claim_identity_before_retrying() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let script = async {
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(response(
                200,
                allocated_claim(json::json!([ready_device_status()])),
            ));
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(response(
                409,
                kube::core::Status::failure("raced", "Conflict").with_code(409),
            ));
            let mut replaced = allocated_claim(json::json!([ready_device_status()]));
            replaced["metadata"]["uid"] = json::json!("replacement-uid");
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(response(200, replaced));
            assert!(
                tokio::time::timeout(Duration::from_millis(50), handle.next_request())
                    .await
                    .is_err()
            );
        };
        let publication = async { publisher.publish_fields(&claim_ref(), [status()]).await };
        let (_, result) = tokio::join!(script, publication);
        assert!(matches!(
            result,
            Err(ClaimDeviceStatusError::ClaimIdentityMismatch { .. })
        ));
    }
}
