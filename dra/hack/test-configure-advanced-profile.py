#!/usr/bin/env python3
"""Check opt-in profile configuration without creating a VM or reading secrets."""
import copy
import importlib.util
import json
import sys
from pathlib import Path
import unittest

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location('configure_profile', Path(__file__).with_name('configure-advanced-profile.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


def kubeadm_documents():
    return [
        {'apiVersion':'kubeadm.k8s.io/v1beta4', 'kind':'InitConfiguration',
         'localAPIEndpoint':{'advertiseAddress':'192.0.2.1'}, 'opaque':'retain-me'},
        {'apiVersion':'kubeadm.k8s.io/v1beta4', 'kind':'ClusterConfiguration',
         'apiServer':{'certSANs':['127.0.0.1'], 'extraArgs':[
             {'name':'audit-log-path','value':'/var/log/audit.log'},
             {'name':'feature-gates','value':'ExistingGate=false'},
             {'name':'runtime-config','value':'apps/v1=true'}]},
         'networking':{'podSubnet':'10.244.0.0/16'}},
        {'apiVersion':'kubelet.config.k8s.io/v1beta1', 'kind':'KubeletConfiguration',
         'cgroupDriver':'systemd', 'featureGates':{'ExistingGate':False}},
    ]


class ProfileTests(unittest.TestCase):
    def test_adds_only_selected_gates_and_preserves_other_configuration(self):
        original = kubeadm_documents()
        before = copy.deepcopy(original)
        configured = profile.configure(original, 'kubeadm')
        self.assertEqual(original, before)
        self.assertEqual(configured[0], original[0])
        cluster = configured[1]
        self.assertEqual(cluster['networking'], original[1]['networking'])
        self.assertEqual(cluster['apiServer']['certSANs'], ['127.0.0.1'])
        args = {arg['name']:arg['value'] for arg in cluster['apiServer']['extraArgs']}
        self.assertEqual(args['audit-log-path'], '/var/log/audit.log')
        self.assertEqual(profile.csv_map(args['feature-gates']), {
            'ExistingGate':'false','GenericWorkload':'true','DRAWorkloadResourceClaims':'true','DRAResourcePoolStatus':'true'})
        self.assertEqual(profile.csv_map(args['runtime-config']), {
            'apps/v1':'true','scheduling.k8s.io/v1beta1':'true','resource.k8s.io/v1alpha3':'true'})
        self.assertEqual(configured[2]['featureGates'], {
            'ExistingGate':False,'GenericWorkload':True,'DRAWorkloadResourceClaims':True})

    def test_configuration_is_idempotent(self):
        first = profile.configure(kubeadm_documents(), 'kubeadm')
        self.assertEqual(profile.configure(first, 'kubeadm'), first)

    def test_runtime_kubelet_configuration_preserves_unrelated_fields(self):
        original = kubeadm_documents()[2]
        original['authentication'] = {'anonymous':{'enabled':False}}
        configured = profile.configure([original], 'kubelet')[0]
        self.assertEqual(configured['authentication'], original['authentication'])
        self.assertEqual(configured['cgroupDriver'], 'systemd')
        self.assertTrue(configured['featureGates']['DRAWorkloadResourceClaims'])

    def test_refuses_missing_duplicate_and_unsupported_configuration(self):
        cases = [kubeadm_documents()[:2], kubeadm_documents()+[kubeadm_documents()[1]]]
        version = kubeadm_documents()
        version[1]['apiVersion'] = 'kubeadm.k8s.io/v1beta3'
        cases.append(version)
        args = kubeadm_documents()
        args[1]['apiServer']['extraArgs'] = {'feature-gates':'ExistingGate=false'}
        cases.append(args)
        for documents in cases:
            with self.subTest(documents=documents), self.assertRaises(ValueError):
                profile.configure(documents, 'kubeadm')

    def test_json_document_stream_accepts_whitespace_and_preserves_values(self):
        documents = kubeadm_documents()
        stream = '  \n' + '\n\n'.join(json.dumps(doc) for doc in documents) + '\n'
        self.assertEqual(profile.read_documents(stream), documents)


if __name__ == '__main__':
    unittest.main()
