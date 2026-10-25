//! Driver-side KEP-5304 identity/attribute snapshots; no API-server extension.
use std::sync::Weak;

use camino::Utf8Component;
use parking_lot::Mutex;

use k8s_device_plugin_core::PreparedDeviceWithMetadata;

use super::*;

mod store;

const CONTAINER_ROOT: &str = "/var/run/kubernetes.io/dra-device-attributes";
const POD_CLAIM_NAME: &str = "resource.kubernetes.io/pod-claim-name";

/// Explicitly selects the beta metadata protocol. Disabled unless attached to
/// the runtime. Paths name directories already exposed to the container runtime.
#[derive(Clone, Debug)]
pub struct DeviceMetadataOptions {
    plugin_directories: Option<(Utf8PathBuf, Utf8PathBuf)>,
    cdi_directory: Utf8PathBuf,
}

impl DeviceMetadataOptions {
    #[must_use]
    pub fn v1beta1() -> Self {
        Self {
            plugin_directories: None,
            cdi_directory: "/var/run/cdi".into(),
        }
    }

    /// Sets driver-local and node-host paths for the same plugin-data mount.
    #[must_use]
    pub fn with_plugin_directories(
        mut self,
        local: impl Into<Utf8PathBuf>,
        host: impl Into<Utf8PathBuf>,
    ) -> Self {
        self.plugin_directories = Some((local.into(), host.into()));
        self
    }

    #[must_use]
    pub fn with_cdi_directory(mut self, directory: impl Into<Utf8PathBuf>) -> Self {
        self.cdi_directory = directory.into();
        self
    }
}

#[derive(Debug)]
pub(crate) struct MetadataRuntime {
    driver: String,
    local: Utf8PathBuf,
    host: Utf8PathBuf,
    cdi: Utf8PathBuf,
    // Held only during registry lookup/update, never across an await.
    locks: Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl MetadataRuntime {
    pub fn new(driver: String, options: DeviceMetadataOptions) -> io::Result<Self> {
        validate_driver_name(&driver).map_err(|error| invalid(error.to_string()))?;
        let default = Utf8Path::new(dra::KUBELET_PLUGINS_PATH).join(&driver);
        let (local, host) = options
            .plugin_directories
            .unwrap_or_else(|| (default.clone(), default));
        for path in [&local, &host, &options.cdi_directory] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|p| matches!(p, Utf8Component::ParentDir))
                || path.as_str().contains('\0')
            {
                return Err(invalid(
                    "metadata directories must be absolute UTF-8 paths without parent traversal",
                ));
            }
        }
        Ok(Self {
            driver,
            local,
            host,
            cdi: options.cdi_directory,
            locks: Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn publish(
        self: &Arc<Self>,
        claim: resourcev1::ResourceClaim,
        devices: Vec<PreparedDeviceWithMetadata>,
        guards: Arc<Vec<tokio::sync::OwnedMutexGuard<()>>>,
    ) -> io::Result<Vec<PreparedDevice>> {
        let plan = Plan::build(self, &claim, devices)?;
        let runtime = Arc::clone(self);
        task::spawn_blocking(move || {
            let _guards = guards; // Keep ownership through I/O even if the RPC is cancelled.
            store::persist(&runtime, &plan)?;
            Ok(plan.devices)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn cleanup(
        self: &Arc<Self>,
        claim: ClaimRef,
        guards: Arc<Vec<tokio::sync::OwnedMutexGuard<()>>>,
    ) -> io::Result<()> {
        let runtime = Arc::clone(self);
        task::spawn_blocking(move || {
            let _guards = guards;
            store::cleanup(&runtime, &claim)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub async fn lock_claims(&self, claims: &[ClaimRef]) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
        let ids = claims
            .iter()
            .map(|c| c.uid.as_str())
            .collect::<BTreeSet<_>>();
        let mut guards = Vec::with_capacity(ids.len());
        for id in ids {
            let mutex = {
                let mut locks = self.locks.lock();
                locks.retain(|_, value| value.strong_count() > 0);
                if let Some(mutex) = locks.get(id).and_then(Weak::upgrade) {
                    mutex
                } else {
                    let mutex = Arc::new(tokio::sync::Mutex::new(()));
                    locks.insert(id.to_string(), Arc::downgrade(&mutex));
                    mutex
                }
            };
            guards.push(mutex.lock_owned().await);
        }
        guards
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn label(name: &str, limit: usize) -> bool {
    !name.is_empty()
        && name.len() <= limit
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

fn dns_subdomain(name: &str, limit: usize) -> bool {
    name.len() <= limit && name.split('.').all(|part| label(part, 63))
}

fn request_ref(name: &str) -> io::Result<&str> {
    let parts = name.split('/').collect::<Vec<_>>();
    if !(1..=2).contains(&parts.len()) || !parts.iter().all(|part| label(part, 63)) {
        return Err(invalid("invalid device request reference"));
    }
    Ok(parts[0])
}

fn attribute_key(key: &str) -> bool {
    let (domain, name) = key
        .split_once('/')
        .map_or((None, key), |(d, n)| (Some(d), n));
    domain.is_none_or(|d| dns_subdomain(d, 63))
        && !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && (name.as_bytes()[0].is_ascii_alphabetic() || name.starts_with('_'))
}

fn attribute_value(value: &AttributeValue) -> bool {
    match value {
        AttributeValue::String(value) => value.len() <= 64,
        AttributeValue::Version(value) => {
            value.len() <= 64 && semver::Version::parse(value).is_ok()
        }
        AttributeValue::Int(_) | AttributeValue::Bool(_) => true,
    }
}

/// Immutable journal contents; the complete plan is recorded before CDI is visible.
#[derive(Debug)]
struct Plan {
    claim: ClaimRef,
    documents: BTreeMap<String, String>,
    specs: BTreeMap<String, String>,
    devices: Vec<PreparedDevice>,
}

impl Plan {
    fn build(
        runtime: &MetadataRuntime,
        claim: &resourcev1::ResourceClaim,
        prepared: Vec<PreparedDeviceWithMetadata>,
    ) -> io::Result<Self> {
        let identity = ClaimRef {
            namespace: claim.metadata.namespace.clone().unwrap_or_default(),
            name: claim.metadata.name.clone().unwrap_or_default(),
            uid: claim.metadata.uid.clone().unwrap_or_default(),
        };
        if !dns_subdomain(&identity.name, 253)
            || !label(&identity.namespace, 63)
            || !label(&identity.uid, 64)
        {
            return Err(invalid("invalid metadata claim identity"));
        }
        let annotation = claim
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(POD_CLAIM_NAME));
        if annotation.is_some_and(|a| !label(a, 63)) {
            return Err(invalid("invalid pod claim name annotation"));
        }
        let declared = claim
            .spec
            .devices
            .as_ref()
            .and_then(|devices| devices.requests.as_ref())
            .into_iter()
            .flatten()
            .map(|r| r.name.as_str())
            .collect::<BTreeSet<_>>();
        if declared.iter().any(|r| !label(r, 63)) {
            return Err(invalid("invalid declared request name"));
        }
        let allocations = claim
            .status
            .as_ref()
            .and_then(|s| s.allocation.as_ref())
            .and_then(|a| a.devices.as_ref())
            .and_then(|a| a.results.as_deref())
            .unwrap_or_default();
        let local = allocations
            .iter()
            .filter(|a| a.driver == runtime.driver)
            .collect::<Vec<_>>();
        for allocation in &local {
            let base = request_ref(&allocation.request)?;
            let declared_request = claim
                .spec
                .devices
                .as_ref()
                .and_then(|devices| devices.requests.as_ref())
                .and_then(|requests| requests.iter().find(|request| request.name == base))
                .ok_or_else(|| invalid("allocation references an undeclared request"))?;
            if let Some((_, subrequest)) = allocation.request.split_once('/')
                && !declared_request
                    .first_available
                    .as_ref()
                    .is_some_and(|requests| {
                        requests.iter().any(|request| request.name == subrequest)
                    })
            {
                return Err(invalid("allocation references an undeclared subrequest"));
            }
        }
        if local.iter().any(|a| a.share_id.is_some()) {
            return Err(invalid(
                "container metadata does not yet support device share IDs",
            ));
        }
        let mut by_request: BTreeMap<String, BTreeMap<(String, String), json::Value>> =
            BTreeMap::new();
        let mut full_names: BTreeMap<String, String> = BTreeMap::new();
        let mut wire = Vec::with_capacity(prepared.len());
        for entry in prepared {
            let device = entry.device;
            if !label(&device.device_name, 63)
                || device.pool_name.len() > 253
                || !device.pool_name.split('/').all(|p| dns_subdomain(p, 253))
            {
                return Err(invalid("invalid prepared metadata device identity"));
            }
            if entry.attributes.len() > 32
                || entry
                    .attributes
                    .iter()
                    .any(|(k, v)| !attribute_key(k) || !attribute_value(v))
            {
                return Err(invalid(
                    "invalid metadata attribute name or attribute count exceeds 32",
                ));
            }
            let attributes = entry
                .attributes
                .into_iter()
                .map(|(k, v)| (k, resourceslice::attribute_value_to_wire(v)))
                .collect::<BTreeMap<_, _>>();
            let requested = if device.request_names.is_empty() {
                declared.iter().map(|r| (*r).to_string()).collect()
            } else {
                device.request_names.clone()
            };
            let mut base_names = BTreeSet::new();
            for requested in requested {
                let base = request_ref(&requested)?;
                if !declared.contains(base) {
                    return Err(invalid("prepared device references an undeclared request"));
                }
                let matching = local
                    .iter()
                    .filter(|a| {
                        a.pool == device.pool_name
                            && a.device == device.device_name
                            && (a.request == requested
                                || (requested == base
                                    && a.request.starts_with(&format!("{base}/"))))
                    })
                    .collect::<Vec<_>>();
                // An all-requests result is valid only for actual allocations;
                // do not invent this physical device in another request's file.
                if matching.is_empty() {
                    if device.request_names.is_empty() {
                        continue;
                    }
                    return Err(invalid(
                        "prepared metadata device does not match this driver's allocation",
                    ));
                }
                for allocation in matching {
                    if request_ref(&allocation.request)? != base {
                        return Err(invalid("invalid allocated request reference"));
                    }
                    if full_names
                        .get(base)
                        .is_some_and(|old| old != &allocation.request)
                    {
                        return Err(invalid("conflicting subrequests for one metadata file"));
                    }
                    full_names.insert(base.to_string(), allocation.request.clone());
                    let content = json::json!({
                        "driver": runtime.driver,
                        "pool": device.pool_name,
                        "name": device.device_name,
                        "attributes": attributes
                    });
                    let key = (device.pool_name.clone(), device.device_name.clone());
                    let devices = by_request.entry(base.to_string()).or_default();
                    if devices.get(&key).is_some_and(|old| old != &content) {
                        return Err(invalid(
                            "conflicting attributes for the same allocated device",
                        ));
                    }
                    devices.insert(key, content);
                }
                base_names.insert(base.to_string());
            }
            if base_names.is_empty() {
                return Err(invalid(
                    "prepared metadata device has no local request allocation",
                ));
            }
            let mut physical = device;
            // Preserve empty => all-requests semantics for physical CDI edits.
            if !physical.request_names.is_empty() {
                physical.request_names = base_names.into_iter().collect();
            }
            wire.push(physical);
        }
        let mut documents = BTreeMap::new();
        let mut specs = BTreeMap::new();
        for (base, devices) in by_request {
            let mut object = json::json!({
                "apiVersion": "metadata.resource.k8s.io/v1beta1",
                "kind": "DeviceMetadata",
                "metadata": {
                    "name": identity.name,
                    "namespace": identity.namespace,
                    "uid": identity.uid,
                    "generation": 1
                },
                "requests": [
                    {
                        "name": full_names[&base],
                        "devices": devices.values().collect::<Vec<_>>()
                    }
                ]
            });
            if let Some(annotation) = annotation {
                object["podClaimName"] = json::json!(annotation);
            }
            let document = json::to_string(&object).map_err(io::Error::other)? + "\n";
            documents.insert(base.clone(), document);
            let id = format!("{}_{}", identity.uid, base);
            let host = runtime
                .host
                .join("dra-device-metadata")
                .join(&identity.uid)
                .join(&base)
                .join("metadata.json");
            let container = if let Some(annotation) = annotation {
                format!(
                    "{CONTAINER_ROOT}/resourceclaimtemplates/{annotation}/{base}/{}-metadata.json",
                    runtime.driver
                )
            } else {
                format!(
                    "{CONTAINER_ROOT}/resourceclaims/{}/{base}/{}-metadata.json",
                    identity.name, runtime.driver
                )
            };
            let spec = json::json!({
                "cdiVersion": "0.8.0",
                "kind": format!("{}/metadata", runtime.driver),
                "devices": [{
                    "name": id,
                    "containerEdits": {
                        "mounts": [{
                            "hostPath": host.as_str(),
                            "containerPath": container,
                            "options": ["ro", "bind"]
                        }]
                    }
                }]
            });
            specs.insert(
                base.clone(),
                json::to_string(&spec).map_err(io::Error::other)? + "\n",
            );
            let representative = devices
                .into_keys()
                .next()
                .ok_or_else(|| invalid("empty metadata request group"))?;
            wire.push(PreparedDevice {
                request_names: vec![base],
                pool_name: representative.0,
                device_name: representative.1,
                cdi_device_ids: vec![format!("{}/metadata={id}", runtime.driver)],
            });
        }
        Ok(Self {
            claim: identity,
            documents,
            specs,
            devices: wire,
        })
    }

    fn journal(&self, driver: &str) -> json::Value {
        json::json!({
            "journalVersion": 1,
            "driver": driver,
            "claim": {
                "namespace": self.claim.namespace,
                "name": self.claim.name,
                "uid": self.claim.uid
            },
            "documents": self.documents,
            "specs": self.specs,
            "devices": self.devices.iter().map(|d| json::json!({
                "requests": d.request_names,
                "pool": d.pool_name,
                "device": d.device_name,
                "cdi": d.cdi_device_ids
            })).collect::<Vec<_>>()
        })
    }
}

#[cfg(test)]
mod tests;
