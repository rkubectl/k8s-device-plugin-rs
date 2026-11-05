#!/usr/bin/env python3
"""Run the real Bash/jq lifecycle against stateful VM-free command doubles."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


FAKE = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
import time
import subprocess

root = Path(os.environ['MOK_TEST_ROOT'])
args = sys.argv[1:]
kind = Path(sys.argv[0]).name
if kind == 'sleep':
    sys.exit(0)
if kind == 'uname':
    print('Darwin' if args == ['-s'] else 'arm64')
    sys.exit(0)
state = json.loads((root / 'state.json').read_text())
c = os.environ['DRA_MULTI_NODE_CLUSTER']
nodes = [c, c+'-worker-1', c+'-worker-2']
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps([kind, *args])+'\n')

def save():
    (root / 'state.json').write_text(json.dumps(state))

def fail(code=23):
    sys.exit(code)

def emit(value):
    print(json.dumps(value), flush=True)

def image(slot):
    return 'docker.io/library/mok@sha256:'+slot

def pod(node, slot):
    return {'metadata': {'name': slot+'-'+node, 'uid': slot+'-'+node},
        'spec': {'nodeName': node, 'containers': [{'image': image(slot)}]},
        'status': {'containerStatuses': [{'name': 'dra-driver', 'ready': True,
            'imageID': image('wrong' if state.get('wrong_pod_image') else slot), 'state': {'running': {'startedAt': '2026-11-05T12:00:02Z'}}}]}}

def consumer(suffix):
    return {'metadata': {'uid': 'consumer-'+suffix}, 'spec': {'nodeName': nodes[1 if suffix == 'a' else 2]},
        'status': {'phase': 'Running', 'containerStatuses': [{'ready': True, 'restartCount': 0,
            'containerID': ('restarted-' if state.get('slot') == 'upgrade' and state.get('restart_consumer') else 'consumer-')+suffix}]}}

if kind == 'container':
    if args[0] == 'list':
        emit(state['inventory'])
    elif args[:2] == ['image', 'inspect']:
        slot = args[2].rsplit(':', 1)[1]
        if state.get('identical'): slot = 'baseline'
        emit([{'configuration': {'name': args[2], 'descriptor': {'digest': 'sha256:'+slot}},
            'variants': [{'digest': 'sha256:manifest-'+slot, 'platform': {'os': 'linux',
                'architecture': state.get('image_arch', 'arm64')}}]}])
    elif args[:2] == ['k8s-versioned', 'create']:
        state['inventory'] += [{'id': node, 'status': {'state': 'running'},
            'configuration': {'rosetta': state.get('rosetta', False), 'platform': {'architecture': 'arm64'},
                'labels': {'com.apple.container.plugin': 'cluster'}}} for node in nodes]
        Path(os.environ['KUBECONFIG']).write_text('temporary secret')
        state['kubeconfig'] = os.environ['KUBECONFIG']
        save()
        if state.get('failure') == 'create': fail(24)
    elif args[:2] == ['k8s-versioned', 'delete']:
        if state.get('cleanup_failure'): fail(29)
        state['inventory'] = [v for v in state['inventory'] if v['id'] not in nodes]
        save()
    elif args[:2] == ['k8s-versioned', 'load-image']:
        if state.get('failure') == 'import': fail(25)
    elif args[0] == 'exec':
        if args[2] == 'ctr':
            if args[6] == 'list':
                tag = args[7].removeprefix('name==')
                slot = tag.rsplit(':', 1)[1]
                print('REF TYPE DIGEST')
                print(tag+' oci sha256:'+('wrong' if state.get('wrong_digest') else slot))
            elif args[6] != 'tag': fail(94)
        elif args[2] == 'crictl':
            if args[4:6] != ['-o', 'json']: fail(95)
            slot = args[6].rsplit(':', 1)[1]
            emit({'status': {'repoDigests': [image(slot)], 'id': 'sha256:config-'+slot}})
        else:
            # Execute the actual cleanup predicate against temporary filesystem paths.
            script = args[4].replace('/var/lib/kubelet/plugins/dra.example.com', str(root/'plugins'))
            script = script.replace('/var/run/cdi', str(root/'cdi'))
            sys.exit(subprocess.run(['sh', '-ec', script, '--', *args[6:]]).returncode)
    else: fail(98)
elif kind == 'kubectl':
    if args == ['config', 'current-context']:
        print(c)
        sys.exit(0)
    args = args[3:]
    if args[:1] == ['-n']: args = args[2:]
    slot = state.get('slot', 'baseline')
    if args[:2] == ['get', '--raw'] and 'watch=true' in args[2]:
        # Avoid writing shared state from the watch process.
        while not (root/'upgrade').exists(): time.sleep(0.01)
        events = []
        for node in nodes[1:]:
            old = pod(node, 'baseline')
            old['status']['containerStatuses'][0]['state'] = {'terminated': {'finishedAt': '2026-11-05T12:00:01Z'}}
            new = pod(node, 'upgrade')
            pending = json.loads(json.dumps(new))
            pending['status'] = {}
            events += [{'type': 'MODIFIED', 'object': old}, {'type': 'ADDED', 'object': pending},
                {'type': 'MODIFIED', 'object': new}, {'type': 'DELETED', 'object': old}]
        if state.get('bad_order'):
            events.sort(key=lambda event: not event['object']['metadata']['uid'].startswith('upgrade-'))
        for event in events: emit(event)
        sys.exit(0)
    if args[0] == 'version': emit({'serverVersion': {'gitVersion': 'v1.36.1'}})
    elif args[:2] == ['get', 'nodes']:
        emit({'items': [{'metadata': {'name': n, 'uid': 'uid-'+n, 'labels': {}},
            'spec': {}, 'status': {'nodeInfo': {'architecture': 'arm64', 'kubeletVersion': 'v1.36.1'}}} for n in nodes]})
    elif args[:2] == ['create', '--dry-run=client']:
        emit({'kind': 'Namespace', 'metadata': {'name': 'k8s-device-plugin-dra-example'}})
        emit({'kind': 'DaemonSet', 'metadata': {}, 'spec': {'template': {'spec': {'containers': [{}]}}}})
    elif args[0] == 'apply':
        data = json.loads(Path(args[2]).read_text())
        for item in data['items']:
            if item['kind'] == 'DaemonSet': state['deployment'] = item
            if item['kind'] == 'Pod':
                state.setdefault('active', []).append(item['metadata']['name'][-1])
                state.setdefault('consumers', []).append(item)
        save()
    elif args[:2] == ['get', 'daemonset']:
        emit({'status': dict(desiredNumberScheduled=2, currentNumberScheduled=2, updatedNumberScheduled=2, numberReady=2),
            'spec': {'updateStrategy': {'rollingUpdate': {'maxSurge': 0}}}})
    elif args[:2] == ['get', 'pods']:
        if args[-1] == 'name':
            for node in nodes[1:]: print('pod/'+slot+'-'+node)
        else: emit({'metadata': {'resourceVersion': '123'}, 'items': [pod(n, slot) for n in nodes[1:]]})
    elif args[:2] == ['get', 'resourceslices']:
        emit({'items': [{'metadata': {'ownerReferences': [{'kind': 'Node', 'name': n, 'uid': 'uid-'+n, 'controller': True}]},
            'spec': {'nodeName': n, 'driver': 'dra.example.com', 'pool': {'name': n, 'generation': 1, 'resourceSliceCount': 1},
                'devices': [{'name': n}]}} for n in nodes[1:]]})
    elif args[:2] == ['get', 'pod']: emit(consumer(args[2][-1]))
    elif args[:2] == ['get', 'resourceclaim']:
        suffix = args[2][-1]
        node = nodes[1 if suffix == 'a' else 2]
        emit({'metadata': {'uid': 'claim-'+suffix}, 'status': {'allocation': {'devices': {
            'results': [{'driver': 'dra.example.com', 'pool': node, 'device': node, 'request': 'widget'}]}}}})
    elif args[:2] == ['get', '--raw']:
        suffix = 'a' if nodes[1] in args[2] else 'b'
        active = suffix in state.get('active', [])
        count = int(active or state.get('nonzero_metric', False))
        if not active and state.get('missing_metric'):
            print('dra_resource_claims_in_use{driver_name="dra.example.com"} 0')
        else:
            print('dra_resource_claims_in_use{driver_name="<any>"} '+str(count))
            if count: print('dra_resource_claims_in_use{driver_name="dra.example.com"} '+str(count))
    elif args[0] == 'exec':
        if '/usr/local/bin/k8s-device-plugin-dra-liveness' in args and state.get('failure') == 'probe': fail(26)
        if 'cat' in args:
            suffix = args[1][-1]
            node = nodes[1 if suffix == 'a' else 2]
            emit({'requests': [{'devices': [{'pool': node, 'name': node}]}]})
    elif args[0] == 'logs': print('unpreparing DRA claim claim-a\nunpreparing DRA claim claim-b')
    elif args[0] == 'delete':
        if args[1].startswith('pod/'):
            state['active'].remove(args[1][-1])
            save()
    elif args[:2] == ['set', 'image']:
        if state.get('failure') == 'upgrade': fail(28)
        state['slot'] = 'upgrade'
        save()
        (root/'upgrade').touch()
    elif args[0] == 'drain' and state.get('failure') == 'drain': fail(27)
    elif args[0] not in ['wait', 'label', 'rollout', 'cordon', 'drain', 'uncordon', 'get']: fail(97)
else: fail(96)
'''


class LifecycleTests(unittest.TestCase):
    def run_lifecycle(self, overrides=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inventory = [{'id': 'existing', 'status': {'state': 'stopped'},
                          'configuration': {'rosetta': False, 'platform': {'architecture': 'arm64'},
                                            'labels': {'com.apple.container.plugin': 'cluster'}}}]
            state = {'inventory': inventory}
            state.update(overrides or {})
            (root/'state.json').write_text(json.dumps(state))
            for name in ['container', 'kubectl', 'uname', 'sleep']:
                path = root/name
                path.write_text(FAKE)
                path.chmod(0o755)
            original = root/'original-kubeconfig'
            original.write_text('original context and credentials')
            if state.get('leftover_cdi'):
                (root/'cdi').mkdir()
                (root/'cdi'/'dra.example.com_metadata_claim-b_widget.json').touch()
            env = dict(os.environ, MOK_TEST_ROOT=tmp, PATH=tmp+os.pathsep+os.environ['PATH'],
                       KUBECONFIG=str(original), CONTAINER=str(root/'container'), KUBECTL=str(root/'kubectl'),
                       JQ=shutil.which('jq'), DRA_MULTI_NODE_CLUSTER='dra-validation-mok-test',
                       DRA_MULTI_NODE_BASELINE_IMAGE='docker.io/library/mok:baseline',
                       DRA_MULTI_NODE_UPGRADE_IMAGE='docker.io/library/mok:upgrade',
                       DRA_MULTI_NODE_ARTIFACTS=str(root/'evidence'))
            result = subprocess.run(['bash', str(Path(__file__).with_name('validate-multi-node.sh'))],
                                    env=env, capture_output=True, text=True, timeout=90)
            calls = [json.loads(line) for line in (root/'calls.jsonl').read_text().splitlines()]
            state = json.loads((root/'state.json').read_text())
            self.assertEqual(original.read_text(), 'original context and credentials')
            if 'kubeconfig' in state: self.assertFalse(Path(state['kubeconfig']).exists())
            evidence = root/'evidence'/'result.json'
            order = root/'evidence'/'upgrade-order.json'
            if order.exists(): state['upgrade_order'] = json.loads(order.read_text())
            return result, calls, state, json.loads(evidence.read_text()) if evidence.exists() else None

    def assert_clean(self, state):
        self.assertEqual([v['id'] for v in state['inventory']], ['existing'])
        self.assertEqual(state['inventory'][0]['status']['state'], 'stopped')

    def test_success_and_release_before_drain(self):
        result, calls, state, evidence = self.run_lifecycle()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_clean(state)
        self.assertEqual(evidence, {'result': 0, 'cleanup': 0})
        for transition in state['upgrade_order']:
            self.assertLess(transition['terminatedEvent'], transition['runningEvent'])
            self.assertGreater(transition['deletedEvent'], transition['addedEvent'])
        ds = state['deployment']['spec']
        self.assertEqual(ds['updateStrategy']['rollingUpdate'], {'maxSurge': 0, 'maxUnavailable': 1})
        self.assertEqual(ds['template']['spec']['nodeSelector'], {'dra.example.com/mok': 'dra-validation-mok-test'})
        driver = ds['template']['spec']['containers'][0]
        self.assertEqual(driver['image'], 'docker.io/library/mok@sha256:baseline')
        self.assertEqual(driver['env'][0]['name'], 'NODE_NAME')
        for variable in ['POOL_NAME', 'DEVICE_NAMES']:
            self.assertEqual(next(e['value'] for e in driver['env'] if e['name'] == variable), '$(NODE_NAME)')
        for consumer in state['consumers']:
            self.assertNotIn('nodeName', consumer['spec'])
            self.assertIn('requiredDuringSchedulingIgnoredDuringExecution', consumer['spec']['affinity']['nodeAffinity'])
        release = next(i for i, call in enumerate(calls) if 'pod/dra-mok-consumer-b' in call and 'delete' in call)
        cleanup = next(i for i, call in enumerate(calls) if call[:2] == ['container', 'exec'] and 'claim-b' in call)
        drain = next(i for i, call in enumerate(calls) if 'drain' in call)
        upgrade = next(i for i, call in enumerate(calls) if 'image' in call and 'set' in call)
        self.assertLess(release, cleanup)
        self.assertLess(cleanup, drain)
        self.assertLess(drain, upgrade)
        self.assertFalse(any(call[:2] == ['container', 'stop'] for call in calls))

    def test_reject_existing_target_and_worker_prefix(self):
        for name in ['dra-validation-mok-test', 'dra-validation-mok-test-worker-1']:
            with self.subTest(name=name):
                result, calls, _, _ = self.run_lifecycle({'inventory': [{'id': name}]})
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any('create' in c for c in calls))

    def test_reject_running_kubernetes_profiles(self):
        for plugin in ['cluster', 'k8s', 'rosettaless-k8s']:
            with self.subTest(plugin=plugin):
                result, calls, _, _ = self.run_lifecycle({'inventory': [{'id': 'foreign', 'status': {'state': 'running'},
                    'configuration': {'labels': {'com.apple.container.plugin': plugin}}}]})
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any('create' in c for c in calls))

    def test_reject_image_platform_and_identical_digests_before_create(self):
        for overrides in [{'image_arch': 'amd64'}, {'identical': True}]:
            with self.subTest(overrides=overrides):
                result, calls, state, _ = self.run_lifecycle(overrides)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any('create' in c for c in calls))
                self.assert_clean(state)

    def test_failure_paths_clean_owned_nodes(self):
        for failure, code in [('create', 24), ('import', 25), ('probe', 1), ('drain', 27), ('upgrade', 28)]:
            with self.subTest(failure=failure):
                result, _, state, evidence = self.run_lifecycle({'failure': failure})
                self.assertEqual(result.returncode, code, result.stderr)
                self.assert_clean(state)
                self.assertEqual(evidence, {'result': code, 'cleanup': 0})

    def test_missing_or_nonzero_metric_prevents_drain(self):
        for metric in ['missing_metric', 'nonzero_metric']:
            with self.subTest(metric=metric):
                result, calls, state, _ = self.run_lifecycle({metric: True})
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any('drain' in c for c in calls))
                self.assert_clean(state)

    def test_native_rosetta_check(self):
        result, _, state, _ = self.run_lifecycle({'rosetta': True})
        self.assertNotEqual(result.returncode, 0)
        self.assert_clean(state)

    def test_uid_scoped_artifact_cleanup_required_before_drain(self):
        result, calls, state, _ = self.run_lifecycle({'leftover_cdi': True})
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any('drain' in c for c in calls))
        self.assert_clean(state)

    def test_upgrade_order_required(self):
        result, _, state, _ = self.run_lifecycle({'bad_order': True})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('old driver must terminate', result.stderr)
        self.assert_clean(state)

    def test_imported_digest_must_match_local_image(self):
        result, calls, state, _ = self.run_lifecycle({'wrong_digest': True})
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any('apply' in c for c in calls))
        self.assert_clean(state)

    def test_running_pod_image_must_match_imported_digest(self):
        result, _, state, _ = self.run_lifecycle({'wrong_pod_image': True})
        self.assertNotEqual(result.returncode, 0)
        self.assert_clean(state)

    def test_consumer_must_not_restart_during_upgrade(self):
        result, _, state, _ = self.run_lifecycle({'restart_consumer': True})
        self.assertNotEqual(result.returncode, 0)
        self.assert_clean(state)

    def test_cleanup_failure_is_reported(self):
        result, _, _, evidence = self.run_lifecycle({'cleanup_failure': True})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(evidence['result'], 0)
        self.assertNotEqual(evidence['cleanup'], 0)
        self.assertIn('owned cluster cleanup failed', result.stderr)

    def test_original_failure_survives_cleanup_failure(self):
        result, _, _, evidence = self.run_lifecycle({'failure': 'create', 'cleanup_failure': True})
        self.assertEqual(result.returncode, 24, result.stderr)
        self.assertEqual(evidence['result'], 24)
        self.assertNotEqual(evidence['cleanup'], 0)


if __name__ == '__main__':
    unittest.main()
