//! Private metadata v1beta1 and immutable journal v1 wire projections.
use serde::Deserialize;
use serde::Serialize;

use super::*;

// Keep fields in canonical key order, including nested objects. Persisted
// artifacts predate these models and are compared byte-for-byte on retry.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeviceMetadata<'a> {
    pub api_version: &'static str,
    pub kind: &'static str,
    pub metadata: metav1::ObjectMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pod_claim_name: Option<&'a str>,
    pub requests: Vec<RequestMetadata<'a>>,
}

#[derive(Debug, Serialize)]
pub(super) struct RequestMetadata<'a> {
    pub devices: Vec<DeviceMetadataEntry>,
    pub name: &'a str,
}

#[derive(Debug, PartialEq, Serialize)]
pub(super) struct DeviceMetadataEntry {
    pub attributes: BTreeMap<String, resourcev1::DeviceAttribute>,
    pub driver: String,
    pub name: String,
    pub pool: String,
}

// Remote derivation preserves the existing domain identity without adding
// serialization commitments to the public core type. Reject extra identity
// fields: the previous Value equality check required an exact object.
#[derive(Debug, Deserialize, Serialize)]
#[serde(remote = "ClaimRef", deny_unknown_fields)]
struct ClaimIdentity {
    name: String,
    namespace: String,
    uid: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    #[serde(with = "ClaimIdentity")]
    pub claim: ClaimRef,
    pub devices: Vec<JournalDevice>,
    pub documents: BTreeMap<String, String>,
    pub driver: String,
    #[serde(rename = "journalVersion")]
    pub version: u32,
    pub specs: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalDevice {
    cdi: Vec<String>,
    device: String,
    pool: String,
    requests: Vec<String>,
}

impl From<&PreparedDevice> for JournalDevice {
    fn from(device: &PreparedDevice) -> Self {
        Self {
            cdi: device.cdi_device_ids.clone(),
            device: device.device_name.to_string(),
            pool: device.pool_name.to_string(),
            requests: device.request_names.clone(),
        }
    }
}
