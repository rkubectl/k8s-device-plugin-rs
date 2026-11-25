# Choosing a Kubernetes patch version for DRA

Use the latest patch your Kubernetes distribution supports on the minor line
you deploy. As of 2026-11-25, the latest upstream releases are [Kubernetes
v1.36.6](https://github.com/kubernetes/kubernetes/releases/tag/v1.36.6) and
[v1.37.2](https://github.com/kubernetes/kubernetes/releases/tag/v1.37.2).
If your provider backports fixes, check its release notes for the affected
components. The versions below are the first upstream patches with each fix
on the indicated line.

## Kubernetes v1.36

| First patch | Workloads affected | Upstream fix |
| --- | --- | --- |
| v1.36.2 | `allocationMode: All` selects devices that consume shared counters. | The scheduler handles the request without panicking. [Kubernetes #138988](https://github.com/kubernetes/kubernetes/pull/138988) |
| v1.36.3 | Claims compete for shared counters or use the opt-in `DRADeviceTaintRules` feature. | Allocation backtracking correctly accounts for shared counters. The scheduler also handles concurrent `DeviceTaintRule` and `ResourceSlice` changes. [Kubernetes #140663](https://github.com/kubernetes/kubernetes/pull/140663) · [Kubernetes #139681](https://github.com/kubernetes/kubernetes/pull/139681) |
| v1.36.4 | Multiple drivers use the same pool name on a node, or kubelet retries a partially failed prepare. | The scheduler isolates shared counters by driver and pool. Kubelet avoids sending duplicate CDI device IDs to the container runtime on retry. [Kubernetes #140504](https://github.com/kubernetes/kubernetes/pull/140504) · [Kubernetes #140955](https://github.com/kubernetes/kubernetes/pull/140955) |
| v1.36.5 | Claims have oversized requests, including `firstAvailable` alternatives. | The scheduler rejects out-of-range counts and can fall through to a smaller alternative. [Kubernetes #141923](https://github.com/kubernetes/kubernetes/pull/141923) |
| v1.36.6 | A Pod uses multiple claims and an operator or controller observes its claim status. | The Pod's `.status.resourceClaimStatuses` stops alternating between partial lists. [Kubernetes #142021](https://github.com/kubernetes/kubernetes/pull/142021) |

## Kubernetes v1.37

| First patch | Workloads affected | Upstream fix |
| --- | --- | --- |
| v1.37.1 | A `DeviceTaintRule` omits `spec.deviceSelector`, which could taint every device on v1.37.0, or a claim has oversized `firstAvailable` alternatives. | An absent selector matches zero devices. Use `deviceSelector: {}` when the rule should apply to every device. The scheduler also rejects out-of-range counts and can fall through to a smaller alternative. [Kubernetes #141998](https://github.com/kubernetes/kubernetes/pull/141998) · [Kubernetes #141922](https://github.com/kubernetes/kubernetes/pull/141922) |
| v1.37.2 | Pods request DRA-backed extended resources and device operators use `NoExecute` taints. | `NoExecute` device taints also evict Pods using the affected devices through DRA-backed extended resources. [Kubernetes #142492](https://github.com/kubernetes/kubernetes/pull/142492) |

## Framework compatibility

Keep the `k8s-openapi` feature for the minor line your application targets
(`v1_36` or `v1_37`). The vendored kubelet protocol definitions already match
`kubernetes-1.37.2`. Retain the root Cargo `h2` override for pluginwatcher
registration on both lines. [DRA installation and compatibility](../dra/README.md#compatibility-and-dependency-override)

The local native arm64 smoke matrix targets v1.36.4 and v1.37.0. These are the
latest published `kindest/node` images for the two lines as of 2026-11-25.
When qualifying a deployment, exercise the affected workloads on the exact
patch and configuration you will run. [Kind v0.33.0 node images](https://github.com/kubernetes-sigs/kind/releases/tag/v0.33.0) · [DRA validation checklist](../dra/README.md#validation-checklist)
