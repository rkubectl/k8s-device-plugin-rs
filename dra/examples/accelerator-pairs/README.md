# Topology-aware accelerator-pair DRA example

This is a runnable, validation-oriented DRA driver and workload example for
accelerator cards that must be allocated in connected pairs. It intentionally
advertises `pair-0-1` and `pair-2-3` as the only allocatable devices. Each pair
contains two member cards connected through `nvlink`; individual cards are not
published.

That representation is important. Requesting `count: 2` ordinary card devices
would only ensure that two cards match a selector. It would not prove that the
two selected cards have a direct interconnect. The DRA selector language
evaluates candidate devices individually, so this example makes the topology
constraint part of the driver's allocatable inventory: one claim requests one
pre-validated pair bundle.

When kubelet prepares an allocated bundle, the driver returns one CDI device
named `accelerator.example.com/pair=<pair>`. Its validation CDI spec supplies
these environment variables:

- `ACCELERATOR_PAIR`, such as `pair-0-1`;
- `ACCELERATOR_PAIR_MEMBERS`, such as `accelerator-0,accelerator-1`;
- `ACCELERATOR_INTERCONNECT`, set to `nvlink`.

For a real driver, replace the static `example_pairs()` inventory with the
vendor topology query, and replace those environment variables with CDI device
nodes, mounts, or vendor runtime configuration for **both** member cards. Keep
the pair bundle as the only schedulable unit unless the backend has a separate,
correctly shared allocation model. Do not also advertise its member cards as
ordinary exclusive devices: doing so permits overlapping allocations.

## Run it

The target cluster must support `resource.k8s.io/v1` DRA. This driver is a
fake hardware fixture; its DaemonSet mounts only kubelet DRA/CDI directories
and has validation-only RBAC.

```sh
container build -f dra/examples/accelerator-pairs/Dockerfile \
  -t accelerator-pair-dra-example .

# Make the image visible to the selected local cluster when required.
container k8s-versioned load-image --name <cluster> accelerator-pair-dra-example

kubectl --context <cluster> apply -k dra/examples/accelerator-pairs
kubectl --context <cluster> -n accelerator-pair-example \
  rollout status daemonset/accelerator-pair-dra-driver
kubectl --context <cluster> -n accelerator-pair-example \
  rollout status deployment/accelerator-pair-deployment
kubectl --context <cluster> -n accelerator-pair-example \
  rollout status statefulset/accelerator-pair-statefulset
```

The Deployment and StatefulSet both refer to the same
`ResourceClaimTemplate`. Kubernetes creates one claim per Pod, which means
each workload replica gets a separate pair allocation. Keep replicas at one
for the two-pair fixture when running both controllers on one node. On real
hardware, set replicas and node selection to the number and placement of
available pair bundles.

Inspect the scheduler's decision and the CDI result with:

```sh
kubectl --context <cluster> -n accelerator-pair-example get resourceclaims
kubectl --context <cluster> -n accelerator-pair-example logs \
  deployment/accelerator-pair-deployment
kubectl --context <cluster> -n accelerator-pair-example logs \
  statefulset/accelerator-pair-statefulset
```

Delete the example with `kubectl --context <cluster> delete -k
dra/examples/accelerator-pairs`.

## Production boundary

This is not production RBAC or a real accelerator backend. A production driver
must discover topology from the vendor library, remove an unhealthy or
topologically invalid pair atomically from its `ResourceSlice`, publish only
the pair's real hardware CDI edits, target nodes that actually contain the
cards, and use immutable images plus least privilege. Because `ResourceSlice`
is cluster-scoped, pair-driver RBAC also needs cluster-owner admission control
that limits slice mutations to this driver and its local node. Retain the
DaemonSet's `maxSurge: 0` until the target kubelet's DRA seamless-upgrade
behavior has been demonstrated.
