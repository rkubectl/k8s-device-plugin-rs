//! Driver-owned `ResourceClaim.status.devices` publication.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;

use k8s_device_plugin_core::ClaimRef;
use k8s_openapi::api::resource::v1::AllocatedDeviceStatus;
use k8s_openapi::api::resource::v1::NetworkDeviceData;
use k8s_openapi::api::resource::v1::ResourceClaim;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use k8s_openapi::apimachinery::pkg::runtime::RawExtension;
use kube::Api;
use kube::Client;
use kube::api::Patch;
use kube::api::PatchParams;
use serde_json::Value;

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
    pub conditions: Option<Vec<Condition>>,
    pub data: Option<Value>,
    pub network_data: Option<NetworkDeviceData>,
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
    ) -> Result<AllocatedDeviceStatus, ClaimDeviceStatusError> {
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
                let raw =
                    serde_json::to_vec(&data).map_err(ClaimDeviceStatusError::SerializeData)?;
                if raw.len() > MAX_DATA_BYTES {
                    return Err(ClaimDeviceStatusError::DataTooLarge {
                        device: key.clone(),
                        bytes: raw.len(),
                    });
                }
                Ok(RawExtension(data))
            })
            .transpose()?;

        Ok(AllocatedDeviceStatus {
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
    SerializeData(serde_json::Error),
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
    client: Client,
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
    pub fn new(client: Client, driver_name: impl Into<String>) -> Self {
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
        let desired = self.desired_statuses(statuses)?;
        if desired.is_empty() {
            return Ok(ClaimDeviceStatusPublishOutcome::Unchanged);
        }

        let api: Api<ResourceClaim> = Api::namespaced(self.client.clone(), &claim.namespace);
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
            let patch = serde_json::json!({
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
            let patch_params = PatchParams {
                field_manager: Some(self.field_manager.clone()),
                ..PatchParams::default()
            };
            match api
                .patch_status(&claim.name, &patch_params, &Patch::Merge(&patch))
                .await
            {
                Ok(_) => return Ok(ClaimDeviceStatusPublishOutcome::Updated),
                Err(kube::Error::Api(error))
                    if error.code == 409 && attempt < MAX_PATCH_ATTEMPTS => {}
                Err(error) => return Err(ClaimDeviceStatusError::Kubernetes(error)),
            }
        }
    }

    fn desired_statuses(
        &self,
        statuses: impl IntoIterator<Item = ClaimDeviceStatus>,
    ) -> Result<BTreeMap<ClaimDeviceStatusKey, AllocatedDeviceStatus>, ClaimDeviceStatusError> {
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
        resource_claim: &ResourceClaim,
        desired: &BTreeMap<ClaimDeviceStatusKey, AllocatedDeviceStatus>,
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
        resource_claim: &ResourceClaim,
        desired: &BTreeMap<ClaimDeviceStatusKey, AllocatedDeviceStatus>,
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
            current
                .get(key)
                .is_some_and(|current_status| *current_status == desired_status)
        })
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use k8s_device_plugin_test::kube_mock::mock_kube_client;
    use k8s_device_plugin_test::kube_mock::resource_claim_json;
    use kube::client::Body;
    use serde_json::json;

    use super::*;

    const DRIVER: &str = "dra.example.com";

    fn claim_ref() -> ClaimRef {
        ClaimRef {
            namespace: "default".to_string(),
            uid: "claim-uid".to_string(),
            name: "claim".to_string(),
        }
    }

    fn allocated_claim(status_devices: Value) -> Value {
        let mut claim = resource_claim_json(
            "claim",
            "default",
            "claim-uid",
            json!({
                "allocation": {
                    "devices": {
                        "results": [
                            {
                                "request": "widget",
                                "driver": DRIVER,
                                "pool": "widget-pool",
                                "device": "widget-0"
                            }
                        ]
                    }
                },
                "devices": status_devices,
            }),
        );
        claim["metadata"]["resourceVersion"] = json!("1");
        claim
    }

    fn status() -> ClaimDeviceStatus {
        ClaimDeviceStatus {
            pool_name: "widget-pool".to_string(),
            device_name: "widget-0".to_string(),
            share_id: None,
            conditions: None,
            data: Some(json!({"phase": "prepared"})),
            network_data: None,
        }
    }

    fn response(status: u16, body: Value) -> http::Response<Body> {
        http::Response::builder()
            .status(status)
            .body(Body::from(
                serde_json::to_vec(&body).expect("serialize mock body"),
            ))
            .expect("build mock response")
    }

    #[tokio::test]
    async fn publish_applies_only_the_local_drivers_allocated_device_status() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let expected_claim = allocated_claim(Value::Null);
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
            let patch: Value = serde_json::from_slice(&body).expect("decode patch");
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
                allocated_claim(json!([{
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
            send.send_response(response(200, allocated_claim(Value::Null)));
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
        let expected_claim = allocated_claim(Value::Null);
        let responder = tokio::spawn(async move {
            let (_get, send) = handle.next_request().await.expect("claim get request");
            send.send_response(response(200, expected_claim));
            let (_patch, send) = handle
                .next_request()
                .await
                .expect("claim status patch request");
            send.send_response(response(
                403,
                json!({
                    "kind": "Status",
                    "apiVersion": "v1",
                    "status": "Failure",
                    "reason": "Forbidden",
                    "code": 403,
                }),
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

    async fn read_patch(request: http::Request<Body>) -> Value {
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
        serde_json::from_slice(&bytes).expect("decode patch")
    }

    fn conflict_response() -> http::Response<Body> {
        response(
            409,
            json!({"status": "Failure", "reason": "Conflict", "code": 409}),
        )
    }

    #[tokio::test]
    async fn successive_upserts_preserve_omitted_foreign_and_shared_entries() {
        let (client, mut handle) = mock_kube_client();
        let publisher = ClaimDeviceStatusPublisher::new(client, DRIVER);
        let responder = tokio::spawn(async move {
            let preserved = json!([
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
                current["metadata"]["resourceVersion"] = json!((version + 1).to_string());
                send.send_response(response(200, current.clone()));
            }
        });
        publisher
            .publish(&claim_ref(), [status()])
            .await
            .expect("first upsert");
        let mut updated = status();
        updated.data = Some(json!({"phase": "ready"}));
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
            send.send_response(response(200, allocated_claim(Value::Null)));
            let (patch, send) = handle.next_request().await.expect("initial patch");
            assert_eq!(read_patch(patch).await["metadata"]["resourceVersion"], "1");
            send.send_response(conflict_response());
            let foreign = json!({"driver": "other.example.com", "pool": "p", "device": "d", "data": {"new": true}});
            let mut current = allocated_claim(json!([foreign.clone()]));
            current["metadata"]["resourceVersion"] = json!("2");
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
                send.send_response(response(200, allocated_claim(Value::Null)));
                let (_patch, send) = handle.next_request().await.expect("initial patch");
                send.send_response(conflict_response());
                let mut current = allocated_claim(Value::Null);
                if replaced {
                    current["metadata"]["uid"] = json!("replacement-uid");
                } else {
                    current["status"]["allocation"] = Value::Null;
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
                tokio::time::timeout(std::time::Duration::from_millis(20), handle.next_request())
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
                send.send_response(response(200, allocated_claim(Value::Null)));
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
            let mut current = allocated_claim(Value::Null);
            current["metadata"]["resourceVersion"] = Value::Null;
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
        desired.data = Some(serde_json::from_str(r#"{ "b": 2, "a": 1 }"#).expect("valid JSON"));
        let current = serde_json::from_value(allocated_claim(json!([{
            "driver": DRIVER, "pool": "widget-pool", "device": "widget-0", "data": {"a": 1, "b": 2}
        }])))
        .expect("valid claim");
        assert!(publisher.statuses_are_current(
            &current,
            &publisher.desired_statuses([desired]).expect("valid status")
        ));
    }
}
