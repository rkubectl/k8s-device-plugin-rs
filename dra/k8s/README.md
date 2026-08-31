# DRA validation manifests

These manifests deploy the `minimal_driver` example from this crate. Build
the image from the repository root, load or publish it for the target cluster,
then apply the kustomization:

```sh
docker build -f dra/Dockerfile -t k8s-device-plugin-dra-example .
kubectl apply -k dra/k8s
```

The target cluster must expose the `resource.k8s.io/v1` DRA API and have DRA
enabled before deployment. This driver publishes one static `widget-0` device
in the `widget-pool` pool on each node; it is for registration and API-path
validation only. It emits a harmless CDI environment marker for each static
device so an end-to-end consumer pod can verify DRA attachment, but it does
not provide a real hardware device.

The manifest is configured for the `dra.example.com` driver name and mounts
only that driver's kubelet plugin directory. If you override `DRIVER_NAME`,
update the matching `hostPath` and `mountPath` in `daemonset.yaml` too.

After the DaemonSet is ready, use [`../hack/e2e-smoke.sh`](../hack/e2e-smoke.sh)
to validate ResourceClaim allocation, kubelet preparation, CDI injection, and
unpreparation with a temporary consumer pod.

The `ClusterRole` is intentionally broader than a production driver needs:
it can read `ResourceClaim`s cluster-wide and manage `ResourceSlice`s
cluster-wide. Upstream's DRA example uses a Validating Admission Policy to
limit access to the local node. That policy is deliberately not copied into
this small validation fixture; do not deploy these manifests as production
RBAC.

`maxSurge: 0` is explicit because a phase-1 driver cannot assume a target
kubelet supports seamless node-plugin upgrades. Deleting the DaemonSet fully
removes the validation fixture:

```sh
kubectl delete -k dra/k8s
```
