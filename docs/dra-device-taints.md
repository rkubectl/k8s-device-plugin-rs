# DRA device taints

Device taints express an explicit device-use policy. A backend can prevent
new allocations during maintenance, evict users of a failed device, or
publish a diagnostic taint. Kubernetes evaluates claim tolerations and owns
Pod eviction. Driver health reports and `PoolDevice::health` remain separate
inputs; the framework does not automatically translate health into taints.

Driver-published taints are beta/default-on in the supported Kubernetes
v1.36 baseline and stable in v1.37. Administrator DeviceTaintRules are stable
in v1.37; this guide's administrator fixture targets that stable API.
Consult the [Kubernetes device-taint guide](https://kubernetes.io/docs/concepts/resource-management/dynamic-resource-allocation/device-taints/)
for control-plane behavior and cluster feature settings.

## Backend publication

Existing `ResourcePool::devices()` implementations and `PoolDevice` struct
literals continue to work. The default `ResourcePool::inventory()` converts
their devices into untainted `InventoryDevice`s. To use taints, override
`inventory()` and return devices and policy from one coherent backend
snapshot. `Arc<T>` delegates this override, including through trait objects.
The publisher calls `inventory()` once per reconciliation.

```rust
use std::collections::BTreeMap;
use k8s_device_plugin_core::{DeviceTaint, DeviceTaintEffect, InventoryDevice, PoolDevice};

let device = InventoryDevice::new(PoolDevice::new("widget-0")).with_taint(
    DeviceTaint::new("example.com/maintenance", DeviceTaintEffect::NoSchedule)
        .with_value("scheduled"),
);
let inventory = BTreeMap::from([("node-0".to_string(), vec![device])]);
```

An effect of `None` only communicates diagnostics. `NoSchedule` prevents
untolerated allocation/scheduling. `NoExecute` also causes Kubernetes to
evict existing consumers whose claims do not tolerate it. Configure
`tolerations` on each claim device request, rather than Pod node tolerations:

```yaml
spec:
  devices:
    requests:
      - name: widget
        exactly:
          deviceClassName: example-widget
          tolerations:
            - key: example.com/maintenance
              operator: Equal
              value: scheduled
              effect: NoSchedule
```

Leaving `DeviceTaint::time_added` unset lets the API server assign the time
on first publication. The publisher preserves that timestamp for an unchanged
key/value/effect, even during unrelated inventory updates. This keeps
NoExecute toleration deadlines stable and avoids advancing the pool generation
on every poll. A changed effect/value or a newly added taint gets a fresh
server default; a backend can explicitly set Unix seconds with
`with_time_added`. Preserve a backend-supplied time across snapshots too.

The publisher omits empty values/lists, compares taints in a stable order,
rejects duplicate key/effect pairs and more than 16 taints per device, and
uses at most 64 devices per slice for a tainted pool. Untainted pools retain
the ordinary 128-device limit. Adding/removing/changing a taint advances the
pool generation; repeating an unchanged snapshot converges without writes.
API-server validation still checks Kubernetes key/value syntax.

## Administrator rules

An operator can apply a `resource.k8s.io/v1` DeviceTaintRule on v1.37,
selecting a specific driver, pool, and device. The operator owns its lifecycle;
the driver's ServiceAccount does not need permission to manage these rules.
See the bounded selector in [the live fixture](../dra/hack/e2e-taints.sh).
Use a separate administrative credential to run that fixture. A rule with no
selector selects no devices; avoid broad selectors for maintenance of one
specific device.

## Native validation

Build the current image and deploy the ordinary validation driver first.
The example's opt-in `DRA_EXAMPLE_TAINT_EFFECT` accepts `None`, `NoSchedule`,
or `NoExecute`, publishing `dra.example.com/maintenance` on its fake devices.
Leaving the variable unset publishes no taint. This is a fake-device test
control, not a hardware failure detector.

```sh
KUBE_CONTEXT=<context> dra/hack/e2e-taints.sh

# Or run core/extended-resource and taint checks serially on both local profiles:
DRA_E2E_TAINTS=1 mise run dra-validate-k8s
```

The fixture requires the unmodified validation DaemonSet, a single widget
pool, and no existing objects named `dra-example-taints`. It refuses an
already-configured example taint. It verifies stable generation/timestamp
across the publisher's poll interval, untolerated NoSchedule rejection,
matching-toleration allocation/CDI, NoExecute eviction, and unpreparation for
the exact claim UID. On v1.37 it repeats the policy checks using an
administrator-owned rule. It deletes its objects/rule and restores the
example's unset taint control on success or failure. The matrix separately
restores the v1.36 baseline. VM-free tests exercise matrix opt-in routing,
failure restoration, and both kubectl fixture JSON output formats.

## Validation evidence (2026-10-23)

Beads `k8s-device-plugin-rs-qt7.6.3` passed full local CI: 138 workspace
tests, strict Clippy/format checks, 66 resource-health/example tests, and 15
VM-free orchestration/fixture-format checks. Tests include legacy struct
literal/default inventory compatibility, Arc delegation, all three effects,
invalid timestamps/duplicates/limits, 64-device splitting, timestamp/order
convergence, explicit retiming, effect changes, and taint removal.

The opt-in serial matrix exited 0 on native v1.36.1 and v1.37.0 with
`rosetta=false`. Both passed ordinary claim/CDI smoke and driver NoSchedule,
toleration, NoExecute eviction, exact-UID unprepare, timestamp/generation
stability, and cleanup. v1.37 also passed extended-resource smoke and the
same policy lifecycle through a bounded administrator rule. The fixture was
built as linux/arm64 with image config
`sha256:973348552593c0dc60042a35676c4415e8950378a99b354f8b2aff941d1c33fb`.
The final v1.36 profile was Ready at `192.168.64.29`, with a successful driver
RPC probe and no published taints; all other VMs, including the builder,
were stopped. The first fixture attempt failed before policy mutation because
kubectl emitted a stream of objects; both stream and List forms now have
regression coverage, and the corrected full matrix passed.
