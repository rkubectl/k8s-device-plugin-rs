#!/usr/bin/env python3
"""Patch JSON documents converted from owned validation-profile YAML by yq."""
import argparse
import copy
import json
import sys

COMMON_GATES = {'GenericWorkload': 'true', 'DRAWorkloadResourceClaims': 'true'}


def csv_map(value):
    result = {}
    for entry in value.split(','):
        if not entry:
            continue
        key, separator, setting = entry.partition('=')
        if not separator or not key or not setting:
            raise ValueError('invalid existing comma-separated option')
        result[key] = setting
    return result


def merge_args(component, updates):
    args = component.get('extraArgs') or []
    if not isinstance(args, list):
        raise ValueError('expected kubeadm v1beta4 extraArgs list')
    kept = [arg for arg in args if arg['name'] not in updates]
    for name, values in updates.items():
        merged = {}
        for arg in args:
            if arg['name'] == name:
                merged.update(csv_map(arg['value']))
        merged.update(values)
        kept.append({'name': name, 'value': ','.join(f'{key}={value}' for key, value in sorted(merged.items()))})
    component['extraArgs'] = kept


def configure(documents, mode):
    documents = copy.deepcopy(documents)
    expected = {'InitConfiguration': 1, 'ClusterConfiguration': 1, 'KubeletConfiguration': 1} if mode == 'kubeadm' else {'KubeletConfiguration': 1}
    for kind, count in expected.items():
        if sum(doc.get('kind') == kind for doc in documents) != count:
            raise ValueError(f'expected exactly {count} {kind} document')
    for doc in documents:
        if doc.get('kind') == 'ClusterConfiguration':
            if doc.get('apiVersion') != 'kubeadm.k8s.io/v1beta4':
                raise ValueError('expected kubeadm.k8s.io/v1beta4')
            api_gates = dict(COMMON_GATES, DRAResourcePoolStatus='true')
            merge_args(doc.setdefault('apiServer', {}), {
                'feature-gates': api_gates,
                'runtime-config': {'scheduling.k8s.io/v1beta1': 'true', 'resource.k8s.io/v1alpha3': 'true'},
            })
            merge_args(doc.setdefault('controllerManager', {}), {'feature-gates': api_gates})
            merge_args(doc.setdefault('scheduler', {}), {'feature-gates': COMMON_GATES})
        elif doc.get('kind') == 'KubeletConfiguration':
            if doc.get('apiVersion') != 'kubelet.config.k8s.io/v1beta1':
                raise ValueError('expected kubelet.config.k8s.io/v1beta1')
            gates = doc.setdefault('featureGates', {})
            if not isinstance(gates, dict):
                raise ValueError('expected kubelet featureGates map')
            gates.update({name: True for name in COMMON_GATES})
    return documents


def read_documents(text):
    decoder = json.JSONDecoder()
    documents = []
    while text.strip():
        doc, end = decoder.raw_decode(text.lstrip())
        documents.append(doc)
        text = text.lstrip()[end:]
    return documents


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=['kubeadm', 'kubelet'], required=True)
    args = parser.parse_args()
    try:
        documents = configure(read_documents(sys.stdin.read()), args.mode)
    except (KeyError, TypeError, ValueError) as error:
        raise SystemExit(str(error)) from error
    for document in documents:
        print(json.dumps(document))
