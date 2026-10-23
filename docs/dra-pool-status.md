# Resource-pool status

`PoolStatusClient` provides opt-in requests to Kubernetes' pool-status
controller. It discovers `resource.k8s.io/v1alpha3` before creating a
cluster-scoped `ResourcePoolStatusRequest`, waits for a bounded result, and
returns the controller's snapshot. The node driver does not calculate counts
or run another accounting controller.

This alpha API requires `DRAResourcePoolStatus` in the API server and
controller manager plus the served `resource.k8s.io/v1alpha3` runtime API.
The [isolated advanced profile](dra-workload-claims.md#requirements-and-profile)
enables those requirements on pinned v1.37.0. The default v1.36 baseline
and the driver's deployment permissions are unchanged. Pool filters and
ordinary device counts have no NUMA dependency.

## Client and example

The caller supplies a `kube::Client` with bounded transport timeouts and
cluster-scoped create/get/delete access for `resourcepoolstatusrequests`.
Those are operator/client permissions, not extra node-driver permissions.
Discovery distinguishes an unserved API from authorization/network errors.

```rust,no_run
use std::time::Duration;
use k8s_device_plugin_dra::PoolStatusClient;
use k8s_openapi::api::resource::v1alpha3::ResourcePoolStatusRequestSpec;

# async fn inspect(client: kube::Client) -> Result<(), Box<dyn std::error::Error>> {
let query = PoolStatusClient::new(client)
    .create(ResourcePoolStatusRequestSpec {
        driver: "dra.example.com".into(),
        pool_name: Some("widget-pool".into()),
        limit: Some(100),
    })
    .await?;
let result = query.wait(Duration::from_secs(60)).await;
let cleanup = query.delete().await; // Also attempted after failure or timeout.
cleanup?;
let status = result?;
println!("{} pools matched", status.pool_count);
# Ok(())
# }
```

Keep the query handle until cleanup succeeds. Its delete uses an atomic UID
precondition, so it cannot delete a replacement with the same name. A missing
object is already cleaned up. Waiting also refuses a changed UID. The wait
deadline includes HTTP reads and polling sleeps; it is not merely a limit on
the number of polls. `Failed=True` surfaces the controller's reason/message.

Dropping the handle does not start detached cleanup work. Callers that cancel
an operation must arrange cleanup themselves. As with any create request,
cancellation or a network failure after server acceptance can leave an
ambiguously created object; generated names use the `dra-pool-status-` prefix.
The example logs its assigned name and attempts deletion on every normal
success, processing failure and wait-timeout path. Cleanup errors identify
the request for an operator to retry.

Run the host-side example against an explicitly selected cluster:

```sh
KUBE_CONTEXT=dra-validation-advanced-137 DRIVER_NAME=dra.example.com \
  POOL_NAME=widget-pool cargo run --locked -p k8s-device-plugin-dra \
  --example pool_status
```

`DRIVER_NAME` defaults to `dra.example.com`; omit `POOL_NAME` to include all
pools for that driver. The example requests at most 100 pools and prints the
status as JSON on stdout, with lifecycle diagnostics on stderr.

## Snapshot semantics and schema boundary

Each completed request is immutable. Create a new request to observe later
allocation changes. `poolCount` is the total matching pool count; the returned
list can be truncated by `limit`. Missing counts stay `None`, especially when
`validationError` reports incomplete ResourceSlice publication. An empty or
failed result must not be presented as zero available devices.

The typed result uses the installed k8s-openapi 0.28 v1.36 schema. It includes
pool generations, slice count, total/allocated/available/unavailable device
counts, node name and validation errors. Newer partition/shareable summary
extensions and request grouping attributes are outside this adapter's typed
surface, alongside the deferred partitioning/consumable-capacity work.

## Native acceptance

From the repository root, with the existing fixture image available:

```sh
DRA_E2E_SKIP_BUILD=1 DRA_E2E_POOL_STATUS=1 \
  DRA_E2E_V137_CLUSTER=dra-validation-advanced-137 \
  ./dra/hack/validate-k8s-versions.sh
```

The opt-in test runs only on v1.37 and checks new snapshots before, during and
after an ordinary widget claim: total 1, allocated `0 -> 1 -> 0`, available
`1 -> 0 -> 1`, unavailable 0. It verifies CDI and matching-UID unpreparation,
deletes the explicitly authored claim before expecting capacity to return,
and verifies each query object is absent after the example exits. Add
`DRA_E2E_WORKLOAD=1` for shared-claim regression on the same advanced profile.
The serial matrix restores the standard v1.36 baseline.

References: pinned [v1.37 pool-status API definitions](https://github.com/kubernetes/kubernetes/blob/v1.37.0/staging/src/k8s.io/api/resource/v1alpha3/types.go)
and the [Kubernetes feature-gate reference](https://kubernetes.io/docs/reference/command-line-tools-reference/feature-gates/).

## Validation evidence — 2026-10-23

- Six focused client tests cover discovery/create/complete/delete, an unserved
  API, forbidden discovery, controller failure, a stalled-HTTP-read deadline,
  and replacement-UID refusal. Missing pool counts remained `None`.
- Full local CI passed: 153 workspace tests, 84 resource-health/example tests,
  formatting, strict Clippy, 23 VM-free matrix/fixture checks and five profile
  configuration tests. The unified facade's default Clippy and DRA-only build
  also passed after adding the public re-exports.
- The unchanged v1.36 baseline returned the explicit unserved-API message;
  no request object was created there. The isolated v1.37 profile retained
  its gates through another guarded IP change to `192.168.64.38`.
- Native snapshots returned total 1, allocated `0 -> 1 -> 0`, available
  `1 -> 0 -> 1`, unavailable 0, one complete slice, generation 1, and no
  validation error. Requests `dra-pool-status-zg7kk`, `dra-pool-status-s774k`
  and `dra-pool-status-856fk` were each verified absent after the example
  deleted them. Claim UID `9338fde4-b5c3-4190-b52b-5d104fdedbb2` prepared
  and unprepared before its explicit deletion released allocation.
- The same fixed-snapshot matrix passed core v1.36, core/extended-resource
  v1.37 and a second shared-claim lifecycle, UID
  `ee9733c8-07b7-4a8f-a747-6e5ec7f3eaf1`. The host-side pool client was built
  from this change. The unchanged validation driver used native image config
  `bd1c6614eaa9e3e6b7e9b192d4fba4a99b8493e2b69e650237e7dd2cb5e19e60`;
  no image rebuild was needed for the operator-side client.
- The complete matrix exited 0 and restored `dra-validation` v1.36.1 at
  `192.168.64.39`, Ready with real-RPC liveness. It was the sole running VM;
  all profiles had `rosetta=false`, the builder/advanced profile were stopped,
  and the DaemonSet retained `maxSurge: 0` without taint/binding controls.
