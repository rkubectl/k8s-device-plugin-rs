#!/usr/bin/env bash
# Fresh-cluster-only native widget lifecycle acceptance. See docs/dra-multi-node.md.
set -euo pipefail
umask 077

CONTAINER=${CONTAINER:-container}
KUBECTL=${KUBECTL:-kubectl}
JQ=${JQ:-jq}
CLUSTER=${DRA_MULTI_NODE_CLUSTER:-dra-validation-mok-136}
BASELINE=${DRA_MULTI_NODE_BASELINE_IMAGE:?select a local baseline image tag}
UPGRADE=${DRA_MULTI_NODE_UPGRADE_IMAGE:?select a local upgrade image tag}
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
NAMESPACE=k8s-device-plugin-dra-example
APP=k8s-device-plugin-dra-example
DRIVER=dra.example.com
PROBE=/usr/local/bin/k8s-device-plugin-dra-liveness
NODE_A="$CLUSTER-worker-1"
NODE_B="$CLUSTER-worker-2"
NODES=("$CLUSTER" "$NODE_A" "$NODE_B")
CREATED=0
API_READY=0
WATCH_PID=
READER_PID=
WORK_DIR=

die() { printf '%s\n' "$*" >&2; exit 1; }
k() { "$KUBECTL" --context "$CLUSTER" --request-timeout=15s "$@"; }
retry() {
    local attempt
    for attempt in {1..30}; do
        if "$@"; then return 0; fi
        sleep 2
    done
    printf 'bounded check failed: %s\n' "$*" >&2
    return 1
}
owned_inventory() {
    "$CONTAINER" list --all --format json |
        "$JQ" --arg cluster "$CLUSTER" '[.[] | select(.id == $cluster or (.id | startswith($cluster + "-worker-")))]'
}
stop_watch() {
    if [[ -n "$WATCH_PID" ]]; then
        kill "$WATCH_PID" 2>/dev/null || true
        wait "$WATCH_PID" 2>/dev/null || true
        WATCH_PID=
    fi
    if [[ -n "$READER_PID" ]]; then
        wait "$READER_PID" 2>/dev/null || true
        READER_PID=
    fi
}
diagnostics() {
    local pod
    k get nodes -o json > "$ARTIFACTS/nodes-final.json" || true
    k -n "$NAMESPACE" get pods,resourceclaims -o json > "$ARTIFACTS/workloads-final.json" || true
    k get resourceslices -o json > "$ARTIFACTS/slices-final.json" || true
    k -n "$NAMESPACE" get events -o json > "$ARTIFACTS/events.json" || true
    for pod in $(k -n "$NAMESPACE" get pods -l "app=$APP" -o name 2>/dev/null); do
        k -n "$NAMESPACE" logs "$pod" --tail=200 > "$ARTIFACTS/${pod#pod/}.log" 2>&1 || true
    done
}
cleanup() {
    local result=$? cleanup_result=0 remaining
    trap - EXIT INT TERM
    set +e
    stop_watch
    if ((API_READY)); then diagnostics; fi
    if ((CREATED)); then
        remaining=$(owned_inventory)
        if [[ $? != 0 ]]; then
            cleanup_result=1
        elif [[ $("$JQ" 'length' <<< "$remaining") != 0 ]]; then
            "$CONTAINER" k8s-versioned delete --name "$CLUSTER" || cleanup_result=$?
        fi
        remaining=$(owned_inventory)
        if [[ $? != 0 ]] || ! "$JQ" -e 'length == 0' <<< "$remaining" >/dev/null; then
            printf 'owned cluster cleanup failed: %s\n' "$CLUSTER" >&2
            cleanup_result=1
        fi
    fi
    "$CONTAINER" list --all --format json |
        "$JQ" '[.[] | {id,state:.status.state,arch:.configuration.platform.architecture,rosetta:.configuration.rosetta}]' \
        > "$ARTIFACTS/inventory-after.json" || cleanup_result=1
    "$JQ" -n --argjson result "$result" --argjson cleanup "$cleanup_result" \
        '{result:$result,cleanup:$cleanup}' > "$ARTIFACTS/result.json"
    if [[ -n "$WORK_DIR" ]]; then rm -rf -- "$WORK_DIR"; fi
    if ((result == 0)); then result=$cleanup_result; fi
    printf 'Evidence: %s\n' "$ARTIFACTS"
    exit "$result"
}

for command in "$CONTAINER" "$KUBECTL" "$JQ"; do command -v "$command" >/dev/null; done
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || die 'requires native arm64 macOS'
[[ "$CLUSTER" =~ ^dra-validation-mok-[a-z0-9-]+$ && ${#CLUSTER} -le 40 ]] ||
    die 'use a dedicated dra-validation-mok-* name of at most 40 characters'
inventory=$("$CONTAINER" list --all --format json)
"$JQ" -e --arg cluster "$CLUSTER" '
    all(.[]; .id != $cluster and (.id | startswith($cluster + "-worker-") | not)) and
    all(.[]; .status.state != "running" or
        ((.configuration.labels["com.apple.container.plugin"] // "") |
         test("^(cluster|k8s|rosettaless-k8s)$") | not))
' <<< "$inventory" >/dev/null || die 'existing target or running Kubernetes profile; refusing mutation'

if [[ -n ${DRA_MULTI_NODE_ARTIFACTS:-} ]]; then
    ARTIFACTS=$DRA_MULTI_NODE_ARTIFACTS
    [[ ! -e "$ARTIFACTS" ]] || die 'artifact path must not already exist'
    mkdir -m 700 -- "$ARTIFACTS"
else
    ARTIFACTS=$(mktemp -d)
fi
"$JQ" '[.[] | {id,state:.status.state,arch:.configuration.platform.architecture,rosetta:.configuration.rosetta}]' \
    <<< "$inventory" > "$ARTIFACTS/inventory-before.json"
"$KUBECTL" config current-context > "$ARTIFACTS/original-context.txt" 2> "$ARTIFACTS/original-context-error.txt" || true
for slot in baseline upgrade; do
    if [[ "$slot" == baseline ]]; then image=$BASELINE; else image=$UPGRADE; fi
    [[ "$image" == */* && ${image##*/} == *:* && "$image" != *@* ]] ||
        die 'supply fully qualified local image tags; the runner resolves immutable digests'
    "$CONTAINER" image inspect "$image" |
        "$JQ" -e --arg image "$image" '
            if length == 1 and .[0].configuration.name == $image and
                (.[0].variants | length) == 1 and
                .[0].variants[0].platform.os == "linux" and
                .[0].variants[0].platform.architecture == "arm64"
            then .[0] | {name:.configuration.name,index:.configuration.descriptor.digest,
                manifest:.variants[0].digest,platform:.variants[0].platform}
            else error("image must have exactly one native linux/arm64 variant") end
        ' > "$ARTIFACTS/$slot-image.json"
done
[[ $("$JQ" -r .index "$ARTIFACTS/baseline-image.json") != \
   $("$JQ" -r .index "$ARTIFACTS/upgrade-image.json") ]] || die 'images must have distinct immutable digests'

WORK_DIR=$(mktemp -d)
export KUBECONFIG="$WORK_DIR/kubeconfig"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
CREATED=1
printf 'Creating isolated native v1.36.1 cluster %s with two workers\n' "$CLUSTER"
"$CONTAINER" k8s-versioned create --name "$CLUSTER" --kubernetes-version v1.36.1 --workers 2 --cpus 2 --memory 2G
owned_inventory > "$ARTIFACTS/native-nodes.json"
"$JQ" -e --argjson names "$("$JQ" -nc --arg c "$CLUSTER" --arg a "$NODE_A" --arg b "$NODE_B" '[$c,$a,$b] | sort')" '
    (map(.id) | sort) == $names and
    all(.[]; .status.state == "running" and .configuration.rosetta == false and
        .configuration.platform.architecture == "arm64")
' "$ARTIFACTS/native-nodes.json" >/dev/null || die 'native node identity/architecture check failed'
[[ $("$KUBECTL" config current-context) == "$CLUSTER" ]] || die 'temporary kubeconfig was not selected'
k version -o json > "$ARTIFACTS/version.json"
"$JQ" -e '.serverVersion.gitVersion == "v1.36.1"' "$ARTIFACTS/version.json" >/dev/null
k wait --for=condition=Ready nodes --all --timeout=180s
k get nodes -o json > "$ARTIFACTS/nodes-before.json"
"$JQ" -e --arg a "$NODE_A" --arg b "$NODE_B" '
    (.items | length) == 3 and
    all(.items[]; .status.nodeInfo.architecture == "arm64" and .status.nodeInfo.kubeletVersion == "v1.36.1") and
    ([.items[] | select(.metadata.name == $a or .metadata.name == $b)] | length) == 2 and
    all(.items[] | select(.metadata.name == $a or .metadata.name == $b);
        (.spec.unschedulable // false) == false and
        (.metadata.labels | has("node-role.kubernetes.io/control-plane") | not))
' "$ARTIFACTS/nodes-before.json" >/dev/null || die 'requires two initially schedulable worker nodes'
API_READY=1

load_image() {
    local image=$1 slot=$2 node inspected digest target runtime_id first_digest= first_id= repository
    repository=${image%:*}
    "$CONTAINER" k8s-versioned load-image --name "$CLUSTER" "$image" --platform linux/arm64
    for node in "${NODES[@]}"; do
        # ctr import creates a tag without a CRI repoDigest. Check the imported
        # target against the host OCI identity before adding its digest alias.
        "$CONTAINER" exec "$node" ctr --namespace k8s.io images list "name==$image" \
            > "$ARTIFACTS/$slot-$node-image-target.txt"
        target=$(awk -v image="$image" '$1 == image {count++; digest=$3}
            END {if (count != 1) exit 1; print digest}' "$ARTIFACTS/$slot-$node-image-target.txt")
        "$JQ" -e --arg digest "$target" '
            .index == $digest or .manifest == $digest
        ' "$ARTIFACTS/$slot-image.json" >/dev/null || die 'imported image differs from inspected local image'
        digest="$repository@$target"
        "$CONTAINER" exec "$node" ctr --namespace k8s.io images tag "$image" "$digest"
        inspected=$("$CONTAINER" exec "$node" crictl inspecti -o json "$digest")
        printf '%s\n' "$inspected" > "$ARTIFACTS/$slot-$node-image-inspect.json"
        "$JQ" -e --arg digest "$digest" '.status.repoDigests | index($digest) != null' \
            <<< "$inspected" >/dev/null || die 'imported digest is not available through CRI'
        runtime_id=$("$JQ" -er '.status.id' <<< "$inspected")
        if [[ -n "$first_digest" ]]; then
            [[ "$digest" == "$first_digest" && "$runtime_id" == "$first_id" ]] || die 'image identity differs between nodes'
        fi
        first_digest=$digest
        first_id=$runtime_id
        "$JQ" -nc --arg node "$node" --arg image "$digest" --arg id "$runtime_id" \
            '{node:$node,image:$image,id:$id}' >> "$ARTIFACTS/$slot-runtime.jsonl"
    done
}
load_image "$BASELINE" baseline
load_image "$UPGRADE" upgrade
BASELINE_DIGEST=$("$JQ" -r .image "$ARTIFACTS/baseline-runtime.jsonl" | head -1)
BASELINE_ID=$("$JQ" -r .id "$ARTIFACTS/baseline-runtime.jsonl" | head -1)
UPGRADE_DIGEST=$("$JQ" -r .image "$ARTIFACTS/upgrade-runtime.jsonl" | head -1)
UPGRADE_ID=$("$JQ" -r .id "$ARTIFACTS/upgrade-runtime.jsonl" | head -1)
[[ "$BASELINE_DIGEST" != "$UPGRADE_DIGEST" && "$BASELINE_ID" != "$UPGRADE_ID" ]] || die 'upgrade must change the runtime image'
k label nodes "$NODE_A" "$NODE_B" "dra.example.com/mok=$CLUSTER"
k create --dry-run=client -k "$SCRIPT_DIR/../k8s" -o json |
    "$JQ" -s '{apiVersion:"v1",kind:"List",items:.}' > "$WORK_DIR/deployment.json"
"$JQ" --arg image "$BASELINE_DIGEST" --arg cluster "$CLUSTER" '
    .items |= map(
        .metadata.labels["dra.example.com/mok"] = $cluster |
        if .kind == "DaemonSet" then
            .spec.template.spec.nodeSelector = {"dra.example.com/mok":$cluster} |
            .spec.updateStrategy.rollingUpdate = {maxSurge:0,maxUnavailable:1} |
            .spec.template.spec.containers[0].image = $image |
            .spec.template.spec.containers[0].imagePullPolicy = "IfNotPresent" |
            .spec.template.spec.containers[0].env = [
                {name:"NODE_NAME",valueFrom:{fieldRef:{fieldPath:"spec.nodeName"}}},
                {name:"DRIVER_NAME",value:"dra.example.com"},
                {name:"POOL_NAME",value:"$(NODE_NAME)"},
                {name:"DEVICE_NAMES",value:"$(NODE_NAME)"},
                {name:"DRA_EXAMPLE_DEVICE_METADATA",value:"1"},
                {name:"RUST_LOG",value:"info"}
            ]
        else . end)
' "$WORK_DIR/deployment.json" > "$WORK_DIR/rendered.json"
cp "$WORK_DIR/rendered.json" "$ARTIFACTS/deployment.json"
k apply -f "$WORK_DIR/rendered.json"

pod_on() {
    local node=$1
    k -n "$NAMESPACE" get pods -l "app=$APP" -o json |
        "$JQ" -er --arg node "$node" '
            [.items[] | select(.spec.nodeName == $node and .metadata.deletionTimestamp == null)] |
            if length == 1 then .[0].metadata.name else error("expected one live driver per node") end'
}
ready() {
    local image=$1 node pod owner
    k -n "$NAMESPACE" get daemonset "$APP" -o json > "$WORK_DIR/ds.json" || return 1
    "$JQ" -e '.status.desiredNumberScheduled == 2 and .status.currentNumberScheduled == 2 and
        .status.updatedNumberScheduled == 2 and .status.numberReady == 2 and
        .spec.updateStrategy.rollingUpdate.maxSurge == 0' "$WORK_DIR/ds.json" >/dev/null || return 1
    k -n "$NAMESPACE" get pods -l "app=$APP" -o json > "$WORK_DIR/pods.json" || return 1
    "$JQ" -e --arg image "$image" --arg a "$NODE_A" --arg b "$NODE_B" '
        [.items[] | select(.metadata.deletionTimestamp == null)] |
        length == 2 and (map(.spec.nodeName) | sort) == ([$a,$b] | sort) and
        all(.[]; .spec.containers[0].image == $image and
            any(.status.containerStatuses[]; .name == "dra-driver" and .ready and .imageID == $image))
    ' "$WORK_DIR/pods.json" >/dev/null || return 1
    k get resourceslices --field-selector="spec.driver=$DRIVER" -o json > "$WORK_DIR/slices.json" || return 1
    "$JQ" -e '.items | length == 2' "$WORK_DIR/slices.json" >/dev/null || return 1
    for node in "$NODE_A" "$NODE_B"; do
        owner=$("$JQ" -er --arg node "$node" '.items[] | select(.metadata.name == $node) | .metadata.uid' "$ARTIFACTS/nodes-before.json") || return 1
        "$JQ" -e --arg node "$node" --arg uid "$owner" --arg driver "$DRIVER" '
            [.items[] | select(.spec.nodeName == $node)] |
            length == 1 and all(.[]; .spec.driver == $driver and .spec.pool.name == $node and
                .spec.pool.resourceSliceCount == 1 and .spec.pool.generation > 0 and
                (.spec.devices | length) == 1 and .spec.devices[0].name == $node and
                any(.metadata.ownerReferences[]; .kind == "Node" and .name == $node and .uid == $uid and .controller))
        ' "$WORK_DIR/slices.json" >/dev/null || return 1
        pod=$(pod_on "$node") || return 1
        k -n "$NAMESPACE" exec "$pod" -- "$PROBE" || return 1
    done
}
wait_ready() {
    k -n "$NAMESPACE" rollout status "daemonset/$APP" --timeout=180s
    retry ready "$1"
}
snapshot() {
    cp "$WORK_DIR/pods.json" "$ARTIFACTS/$1-pods.json"
    cp "$WORK_DIR/slices.json" "$ARTIFACTS/$1-slices.json"
}
wait_ready "$BASELINE_DIGEST"
snapshot baseline

consumer_fixture() {
    local suffix=$1 node=$2
    "$JQ" -n --arg ns "$NAMESPACE" --arg node "$node" --arg suffix "$suffix" '
        {apiVersion:"v1",kind:"List",items:[
            {apiVersion:"resource.k8s.io/v1",kind:"DeviceClass",metadata:{name:"dra-mok-widget"},
                spec:{selectors:[{cel:{expression:"device.driver == \"dra.example.com\""}}]}},
            {apiVersion:"resource.k8s.io/v1",kind:"ResourceClaim",
                metadata:{name:("dra-mok-claim-"+$suffix),namespace:$ns},
                spec:{devices:{requests:[{name:"widget",exactly:{deviceClassName:"dra-mok-widget"}}]}}},
            {apiVersion:"v1",kind:"Pod",metadata:{name:("dra-mok-consumer-"+$suffix),namespace:$ns},
                spec:{restartPolicy:"Never",terminationGracePeriodSeconds:1,
                    affinity:{nodeAffinity:{requiredDuringSchedulingIgnoredDuringExecution:{nodeSelectorTerms:[
                        {matchFields:[{key:"metadata.name",operator:"In",values:[$node]}]}]}}},
                    containers:[{name:"consumer",image:"busybox:1.37",imagePullPolicy:"IfNotPresent",
                        env:[{name:"EXPECTED_DEVICE",value:$node}],
                        command:["sh","-ec","test \"$DRA_E2E_DEVICE\" = \"$EXPECTED_DEVICE\"; echo \"DRA_MOK_DEVICE=$DRA_E2E_DEVICE\"; sleep 3600"],
                        resources:{claims:[{name:"widget"}]}}],
                    resourceClaims:[{name:"widget",resourceClaimName:("dra-mok-claim-"+$suffix)}]}}
        ]}'
}
consumer_fixture a "$NODE_A" > "$WORK_DIR/consumer-a.json"
consumer_fixture b "$NODE_B" > "$WORK_DIR/consumer-b.json"
k apply -f "$WORK_DIR/consumer-a.json"
k apply -f "$WORK_DIR/consumer-b.json"
consumer_ok() {
    local suffix=$1 node=$2 saved="$ARTIFACTS/consumer-$1.json"
    k -n "$NAMESPACE" get pod "dra-mok-consumer-$suffix" -o json > "$WORK_DIR/consumer.json" || return 1
    "$JQ" -e --arg node "$node" '.spec.nodeName == $node and .status.phase == "Running" and
        all(.status.containerStatuses[]; .ready and .restartCount == 0)' "$WORK_DIR/consumer.json" >/dev/null || return 1
    if [[ -f "$saved" ]]; then
        "$JQ" -e --slurpfile saved "$saved" '
            .metadata.uid == $saved[0].metadata.uid and
            .status.containerStatuses[0].containerID == $saved[0].status.containerStatuses[0].containerID
        ' "$WORK_DIR/consumer.json" >/dev/null || return 1
    fi
    k -n "$NAMESPACE" exec "dra-mok-consumer-$suffix" -- sh -ec 'test "$DRA_E2E_DEVICE" = "$EXPECTED_DEVICE"' || return 1
    k -n "$NAMESPACE" get resourceclaim "dra-mok-claim-$suffix" -o json > "$WORK_DIR/claim.json" || return 1
    "$JQ" -e --arg node "$node" --arg driver "$DRIVER" '
        .status.allocation.devices.results | length == 1 and
        all(.[]; .driver == $driver and .pool == $node and .device == $node and .request == "widget")
    ' "$WORK_DIR/claim.json" >/dev/null || return 1
    k -n "$NAMESPACE" exec "dra-mok-consumer-$suffix" -- cat \
        "/var/run/kubernetes.io/dra-device-attributes/resourceclaims/dra-mok-claim-$suffix/widget/$DRIVER-metadata.json" |
        "$JQ" -e --arg node "$node" '.requests[0].devices | length == 1 and
            all(.[]; .pool == $node and .name == $node)' >/dev/null || return 1
    if [[ ! -f "$saved" ]]; then cp "$WORK_DIR/consumer.json" "$saved"; fi
}
for suffix in a b; do k -n "$NAMESPACE" wait --for=condition=Ready "pod/dra-mok-consumer-$suffix" --timeout=180s; done
retry consumer_ok a "$NODE_A"
cp "$WORK_DIR/claim.json" "$ARTIFACTS/claim-a.json"
retry consumer_ok b "$NODE_B"
cp "$WORK_DIR/claim.json" "$ARTIFACTS/claim-b.json"

claims_metric() {
    local node=$1 expected=$2 metrics
    metrics=$(k get --raw "/api/v1/nodes/$node/proxy/metrics") || return 1
    printf '%s\n' "$metrics" > "$ARTIFACTS/$node-claims-$expected.prom"
    # v1.36.1 drops a driver's series after its last prepared claim. Its
    # always-present <any> series proves zero across every driver on this node.
    awk -v expected="$expected" '
        /^dra_resource_claims_in_use\{/ {
            if (index($0, "driver_name=\"<any>\"")) {seen=1; total+=$NF}
            if (index($0, "driver_name=\"dra.example.com\"")) {driver_seen=1; driver_total+=$NF}
        }
        END {exit !(seen && total == expected &&
            (expected == 0 ? driver_total == 0 : driver_seen && driver_total == expected))}
    ' <<< "$metrics"
}
retry claims_metric "$NODE_A" 1
retry claims_metric "$NODE_B" 1
claim_clean() {
    local suffix=$1 node=$2 claim_uid=$3 pod logs
    pod=$(pod_on "$node") || return 1
    logs=$(k -n "$NAMESPACE" logs "$pod" --tail=200) || return 1
    grep -F "$claim_uid" <<< "$logs" | grep -F 'unpreparing DRA claim' > "$ARTIFACTS/unprepare-$suffix.log" || return 1
    claims_metric "$node" 0 || return 1
    "$CONTAINER" exec "$node" sh -ec '
        test ! -e "/var/lib/kubelet/plugins/dra.example.com/dra-device-metadata/$1"
        test ! -e "/var/run/cdi/dra.example.com_metadata_${1}_widget.json"
    ' -- "$claim_uid" || return 1
}
release_claim() {
    local suffix=$1 node=$2 claim_uid
    claim_uid=$("$JQ" -er .metadata.uid "$ARTIFACTS/claim-$suffix.json")
    k -n "$NAMESPACE" delete "pod/dra-mok-consumer-$suffix" --wait=true --timeout=180s
    retry claim_clean "$suffix" "$node" "$claim_uid"
    cp "$ARTIFACTS/$node-claims-0.prom" "$ARTIFACTS/release-$suffix.prom"
    k -n "$NAMESPACE" delete "resourceclaim/dra-mok-claim-$suffix" --wait=true --timeout=180s
}
printf 'Per-node concurrent allocation, CDI and metadata isolation passed\n'
release_claim b "$NODE_B"
consumer_ok a "$NODE_A"
k cordon "$NODE_B"
k drain "$NODE_B" --ignore-daemonsets --timeout=180s
k uncordon "$NODE_B"
k wait --for=condition=Ready "node/$NODE_B" --timeout=180s
wait_ready "$BASELINE_DIGEST"
consumer_ok a "$NODE_A"
snapshot post-maintenance
printf 'Safe maintenance and consumer continuity passed\n'

k -n "$NAMESPACE" get pods -l "app=$APP" -o json > "$ARTIFACTS/pre-upgrade-pods.json"
resource_version=$("$JQ" -er '.metadata.resourceVersion | @uri' "$ARTIFACTS/pre-upgrade-pods.json")
mkfifo "$WORK_DIR/pod-watch"
"$KUBECTL" --context "$CLUSTER" --request-timeout=190s get --raw \
    "/api/v1/namespaces/$NAMESPACE/pods?watch=true&resourceVersion=$resource_version&labelSelector=app%3D$APP&timeoutSeconds=180" \
    > "$WORK_DIR/pod-watch" &
WATCH_PID=$!
"$JQ" --unbuffered -c '.' < "$WORK_DIR/pod-watch" > "$ARTIFACTS/upgrade-watch.jsonl" &
READER_PID=$!
k -n "$NAMESPACE" set image "daemonset/$APP" "dra-driver=$UPGRADE_DIGEST"
wait_ready "$UPGRADE_DIGEST"
snapshot upgraded
upgrade_order() {
    "$JQ" -s -e --slurpfile old "$ARTIFACTS/pre-upgrade-pods.json" --slurpfile new "$ARTIFACTS/upgraded-pods.json" '
        . as $events |
        [$old[0].items[] | . as $before |
            ($new[0].items[] | select(.spec.nodeName == $before.spec.nodeName)) as $after |
            ([$events | to_entries[] | select(.value.type == "DELETED" and
                .value.object.metadata.uid == $before.metadata.uid)] | first) as $deleted |
            ([$events | to_entries[] | select(.value.type == "ADDED" and
                .value.object.metadata.uid == $after.metadata.uid)] | first) as $added |
            ([$events | to_entries[] | select(.value.object.metadata.uid == $before.metadata.uid and
                .value.object.status.containerStatuses[0].state.terminated.finishedAt != null)] | first) as $terminated |
            ([$events | to_entries[] | select(.value.object.metadata.uid == $after.metadata.uid and
                .value.object.status.containerStatuses[0].state.running.startedAt != null)] | first) as $running |
            {node:$before.spec.nodeName,old:$before.metadata.uid,new:$after.metadata.uid,
                deletedEvent:$deleted.key,addedEvent:$added.key,
                terminatedEvent:$terminated.key,runningEvent:$running.key,
                finished:$terminated.value.object.status.containerStatuses[0].state.terminated.finishedAt,
                started:$after.status.containerStatuses[0].state.running.startedAt}
        ] |
        if length == 2 and all(.[]; .old != .new and .deletedEvent != null and .addedEvent != null and
            .terminatedEvent != null and .runningEvent != null and .terminatedEvent < .runningEvent and
            .finished != null and .started != null and .finished <= .started)
        then . else error("old driver must terminate before its replacement starts; both Pod lifetimes must be observed") end
    ' "$ARTIFACTS/upgrade-watch.jsonl" > "$WORK_DIR/upgrade-order.json"
}
retry upgrade_order
cp "$WORK_DIR/upgrade-order.json" "$ARTIFACTS/upgrade-order.json"
stop_watch
consumer_ok a "$NODE_A"
release_claim a "$NODE_A"
retry claims_metric "$NODE_A" 0
retry claims_metric "$NODE_B" 0
printf 'Immutable-image upgrade, consumer continuity and final unprepare passed\n'
printf 'Native multi-node framework acceptance passed on %s\n' "$CLUSTER"
