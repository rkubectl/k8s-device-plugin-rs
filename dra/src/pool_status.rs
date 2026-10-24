//! Opt-in requests to Kubernetes' pool-status controller. No driver accounting.

use std::fmt;
use std::time::Duration;

use k8s_openapi::api::resource::v1alpha3::ResourcePoolStatusRequest;
use k8s_openapi::api::resource::v1alpha3::ResourcePoolStatusRequestSpec;
use k8s_openapi::api::resource::v1alpha3::ResourcePoolStatusRequestStatus;
use kube::Api;
use kube::api::DeleteParams;
use kube::api::PostParams;
use kube::api::Preconditions;

const API_VERSION: &str = "resource.k8s.io/v1alpha3";

/// Errors from discovery, processing, or UID-safe request cleanup.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PoolStatusError {
    #[error(
        "ResourcePoolStatusRequest is not served; enable DRAResourcePoolStatus and resource.k8s.io/v1alpha3"
    )]
    Unsupported,
    #[error(transparent)]
    Api(#[from] kube::Error),
    #[error("pool-status request did not complete within {0:?}")]
    Timeout(Duration),
    #[error("pool-status request failed: {reason}: {message}")]
    Failed { reason: String, message: String },
    #[error("pool-status request {0} was replaced")]
    Replaced(String),
    #[error("created pool-status request has no server-assigned name or UID")]
    MissingIdentity,
}

/// Creates explicit, one-shot requests to the optional Kubernetes controller.
///
/// This has no effect on the default node-driver lifecycle. The caller supplies
/// a client with appropriate transport timeouts and create/get/delete access
/// to cluster-scoped ResourcePoolStatusRequests.
#[derive(Clone)]
pub struct PoolStatusClient {
    client: kube::Client,
}

impl fmt::Debug for PoolStatusClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PoolStatusClient").finish_non_exhaustive()
    }
}

impl PoolStatusClient {
    pub fn new(client: kube::Client) -> Self {
        Self { client }
    }

    /// Discovers the alpha API before creating a generated-name request.
    ///
    /// Keep the returned handle until explicit deletion succeeds. Cancelling
    /// creation after the server accepts it can leave a `dra-pool-status-*`
    /// object; callers must account for ambiguous writes in their own recovery.
    pub async fn create(
        &self,
        spec: ResourcePoolStatusRequestSpec,
    ) -> Result<PoolStatusQuery, PoolStatusError> {
        let discovery = match self.client.list_api_group_resources(API_VERSION).await {
            Ok(discovery) => discovery,
            Err(kube::Error::Api(error)) if error.code == 404 => {
                return Err(PoolStatusError::Unsupported);
            }
            Err(error) => return Err(error.into()),
        };
        if !discovery
            .resources
            .iter()
            .any(|resource| resource.name == "resourcepoolstatusrequests" && !resource.namespaced)
        {
            return Err(PoolStatusError::Unsupported);
        }
        let api: Api<ResourcePoolStatusRequest> = Api::all(self.client.clone());
        let request = ResourcePoolStatusRequest {
            metadata: kube::api::ObjectMeta {
                generate_name: Some("dra-pool-status-".to_string()),
                ..Default::default()
            },
            spec,
            status: None,
        };
        let created = api.create(&PostParams::default(), &request).await?;
        let name = created.metadata.name.filter(|name| !name.is_empty());
        let uid = created.metadata.uid.filter(|uid| !uid.is_empty());
        match (name, uid) {
            (Some(name), Some(uid)) => Ok(PoolStatusQuery { api, name, uid }),
            _ => Err(PoolStatusError::MissingIdentity),
        }
    }
}

/// An owned request identity. Waiting never relinquishes ownership.
///
/// Call [`Self::delete`] after success, failure or timeout. Dropping this handle
/// does not spawn cleanup tasks or delete objects. A completed request is an
/// immutable snapshot: create a new request to observe later allocation changes.
#[must_use = "pool-status requests require explicit UID-safe deletion"]
pub struct PoolStatusQuery {
    api: Api<ResourcePoolStatusRequest>,
    name: String,
    uid: String,
}

impl fmt::Debug for PoolStatusQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PoolStatusQuery")
            .field("name", &self.name)
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}

impl PoolStatusQuery {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Waits for Complete=True or Failed=True, bounding both HTTP reads and
    /// polling sleeps. Returned types cover the installed v1.36 schema: newer
    /// partition/shareable summary extensions are not represented by this API.
    /// Missing device counts remain None, including pools with validationError.
    pub async fn wait(
        &self,
        timeout: Duration,
    ) -> Result<ResourcePoolStatusRequestStatus, PoolStatusError> {
        tokio::time::timeout(timeout, async {
            loop {
                let current = self.api.get(&self.name).await?;
                if current.metadata.uid.as_deref() != Some(self.uid.as_str()) {
                    return Err(PoolStatusError::Replaced(self.name.clone()));
                }
                if let Some(status) = current.status {
                    let conditions = status.conditions.as_deref().unwrap_or_default();
                    if let Some(failed) = conditions
                        .iter()
                        .find(|condition| condition.type_ == "Failed" && condition.status == "True")
                    {
                        return Err(PoolStatusError::Failed {
                            reason: failed.reason.clone(),
                            message: failed.message.clone(),
                        });
                    }
                    if conditions.iter().any(|condition| {
                        condition.type_ == "Complete" && condition.status == "True"
                    }) {
                        return Ok(status);
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .map_err(|_| PoolStatusError::Timeout(timeout))?
    }

    /// Deletes only this UID, atomically refusing any replacement at the same
    /// name. Already absent is success. Other errors retain the handle for retry.
    pub async fn delete(&self) -> Result<(), PoolStatusError> {
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(self.uid.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        match self.api.delete(&self.name, &params).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use serde_json as json;

    use k8s_device_plugin_test::kube_mock::mock_kube_client;
    use k8s_device_plugin_test::kube_mock::not_found;
    use k8s_device_plugin_test::kube_mock::respond;

    use super::*;

    fn discovery() -> k8s_openapi::apimachinery::pkg::apis::meta::v1::APIResourceList {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::APIResource;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::APIResourceList;

        APIResourceList {
            group_version: API_VERSION.into(),
            resources: vec![APIResource {
                name: "resourcepoolstatusrequests".into(),
                singular_name: "resourcepoolstatusrequest".into(),
                namespaced: false,
                kind: "ResourcePoolStatusRequest".into(),
                verbs: ["create", "get", "delete"].map(String::from).into(),
                ..Default::default()
            }],
        }
    }

    fn request(status: Option<ResourcePoolStatusRequestStatus>) -> ResourcePoolStatusRequest {
        ResourcePoolStatusRequest {
            metadata: kube::api::ObjectMeta {
                name: Some("dra-pool-status-test".into()),
                uid: Some("request-uid".into()),
                ..Default::default()
            },
            spec: ResourcePoolStatusRequestSpec {
                driver: "dra.example.com".into(),
                ..Default::default()
            },
            status,
        }
    }

    fn status(condition: &str) -> ResourcePoolStatusRequestStatus {
        use k8s_openapi::api::resource::v1alpha3::PoolStatus;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;

        ResourcePoolStatusRequestStatus {
            pool_count: 1,
            conditions: Some(vec![Condition {
                type_: condition.into(),
                status: "True".into(),
                reason: "Test".into(),
                message: "controller result".into(),
                last_transition_time: Time("2026-10-23T00:00:00Z".parse().unwrap()),
                observed_generation: None,
            }]),
            pools: Some(vec![PoolStatus {
                driver: "dra.example.com".into(),
                pool_name: "widget-pool".into(),
                generation: 7,
                validation_error: Some("incomplete publication".into()),
                ..Default::default()
            }]),
        }
    }

    fn query(client: kube::Client) -> PoolStatusQuery {
        PoolStatusQuery {
            api: Api::all(client),
            name: "dra-pool-status-test".into(),
            uid: "request-uid".into(),
        }
    }

    #[tokio::test]
    async fn discovery_create_complete_and_uid_safe_delete() {
        let (client, mut handle) = mock_kube_client();
        let responder = tokio::spawn(async move {
            let get = respond(&mut handle, 200, &discovery()).await;
            assert_eq!(get.uri().path(), "/apis/resource.k8s.io/v1alpha3");
            let create = respond(&mut handle, 201, &request(None)).await;
            assert_eq!(create.method(), http::Method::POST);
            let body = json::from_slice::<json::Value>(
                &create.into_body().collect().await.unwrap().to_bytes(),
            )
            .unwrap();
            assert_eq!(body["metadata"]["generateName"], "dra-pool-status-");
            assert!(body["metadata"]["name"].is_null());
            assert_eq!(
                body["spec"],
                json::json!({"driver":"dra.example.com", "poolName":"widget-pool", "limit":1})
            );
            respond(&mut handle, 200, &request(Some(status("Complete")))).await;
            let delete = respond(&mut handle, 200, &request(Some(status("Complete")))).await;
            assert_eq!(delete.method(), http::Method::DELETE);
            let body = json::from_slice::<json::Value>(
                &delete.into_body().collect().await.unwrap().to_bytes(),
            )
            .unwrap();
            assert_eq!(body["preconditions"]["uid"], "request-uid");
        });
        let query = PoolStatusClient::new(client)
            .create(ResourcePoolStatusRequestSpec {
                driver: "dra.example.com".into(),
                pool_name: Some("widget-pool".into()),
                limit: Some(1),
            })
            .await
            .unwrap();
        let status = query.wait(Duration::from_secs(5)).await.unwrap();
        let pool = &status.pools.unwrap()[0];
        assert_eq!(pool.total_devices, None);
        assert_eq!(pool.allocated_devices, None);
        assert_eq!(pool.available_devices, None);
        assert_eq!(
            pool.validation_error.as_deref(),
            Some("incomplete publication")
        );
        query.delete().await.unwrap();
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_discovery_never_creates_a_request() {
        for code in [200, 404] {
            let (client, mut handle) = mock_kube_client();
            let responder = tokio::spawn(async move {
                if code == 404 {
                    respond(&mut handle, code, &not_found()).await;
                } else {
                    let mut body = discovery();
                    body.resources.clear();
                    respond(&mut handle, code, &body).await;
                }
            });
            let result = PoolStatusClient::new(client)
                .create(ResourcePoolStatusRequestSpec::default())
                .await;
            assert!(matches!(result, Err(PoolStatusError::Unsupported)));
            responder.await.unwrap();
        }
    }

    #[tokio::test]
    async fn discovery_forbidden_is_an_api_error() {
        let (client, mut handle) = mock_kube_client();
        let responder = tokio::spawn(async move {
            respond(
                &mut handle,
                403,
                &kube::core::Status::failure("test failure", "Forbidden").with_code(403),
            )
            .await;
        });
        let result = PoolStatusClient::new(client)
            .create(ResourcePoolStatusRequestSpec::default())
            .await;
        assert!(
            matches!(result, Err(PoolStatusError::Api(kube::Error::Api(error))) if error.code == 403)
        );
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn failed_processing_retains_handle_for_cleanup() {
        let (client, mut handle) = mock_kube_client();
        let responder = tokio::spawn(async move {
            respond(&mut handle, 200, &request(Some(status("Failed")))).await;
            respond(&mut handle, 404, &not_found()).await;
        });
        let query = query(client);
        assert!(matches!(query.wait(Duration::from_secs(5)).await,
            Err(PoolStatusError::Failed { reason, message }) if reason == "Test" && message == "controller result"));
        query.delete().await.unwrap();
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_bounds_a_stalled_http_read_and_still_allows_cleanup() {
        let (client, mut handle) = mock_kube_client();
        let query = query(client);
        let responder = tokio::spawn(async move {
            let (_get, send) = handle.next_request().await.unwrap();
            // Hold the response beyond the outer wait deadline.
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(send);
            respond(&mut handle, 404, &not_found()).await;
        });
        assert!(matches!(
            query.wait(Duration::from_millis(10)).await,
            Err(PoolStatusError::Timeout(_))
        ));
        query.delete().await.unwrap();
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn replaced_request_is_not_read_as_ours_or_deleted_without_a_uid_guard() {
        let (client, mut handle) = mock_kube_client();
        let responder = tokio::spawn(async move {
            let mut replacement = request(Some(status("Complete")));
            replacement.metadata.uid = Some("replacement-uid".into());
            respond(&mut handle, 200, &replacement).await;
            let delete = respond(
                &mut handle,
                409,
                &kube::core::Status::failure("test failure", "Conflict").with_code(409),
            )
            .await;
            let body = json::from_slice::<json::Value>(
                &delete.into_body().collect().await.unwrap().to_bytes(),
            )
            .unwrap();
            assert_eq!(body["preconditions"]["uid"], "request-uid");
        });
        let query = query(client);
        assert!(matches!(
            query.wait(Duration::from_secs(5)).await,
            Err(PoolStatusError::Replaced(_))
        ));
        assert!(
            matches!(query.delete().await, Err(PoolStatusError::Api(kube::Error::Api(error))) if error.code == 409)
        );
        responder.await.unwrap();
    }
}
