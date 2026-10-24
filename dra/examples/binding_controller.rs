//! One explicit validation-controller action before Pod binding. It reports
//! simulated attachment state for an allocated claim using the same safe status
//! API a hardware attachment controller can call. It never allocates a claim.

use std::env;
use std::io;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use k8s_openapi::api::resource::v1 as resourcev1;
use k8s_openapi::apimachinery::pkg::apis::meta::v1 as metav1;
use k8s_openapi::jiff;

use k8s_device_plugin_core::ClaimRef;
use k8s_device_plugin_dra::ClaimDeviceStatus;
use k8s_device_plugin_dra::ClaimDeviceStatusPublisher;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BindingState {
    Pending,
    Ready,
    Failed,
}

impl std::str::FromStr for BindingState {
    type Err = io::Error;

    fn from_str(value: &str) -> io::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "BINDING_STATE must be pending, ready, or failed",
            )),
        }
    }
}

fn statuses_for_claim(
    claim: &resourcev1::ResourceClaim,
    driver: &str,
    state: BindingState,
    now: &metav1::Time,
) -> io::Result<Vec<ClaimDeviceStatus>> {
    let allocation = claim
        .status
        .as_ref()
        .and_then(|status| status.allocation.as_ref())
        .and_then(|allocation| allocation.devices.as_ref())
        .and_then(|devices| devices.results.as_ref())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "claim has no device allocation"))?;
    let mut reports = std::collections::BTreeMap::new();
    for device in allocation.iter().filter(|device| device.driver == driver) {
        if device.binding_conditions.as_ref().is_none_or(Vec::is_empty) {
            continue;
        }
        let key = (
            device.pool.clone(),
            device.device.clone(),
            device.share_id.clone(),
        );
        let report = reports.entry(key).or_insert_with(|| {
            let existing = claim
                .status
                .as_ref()
                .and_then(|status| status.devices.as_ref())
                .and_then(|statuses| {
                    statuses.iter().find(|status| {
                        status.driver == driver
                            && status.pool == device.pool
                            && status.device == device.device
                            && status.share_id == device.share_id
                    })
                });
            let mut report = ClaimDeviceStatus::new(&device.pool, &device.device);
            report.share_id.clone_from(&device.share_id);
            report.conditions = Some(
                existing
                    .and_then(|status| status.conditions.clone())
                    .unwrap_or_default(),
            );
            report
        });
        let conditions = report.conditions.get_or_insert_default();
        for (names, satisfied) in [
            (&device.binding_conditions, state == BindingState::Ready),
            (
                &device.binding_failure_conditions,
                state == BindingState::Failed,
            ),
        ] {
            for name in names.iter().flatten() {
                let status = if satisfied { "True" } else { "False" };
                let current = conditions
                    .iter_mut()
                    .find(|condition| condition.type_ == *name);
                let transition_time = current
                    .as_ref()
                    .filter(|condition| condition.status == status)
                    .map_or_else(
                        || now.clone(),
                        |condition| condition.last_transition_time.clone(),
                    );
                let condition = metav1::Condition {
                    type_: name.clone(),
                    status: status.to_string(),
                    observed_generation: claim.metadata.generation,
                    reason: format!("Validation{state:?}"),
                    message: "simulated attachment state from the validation controller"
                        .to_string(),
                    last_transition_time: transition_time,
                };
                if let Some(current) = current {
                    *current = condition;
                } else {
                    conditions.push(condition);
                }
            }
        }
    }
    if reports.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "claim has no binding conditions for this driver",
        ));
    }
    Ok(reports.into_values().collect())
}

fn required_var(name: &str) -> io::Result<String> {
    env::var(name).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be set: {error}"),
        )
    })
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let namespace = required_var("CLAIM_NAMESPACE")?;
    let name = required_var("CLAIM_NAME")?;
    let uid = required_var("CLAIM_UID")?;
    let state = required_var("BINDING_STATE")?.parse()?;
    let driver = env::var("DRIVER_NAME").unwrap_or_else(|_| "dra.example.com".to_string());
    let mut config = if let Some(context) = env::var("KUBE_CONTEXT")
        .ok()
        .filter(|context| !context.is_empty())
    {
        kube::Config::from_kubeconfig(&kube::config::KubeConfigOptions {
            context: Some(context),
            ..default()
        })
        .await
        .map_err(io::Error::other)?
    } else {
        kube::Config::infer().await.map_err(io::Error::other)?
    };
    config.read_timeout = Some(Duration::from_secs(15));
    config.connect_timeout = Some(Duration::from_secs(10));
    let client = kube::Client::try_from(config).map_err(io::Error::other)?;
    let api: kube::Api<resourcev1::ResourceClaim> =
        kube::Api::namespaced(client.clone(), &namespace);
    let claim = api.get(&name).await.map_err(io::Error::other)?;
    if claim.metadata.uid.as_deref() != Some(&uid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "claim UID changed before reporting binding state",
        ));
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let now = metav1::Time(
        jiff::Timestamp::from_second(i64::try_from(seconds).map_err(io::Error::other)?)
            .map_err(io::Error::other)?,
    );
    let statuses = statuses_for_claim(&claim, &driver, state, &now)?;
    let claim_ref = ClaimRef {
        namespace,
        name,
        uid,
    };
    let result = ClaimDeviceStatusPublisher::new(client, driver)
        .publish_fields(&claim_ref, statuses)
        .await
        .map_err(io::Error::other)?;
    println!(
        "reported {state:?} for {}/{}: {result:?}",
        claim_ref.namespace, claim_ref.name
    );
    Ok(())
}

fn default<T: Default>() -> T {
    T::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocated_claim() -> resourcev1::ResourceClaim {
        resourcev1::ResourceClaim {
            metadata: metav1::ObjectMeta {
                name: Some("claim".into()),
                namespace: Some("default".into()),
                uid: Some("uid".into()),
                generation: Some(1),
                ..default()
            },
            spec: resourcev1::ResourceClaimSpec {
                devices: Some(resourcev1::DeviceClaim::default()),
            },
            status: Some(resourcev1::ResourceClaimStatus {
                allocation: Some(resourcev1::AllocationResult {
                    devices: Some(resourcev1::DeviceAllocationResult {
                        results: Some(vec![resourcev1::DeviceRequestAllocationResult {
                            driver: "dra.example.com".into(),
                            pool: "pool-0".into(),
                            device: "widget-0".into(),
                            request: "widget".into(),
                            binding_conditions: Some(vec!["dra.example.com/Attached".into()]),
                            binding_failure_conditions: Some(vec![
                                "dra.example.com/AttachFailed".into(),
                            ]),
                            ..default()
                        }]),
                        ..default()
                    }),
                    ..default()
                }),
                ..default()
            }),
        }
    }

    #[test]
    fn reports_pending_ready_and_failed_on_per_device_status() {
        let now = metav1::Time(jiff::Timestamp::from_second(100).unwrap());
        for (state, expected) in [
            (BindingState::Pending, ["False", "False"]),
            (BindingState::Ready, ["True", "False"]),
            (BindingState::Failed, ["False", "True"]),
        ] {
            let reports =
                statuses_for_claim(&allocated_claim(), "dra.example.com", state, &now).unwrap();
            assert_eq!(reports[0].pool_name, "pool-0");
            assert_eq!(reports[0].device_name, "widget-0");
            let conditions = reports[0].conditions.as_ref().unwrap();
            assert_eq!(
                [conditions[0].status.as_str(), conditions[1].status.as_str()],
                expected
            );
            assert_eq!(conditions[0].observed_generation, Some(1));
        }
    }

    #[test]
    fn repeated_reports_preserve_transition_times_but_status_changes_advance_them() {
        let mut claim = allocated_claim();
        let old = metav1::Time(jiff::Timestamp::from_second(100).unwrap());
        let now = metav1::Time(jiff::Timestamp::from_second(200).unwrap());
        let reports =
            statuses_for_claim(&claim, "dra.example.com", BindingState::Ready, &old).unwrap();
        let status = resourcev1::AllocatedDeviceStatus {
            device: "widget-0".to_string(),
            pool: "pool-0".to_string(),
            driver: "dra.example.com".to_string(),
            conditions: reports[0].conditions.clone(),
            ..default()
        };
        claim.status.as_mut().unwrap().devices = Some(vec![status]);
        let repeated =
            statuses_for_claim(&claim, "dra.example.com", BindingState::Ready, &now).unwrap();
        assert_eq!(
            repeated[0].conditions.as_ref().unwrap()[0].last_transition_time,
            old
        );
        let changed =
            statuses_for_claim(&claim, "dra.example.com", BindingState::Failed, &now).unwrap();
        assert_eq!(
            changed[0].conditions.as_ref().unwrap()[0].last_transition_time,
            now
        );
    }

    #[test]
    fn refuses_unallocated_and_other_driver_claims_and_unknown_states() {
        let now = metav1::Time(jiff::Timestamp::from_second(100).unwrap());
        assert!(
            statuses_for_claim(
                &allocated_claim(),
                "other.example.com",
                BindingState::Ready,
                &now
            )
            .is_err()
        );
        let mut claim = allocated_claim();
        claim.status = None;
        assert!(statuses_for_claim(&claim, "dra.example.com", BindingState::Ready, &now).is_err());
        assert!("unknown".parse::<BindingState>().is_err());
    }
}
