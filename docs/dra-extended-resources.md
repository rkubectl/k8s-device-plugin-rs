# Consume DRA devices as extended resources

Kubernetes v1.37 makes DRA extended-resource allocation stable. It lets a
`DeviceClass` turn devices selected from a DRA driver's `ResourceSlice` objects
into a familiar container resource such as `acme.example.com/widget`. A
workload requests that resource in `resources.limits`; Kubernetes creates and
manages the backing `ResourceClaim` automatically.

This guide has two audiences:

- **Driver authors** implement and deploy the DRA backend that publishes and
  prepares devices.
- **Platform and workload authors** create a `DeviceClass` mapping, then
  consume a device through ordinary container resources.

The feature is GA in Kubernetes v1.37. Kubernetes v1.36 has an earlier beta
form, but this workspace's supported v1.36 baseline does not depend on it.
Use a v1.37 cluster for the manifests in this guide.

For the Kubernetes API semantics and the special
`deviceclass.resource.kubernetes.io/<DeviceClass>` resource form, see the
[upstream DRA API documentation](https://kubernetes.io/docs/concepts/resource-management/dynamic-resource-allocation/dra-api/#extended-resource-allocation-by-dra).

## What Kubernetes does

```text
DRA driver                 Kubernetes control plane                 workload
----------                 ------------------------                 --------
ResourceSlice ──────> DeviceClass extendedResourceName ──────> Pod limit
       │                         │                                     │
       │                         └──── scheduler generates ───────────┘
       │                                      ResourceClaim
       │                                            │
       └──────── kubelet resolves allocation ───────┘
                              │
                    NodePrepareResources
                              │
                    PreparedDevice / CDI edits
                              │
                         container starts
```

The application does not create, name, or clean up a `ResourceClaim`. It only
sets a resource limit. Kubernetes records the generated claim in the Pod and
deletes it when the Pod no longer needs it.

## 1. Implement the driver

No separate Rust runtime path is required for extended-resource consumption.
The driver still has the same three responsibilities as claim-based DRA:

1. Publish stable pool and device names through `ResourcePool::devices`.
2. Turn each allocated `ResolvedClaim` into `PreparedDevice` values in
   `ClaimPreparer::prepare`.
3. Undo only driver-owned preparation in `ClaimPreparer::unprepare`.

The complete, copyable implementation is
[`dra/examples/minimal_driver.rs`](../dra/examples/minimal_driver.rs). It is a
validation driver that publishes `widget-0` in `widget-pool` and prepares it by
returning the CDI device ID `dra.example.com/widget=widget-0`. Replace its
static inventory and CDI writer with your hardware discovery and preparation
code, but retain the idempotent claim lifecycle.

The essential backend shape is deliberately the same for explicit and
generated claims:

```rust
#[async_trait]
impl ResourcePool for MyDriver {
    async fn devices(&self) -> BTreeMap<String, Vec<PoolDevice>> {
        // The scheduler persists these pool and device names in allocations.
        BTreeMap::from([(self.pool_name.clone(), self.current_devices())])
    }
}

#[async_trait]
impl ClaimPreparer for MyDriver {
    async fn prepare(
        &self,
        claims: &[ResolvedClaim],
    ) -> BTreeMap<ClaimRef, Result<Vec<PreparedDevice>, PrepareError>> {
        // Validate each allocated pool/device pair, make preparation
        // repeatable, and return CDI IDs or other driver-owned artifacts.
        todo!()
    }

    async fn unprepare(&self, claim: &ClaimRef) -> Result<(), PrepareError> {
        // Safely reverse artifacts for retries and normal teardown.
        todo!()
    }
}
```

`DraPlugin::new(client, driver_name, node_name, driver).run().await` then
handles ResourceSlice publication, pluginwatcher registration, and kubelet RPC
serving. Follow the [first DRA driver checklist](getting-started.md#first-dra-driver)
and the [deployment contract](../dra/k8s/README.md#production-deployment-contract)
for the complete binary, DaemonSet, RBAC, and upgrade requirements.

## 2. Map the driver to an extended resource

After the driver is deployed and publishing devices, a cluster operator creates
a `DeviceClass`. Its selector must match only the intended DRA devices. Its
`extendedResourceName` is the name workload authors will request.

```yaml
apiVersion: resource.k8s.io/v1
kind: DeviceClass
metadata:
  name: acme-widget
spec:
  selectors:
    - cel:
        expression: device.driver == "acme.example.com"
  extendedResourceName: acme.example.com/widget
```

Use a DNS-qualified resource name you own. The mapping is cluster-scoped, so
coordinate its name, selector, and access policy with cluster operators. A
broad selector can make unrelated devices eligible for the resource request.

## 3. Consume it from a Pod

The workload uses the mapped name in a container limit and does **not** include
`spec.resourceClaims` or a separate `ResourceClaim` object:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: widget-consumer
  namespace: default
spec:
  restartPolicy: Never
  containers:
    - name: app
      image: registry.example.com/widget-app:1.0
      resources:
        limits:
          acme.example.com/widget: 1
```

The scheduler selects one matching DRA device and Kubernetes records the
generated claim name in `.spec.resourceClaims[0].resourceClaimName`. Normal
ResourceClaim allocation, reservation, kubelet preparation, CDI injection, and
unpreparation follow from there.

The full, runnable repository fixture uses the same pattern with the example
driver's name:

```yaml
spec:
  extendedResourceName: dra.example.com/widget
---
spec:
  containers:
    - resources:
        limits:
          dra.example.com/widget: 1
```

See [`dra/hack/e2e-extended-resource.yaml`](../dra/hack/e2e-extended-resource.yaml)
for the complete `DeviceClass` and consumer Pod, including the CDI assertion.

## 4. Inspect the generated claim and device status

Once the Pod has been created, obtain its generated claim name without guessing
it. The following commands stop immediately if Kubernetes has not recorded one:

```sh
namespace=default
pod=widget-consumer
claim_name="$(kubectl -n "$namespace" get pod "$pod" \
  -o jsonpath='{.spec.resourceClaims[0].resourceClaimName}')"
test -n "$claim_name"

kubectl -n "$namespace" get resourceclaim "$claim_name" -o yaml
kubectl -n "$namespace" get pod "$pod" -o yaml
```

The claim's allocation and reservation are scheduler-owned. If the driver uses
`ClaimDeviceStatusPublisher`, entries under `status.devices` show the
driver-owned preparation status as well. The minimal driver publishes
`{"phase":"prepared"}` after it has successfully prepared the allocated
device; it does not report success before its preparation work completes.

## 5. Run the end-to-end example

The checked-in smoke test deploys the example DRA DaemonSet, creates the
mapping and Pod, confirms the CDI marker `DRA_E2E_DEVICE=widget-0`, verifies
the generated claim's status, and waits for generated-claim cleanup after Pod
deletion:

```sh
# Build a native linux/arm64 image from the repository root.
container build --platform linux/arm64 -f dra/Dockerfile \
  -t k8s-device-plugin-dra-example .

# Make it available to a Kubernetes v1.37 cluster and deploy the driver.
container k8s-versioned load-image --name dra-validation-137 \
  k8s-device-plugin-dra-example
kubectl --context dra-validation-137 apply -k dra/k8s

# Exercise consumption through resources.limits, with no hand-authored claim.
KUBE_CONTEXT=dra-validation-137 dra/hack/e2e-extended-resource.sh
```

The serial local matrix runs this v1.37 fixture as well as the explicit-claim
baseline on v1.36 and v1.37:

```sh
mise run dra-validate-k8s
```

The Apple Container profiles are native linux/arm64 and use Rosetta-disabled
configuration. The DRA validation fixture is intentionally not production RBAC
or a production DaemonSet template.

## Coexistence and migration

The same resource name can be supplied by a classic Device Plugin on some
nodes and by a DRA `DeviceClass` mapping on other nodes. Do not provide that
same extended resource through both mechanisms on a single node: there must be
one authoritative allocator for a hardware inventory on each node.

This permits a controlled migration:

1. Keep legacy nodes on the classic device plugin.
2. Deploy the DRA driver and `DeviceClass` mapping on a separate node pool.
3. Run existing Pods that request `acme.example.com/widget`; the scheduler can
   select either compatible node pool.
4. Move nodes only after ensuring the old and new providers cannot allocate the
   same physical device.

Choose an explicit `ResourceClaim` when a workload needs multiple named
requests, constraints, allocation configuration, or claim sharing semantics.
Choose the extended-resource bridge when its simple cardinality and familiar
`resources.limits` interface are sufficient.

## Troubleshooting

| Symptom | Check |
|---|---|
| Pod remains Pending | Check the Pod events, `ResourceSlice` objects, and whether the `DeviceClass` selector matches the driver and device attributes. |
| No generated claim name | Confirm the API server, scheduler, controller manager, and kubelet are Kubernetes v1.37 with the stable DRA extended-resource feature available. |
| Claim allocated but container has no device access | Inspect the DRA DaemonSet logs for `preparing DRA claim`, the driver's `PreparedDevice` output, and the node's CDI runtime configuration. |
| Claim status has no driver entry | Confirm that the driver creates `ClaimDeviceStatusPublisher`, publishes only after preparation, and has the required node-aware RBAC. |
| Conflicting capacity or allocations | Ensure the classic device plugin and DRA mapping do not advertise the same extended resource for the same hardware on one node. |

For the DRA protocol boundary and API-version details, return to the
[crate guide](../dra/README.md). For a DRA-native workload that needs richer
selection, see the [explicit `ResourceClaim` smoke fixture](../dra/hack/e2e-smoke.yaml).
