#!/usr/bin/env python3
"""Exercise the matrix's orchestration and recovery without touching a VM."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


# Stateful command doubles reject concurrent profiles and raw starts. The real
# matrix and jq run unchanged; fake smoke scripts stand in for cluster workloads.
FAKE_COMMAND = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['MATRIX_TEST_ROOT'])
state = json.loads((root / 'state.json').read_text())
args = sys.argv[1:]
kind = Path(sys.argv[0]).name
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps([kind, *args, *([os.environ.get('KUBE_CONTEXT')] if kind.startswith('e2e-') else [])]) + '\n')

def save():
    (root / 'state.json').write_text(json.dumps(state))

def fail(code):
    save()
    sys.exit(code)

if kind == 'container':
    if args[0] == 'list':
        state['lists'] = state.get('lists', 0) + 1
        if state.get('fail_list_after') == state['lists']:
            fail(23)
        save()
        print(json.dumps([{'id': name, 'status': {'state': status},
            'configuration': {'rosetta': state.get('rosetta', False)}}
            for name, status in state['clusters'].items()]))
    elif args[0] == 'stop':
        if args[1] == state.get('fail_stop'):
            fail(24)
        state['clusters'][args[1]] = 'stopped'
        save()
    elif args[:2] == ['k8s-versioned', 'start']:
        name = args[3]
        starts = state.setdefault('starts', {})
        starts[name] = starts.get(name, 0) + 1
        if name == state.get('fail_start') and starts[name] == state.get('fail_start_n', 1):
            fail(25)
        if any(status == 'running' for other, status in state['clusters'].items() if other != name):
            fail(99)
        state['clusters'][name] = 'running'
        save()
    elif args[:2] != ['k8s-versioned', 'load-image'] and args[0] != 'build':
        fail(98)
elif kind == 'kubectl':
    name = args[1]
    if args[2] == 'version':
        print(json.dumps({'serverVersion': {'gitVersion': state.get('version_override',
            'v1.37.0' if name.endswith('-137') else 'v1.36.1')}}))
    elif 'logs' in args and state.get('fail_logs'):
        fail(26)
else:
    if kind == state.get('fail_smoke'):
        fail(27)
'''


class MatrixTests(unittest.TestCase):
    def run_matrix(self, overrides=None, env_overrides=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            state = {'clusters': {'dra-validation': 'stopped', 'dra-validation-137': 'stopped'}}
            state.update(overrides or {})
            (root / 'state.json').write_text(json.dumps(state))
            script = root / 'validate-k8s-versions.sh'
            shutil.copyfile(Path(__file__).with_name(script.name), script)
            for name in ['container', 'kubectl', 'e2e-smoke.sh', 'e2e-extended-resource.sh']:
                path = root / name
                path.write_text(FAKE_COMMAND)
                path.chmod(0o755)
            env = dict(os.environ, MATRIX_TEST_ROOT=tmp, CONTAINER=str(root / 'container'),
                       KUBECTL=str(root / 'kubectl'), JQ=shutil.which('jq'),
                       DRA_E2E_SKIP_BUILD='1', DRA_E2E_V136_CLUSTER='dra-validation',
                       DRA_E2E_V137_CLUSTER='dra-validation-137')
            env.update(env_overrides or {})
            result = subprocess.run(['bash', str(script)], env=env, capture_output=True, text=True)
            calls_file = root / 'calls.jsonl'
            calls = [json.loads(line) for line in calls_file.read_text().splitlines()] if calls_file.exists() else []
            state = json.loads((root / 'state.json').read_text())
            return result, calls, state

    def assert_baseline(self, state):
        self.assertEqual(state['clusters'], {'dra-validation': 'running', 'dra-validation-137': 'stopped'})

    def test_success_is_serial_and_uses_guarded_restore(self):
        result, calls, state = self.run_matrix(env_overrides={'DRA_E2E_SKIP_BUILD': '0'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_baseline(state)
        self.assertIn(['container', 'build', '--platform', 'linux/arm64', '-f', 'dra/Dockerfile',
                       '-t', 'docker.io/library/k8s-device-plugin-dra-example:latest', '.'], calls)
        self.assertEqual([c for c in calls if c[:3] == ['container', 'k8s-versioned', 'load-image']], [
            ['container', 'k8s-versioned', 'load-image', '--name', name,
             'docker.io/library/k8s-device-plugin-dra-example:latest']
            for name in ['dra-validation', 'dra-validation-137']])
        self.assertEqual([c for c in calls if c[:3] == ['container', 'k8s-versioned', 'start']], [
            ['container', 'k8s-versioned', 'start', '--name', 'dra-validation'],
            ['container', 'k8s-versioned', 'start', '--name', 'dra-validation-137'],
            ['container', 'k8s-versioned', 'start', '--name', 'dra-validation']])
        self.assertEqual([c for c in calls if c[0] == 'e2e-smoke.sh'], [
            ['e2e-smoke.sh', 'dra-validation'], ['e2e-smoke.sh', 'dra-validation-137']])
        self.assertEqual(sum(c[0] == 'e2e-extended-resource.sh' for c in calls), 1)
        for name in state['clusters']:
            logs = next(i for i, c in enumerate(calls) if c[:3] == ['kubectl', '--context', name] and 'logs' in c)
            stop = calls.index(['container', 'stop', name])
            self.assertLess(logs, stop)

    def test_running_profile_still_gets_guarded_repair(self):
        result, calls, state = self.run_matrix({'clusters': {
            'dra-validation': 'running', 'dra-validation-137': 'stopped'}})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(state['starts']['dra-validation'], 2)
        self.assert_baseline(state)

    def test_start_failure_preserved_and_baseline_restored(self):
        result, calls, state = self.run_matrix({'fail_start': 'dra-validation-137'})
        self.assertEqual(result.returncode, 25, result.stderr)
        self.assert_baseline(state)

    def test_smoke_failure_collects_logs_and_restores(self):
        result, calls, state = self.run_matrix({'fail_smoke': 'e2e-extended-resource.sh'})
        self.assertEqual(result.returncode, 27, result.stderr)
        self.assert_baseline(state)
        self.assertTrue(any(c[:3] == ['kubectl', '--context', 'dra-validation-137'] and 'logs' in c for c in calls))

    def test_failed_restoration_makes_success_nonzero(self):
        result, calls, state = self.run_matrix({'fail_start': 'dra-validation', 'fail_start_n': 2})
        self.assertEqual(result.returncode, 25, result.stderr)
        self.assertIn('failed to restore', result.stderr)

    def test_cleanup_failure_preserves_original_failure(self):
        result, calls, state = self.run_matrix({'fail_smoke': 'e2e-extended-resource.sh',
                                               'fail_start': 'dra-validation', 'fail_start_n': 2})
        self.assertEqual(result.returncode, 27, result.stderr)
        self.assertIn('failed to restore', result.stderr)

    def test_failed_stop_prevents_baseline_start(self):
        result, calls, state = self.run_matrix({'fail_stop': 'dra-validation-137'})
        self.assertEqual(result.returncode, 24, result.stderr)
        self.assertEqual(state['starts']['dra-validation'], 1)
        self.assertEqual(state['clusters']['dra-validation-137'], 'running')

    def test_status_query_failure_is_not_treated_as_stopped(self):
        result, calls, state = self.run_matrix({'fail_list_after': 3})
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertFalse(any(c[0] == 'e2e-smoke.sh' for c in calls))
        self.assert_baseline(state)

    def test_failed_cleanup_status_query_prevents_baseline_start(self):
        result, calls, state = self.run_matrix({'fail_list_after': 7})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(state['starts']['dra-validation'], 1)
        self.assertIn('failed to restore', result.stderr)

    def test_unavailable_logs_do_not_hide_smoke_result(self):
        result, calls, state = self.run_matrix({'fail_logs': True, 'fail_smoke': 'e2e-smoke.sh'})
        self.assertEqual(result.returncode, 27, result.stderr)
        self.assert_baseline(state)

    def test_wrong_server_version_fails_and_reports_restore_failure(self):
        result, calls, state = self.run_matrix({'version_override': 'v1.35.0'})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('failed to restore', result.stderr)
        self.assertFalse(any(c[0] == 'e2e-smoke.sh' for c in calls))

    def test_preflight_failure_never_starts_or_stops(self):
        for overrides, env in [({'rosetta': True}, {}),
                               ({}, {'DRA_E2E_V137_CLUSTER': 'dra-validation'}),
                               ({}, {'KUBECTL': '/nonexistent/kubectl'})]:
            with self.subTest(overrides=overrides, env=env):
                result, calls, state = self.run_matrix(overrides, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(c[0] == 'container' and c[1] != 'list' for c in calls))


if __name__ == '__main__':
    if not shutil.which('jq'):
        raise SystemExit('jq is required to test the matrix')
    unittest.main()
