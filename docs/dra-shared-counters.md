# Partitionable devices and shared counters

Shared counters let the Kubernetes scheduler allocate overlapping views of
hardware without assigning the same capacity twice. For example, publish two
individual accelerator cards and one complete pair: each card consumes one
unit of a shared `cards: 2` counter, and the pair consumes both units. Taking
a card makes that pair unavailable; taking the pair makes both cards unavailable.

This adapter uses stable `resource.k8s.io/v1` types in both supported schemas,
Kubernetes v1.36 and v1.37. `DRAPartitionableDevices` is beta and enabled by
default on both versions. Operators that disable that gate must not enable
this adapter. Selecting a Rust schema does not enable server feature gates.

## Backend contract

Override `ResourcePool::snapshot()` to return a
`BTreeMap<PoolName, PoolInventory>`. Read devices, policies, and counter sets
from one coherent backend state. The publisher calls this method once per
reconciliation; `Arc<dyn ResourcePool>` preserves the override.

Build each pool with DRA core types, also exported by the facade crate:

```rust
use k8s_device_plugin::CounterSet;
use k8s_device_plugin::DeviceCounterConsumption;
use k8s_device_plugin::InventoryDevice;
use k8s_device_plugin::PoolDevice;
use k8s_device_plugin::PoolInventory;
use k8s_device_plugin::Quantity;

let devices = [("card-0", "card", 1), ("card-1", "card", 1), ("bridge-a", "pair", 2)]
    .into_iter()
    .map(|(name, kind, count)| {
        InventoryDevice::new(PoolDevice::new(name.parse().unwrap()).attribute("type", kind))
            .with_consumes_counters(
                DeviceCounterConsumption::new("pair-a".parse().unwrap()).counter("cards".parse().unwrap(), Quantity::from(count)),
            )
    })
    .collect();
let pool = PoolInventory::new(devices)
    .with_counter_set(CounterSet::new("pair-a".parse().unwrap()).counter("cards".parse().unwrap(), Quantity::from(2)));
```

Counters and ordinary device capacities share the framework's exact
`Quantity` type. Use `Quantity::from(2)` for integers or
`"0.5".parse::<Quantity>()?` for fractions, SI units and exponent notation.
There is no floating-point conversion. The publisher emits Kubernetes wire
forms such as `0.5 -> 500m` and `1.5Gi -> 1536Mi`; equivalent server spellings
compare numerically, so a round trip converges. See the
[quantity migration and dependency review](plans/kubernetes-quantities.md).
Names must be DNS labels. Each counter map must
contain 1–32 entries, and each device may reference at most two distinct
counter sets. Every referenced set and counter must exist in the same pool.
Invalid counter names fail checked construction. Invalid counter sizes and
references return the publisher's 422 `invalid_pool_error` before mutation. Counter definitions
are scoped to a pool, including across multiple counter slices.

The snapshot extension preserves existing backend implementations; update
integer Quantity constructors as described in the migration guide.
The default `snapshot()` delegates to
`inventory()`, which defaults to `devices()`. `PoolDevice` identity fields require checked names; taints and binding metadata
remain supported. Use `snapshot()` instead of
separate counter and device queries to avoid publishing incompatible reads.

## Publication and allocation

Kubernetes forbids devices and shared counters in the same ResourceSlice.
The publisher emits device slices followed by counters-only slices, with at
most eight counter sets per counter slice. Pools containing consuming or
tainted devices use device chunks of at most 64; ordinary pools retain the
128-device limit and their existing wire output.

Kubernetes v1.37 compatibility groups cannot be expressed by this shared-counter
adapter. The schema-dependent backend model and conversion are deferred to
`k8s-device-plugin-rs-qt7.6.10`.

Every slice has the same pool generation and total `resourceSliceCount`,
including counter slices. A counter or consumption change advances the
generation once. Reordering devices, counter sets, or consumption entries
does not trigger a write. All desired slices are reconciled before stale
slices are removed with UID/resourceVersion preconditions. An empty pool is
removed; a pool containing only counter sets remains published.

Use two `DeviceClass` selectors to expose card and pair views, for example:

```yaml
spec:
  selectors:
    - cel:
        expression: 'device.driver == "dra.example.com" && device.attributes["dra.example.com"].type == "card"'
```

The pair class selects `"pair"` instead. On Kubernetes v1.37, each class can
also set its own `extendedResourceName`; see the
[extended-resource guide](dra-extended-resources.md). The scheduler owns
counter accounting and releases capacity when allocations end. No share IDs,
consumable device capacity, or changes to preparation/CDI are required.
The allocated device name still reaches `ClaimPreparer`; a real pair backend
returns the CDI artifacts for both member cards.

## Validation fixture

The existing `minimal_driver` supports `DRA_EXAMPLE_SHARED_COUNTERS=1`. This
replaces its configured widget inventory with four fake cards and two pair
views, backed by two shared counter sets. Each pair has one counter unit;
each card consumes `0.5` and each pair consumes `1`. The fixture also publishes
a `0.5` device capacity. It retains the ordinary status,
preparation, and CDI marker paths. See
[`partitionable_inventory`](../dra/examples/minimal_driver.rs) for the complete
typed inventory and `ResourcePool::snapshot` implementation.

With the validation DaemonSet running on a single-node kind-compatible
cluster, run:

```sh
KUBE_CONTEXT=<context> dra/hack/e2e-shared-counters.sh
```

The fixture refuses existing objects, active driver allocations, and
conflicting driver overrides. It checks separate counter/device slices and
an unchanged poll, then exercises two pair allocations plus a waiting third,
a card followed by a pair on the other hardware, and a pair excluding a
three-card request while a two-card request succeeds. Releasing consumers
allows waiting pods to start through the existing CDI path. Cleanup removes
owned claims, pods, classes, and the temporary driver override.

Include it in the serial native v1.36/v1.37 matrix with:

```sh
DRA_E2E_SHARED_COUNTERS=1 dra/hack/validate-k8s-versions.sh
```

Keep `maxSurge: 0` and native arm64 with `rosetta=false`. This is framework
acceptance with fake devices; hardware discovery and production deployment
qualification remain driver responsibilities.

The limits and mutual exclusion rule follow Kubernetes
[v1.36.4 API types](https://github.com/kubernetes/kubernetes/blob/v1.36.4/staging/src/k8s.io/api/resource/v1/types.go)
and [validation](https://github.com/kubernetes/kubernetes/blob/v1.36.4/pkg/apis/resource/validation/validation.go).
Local tracking: `qt7.6.9` and `ls9`; consumer request:
[GitHub #2](https://github.com/rkubectl/k8s-device-plugin-rs/issues/2).

## Acceptance — 2026-11-06

The serial native run passed on Kubernetes v1.36.1 and v1.37.0 using the
existing `dra-upgrade-348-136` and `dra-upgrade-348-137` profiles. Both passed
the ordinary CDI and FirstAvailable smoke tests, separate shared-counter
slices with an unchanged 35-second poll, two pairs plus a waiting third,
card/pair exclusion, pair versus two/three cards, release/retry and fixture
cleanup. The v1.37 extended-resource generated-claim lifecycle also passed
after restoration of the ordinary widget backend.

The linux/arm64 image built with Rust 1.98 was
`docker.io/library/k8s-device-plugin-dra-example:shared-counters-20261106`,
with config digest
`sha256:276d51fd9553a53f3cc607c20b01fd40617d8d91c0c6255b21b8812c9cad72ed`;
that digest was confirmed on the running v1.36 driver Pod. Both profiles
imported the same tag. The run retained `maxSurge: 0`, used a temporary
kubeconfig, and restored both profiles and the builder to their initial
stopped state. Every VM retained arm64 and `rosetta=false`.

Local proof passed full `mise run ci` (213 workspace tests, resource-health
targets/examples, 29 matrix tests, 14 multi-node tests, five profile tests,
and independent consumer-selected v1.36/v1.37 schemas), additional v1.36
core/DRA all-target/all-feature tests, strict all-feature rustdoc, and strict
Clippy on both schemas. Final API review preserved the existing exhaustive
`ValidationError` enum; counter-name validation and all eight counter
regressions passed again on both schemas after that compatibility adjustment.

### Exact-quantity follow-up — 2026-11-06

Task `56b` replaces the integer-only quantity with the standalone
`k8s-quantity` crate and optional OpenAPI conversions. The updated fixture
passed native v1.36.1/v1.37.0 acceptance with `0.5` counter consumption and
device capacity, including unchanged polls, exclusion, release/retry and
CDI. The v1.37 extended-resource regression and cleanup also passed. See the
[quantity guide's acceptance record](plans/kubernetes-quantities.md#acceptance--2026-11-06)
for the final image digest, conformance evidence and API migration.
