"""Offline deployment contract tests; no SSH connections or systemd changes."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import re
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import manage
import remote
from check_release import inventory, verify_server


def topology(root):
    return {'schemaVersion': 1, 'deploymentId': 'my-deployment', 'releaseRoot': root / 'releases', 'outputDir': root / 'output',
            'control': {'sshTarget': 'control.invalid', 'publicUrl': 'https://control.invalid:8443', 'port': 8443, 'managed': True},
            'gateways': [{'sshTarget': 'gateway.invalid', 'id': 'node-1', 'name': 'Node 1', 'publicHost': 'gateway.invalid', 'port': 2222}]}


def report(identity='a', architecture='amd64'):
    return {'machineIdHash': identity * 64, 'architecture': architecture, 'listeners': [], 'roles': {},
            'services': {'control': {'active': False, 'enabled': False}, 'gateway': {'active': False, 'enabled': False}}}


def artifact(root, arch='amd64'):
    root.mkdir(parents=True)
    binary = bytearray(20); binary[:6] = b'\x7fELF\x02\x01'; binary[18:20] = {'amd64': 62, 'arm64': 183}[arch].to_bytes(2, 'little')
    (root / 'reborn').write_bytes(binary)
    (root / 'rules.json').write_text(json.dumps({'hosts': ['game.invalid'], 'domains': ['assets.invalid'], 'ports': [80, 443]}))
    (root / 'README.md').write_text('Release\n'); (root / 'LICENSE').write_text('License\n')
    (root / 'manifest.json').write_text(json.dumps({'version': '0.4.0', 'architecture': arch, 'protocol': 1, 'files': inventory(root)}))
    return root, verify_server(root)


class FakeSSH:
    def __init__(self):
        self.calls = []; self.fail_preflight = None; self.fail_role = None; self.fail_join = False; self.existing = None; self.ticket = 0
    def dependencies(self, target, install=False, identity=None):
        return {'machineIdHash': ('a' if target == 'control.invalid' else 'b') * 64,
                'architecture': 'amd64', 'distribution': 'debian', 'version': '13',
                'missingDependencies': [], 'checksComplete': True, 'changed': False}
    def worker(self, target, request):
        self.calls.append((request['action'], target, request.copy()))
        if request['action'] == 'preflight':
            if target == self.fail_preflight: raise RuntimeError('preflight failed')
            return report('a' if target == 'control.invalid' else 'b')
        if request['action'] == 'apply':
            if request['role'] == self.fail_role: raise RuntimeError('install failed')
            return {'snapshot': ('1' if request['role'] == 'control' else '2') * 32}
        return {'verified': True}
    def upload(self, target, root): self.calls.append(('upload', target, {})); return '/tmp/gbf-release-' + 'f' * 32
    def cleanup(self, target, path): self.calls.append(('cleanup', target, {}))
    def certificate(self, target):
        self.calls.append(('certificate', target, {}))
        return '-----BEGIN CERTIFICATE-----\nZmFrZS1jZXJ0aWZpY2F0ZQ==\n-----END CERTIFICATE-----\n'
    def admin(self, target, request):
        self.calls.append(('admin', target, request.copy()))
        if request['action'] == 'snapshot': return {'nodes': [] if self.existing is None else [self.existing]}
        self.ticket += 1
        self.existing = {'id': 'node-1', 'host': 'gateway.invalid', 'port': 2222, 'registered': False, 'status': 'pending'}
        return {'token': 'fixture-ticket-' + str(self.ticket)}
    def external(self, topology, groups, certificate): self.calls.append(('external', None, {})); return {'controlTls': 'passed'}
    def join(self, target, token):
        self.calls.append(('join', target, {'hasTicket': bool(token)}))
        if self.fail_join: raise RuntimeError('ticket expired')
        self.existing['registered'] = True


class TopologyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.root = Path(self.temp.name); self.topology = topology(self.root)
    def tearDown(self): self.temp.cleanup()
    def test_public_example_parses_without_secret_fields(self):
        config = manage.load_topology(Path(__file__).with_name('topology.example.json'))
        self.assertEqual(config['schemaVersion'], 1)
        raw = Path(__file__).with_name('topology.example.json').read_text().lower()
        for value in ('token', 'password', 'privatekey', '45.153.'):
            self.assertNotIn(value, raw)
    def test_deployment_id_matches_client_slug_and_reserved_name_rules(self):
        for value in ['my-deployment', '0', 'deployment_1', 'a' * 64, 'com10', 'console']:
            self.assertEqual(manage.deployment_id(value), value)
        for value in ['', 'A', '-bad', '_bad', 'two words', 'a' * 65, '部署', 'a/b', None, 1, 'con', 'prn', 'aux', 'nul'] + [f'{prefix}{i}' for prefix in ['com', 'lpt'] for i in range(1, 10)]:
            with self.subTest(value=value), self.assertRaises(ValueError): manage.deployment_id(value)
    def test_topology_requires_an_explicit_stable_deployment_id(self):
        value = topology(self.root); value['releaseRoot'] = '.'; value['outputDir'] = '.'
        for bad in [None, 'CON', 'lpt1']:
            value['deploymentId'] = bad
            path = self.root / 'topology.json'; path.write_text(json.dumps(value))
            with self.assertRaises(ValueError): manage.load_topology(path)
        del value['deploymentId']; path.write_text(json.dumps(value))
        with self.assertRaises(ValueError): manage.load_topology(path)
    def test_aliases_of_one_host_merge_same_gateway(self):
        other = dict(self.topology['gateways'][0], sshTarget='alias.invalid')
        self.topology['gateways'].append(other)
        groups = manage.group_hosts(self.topology, {'control.invalid': report('a'), 'gateway.invalid': report('b'), 'alias.invalid': report('b')})
        self.assertEqual(len(groups), 2); self.assertEqual(groups[1]['aliases'], ['gateway.invalid', 'alias.invalid'])
    def test_aliases_cannot_hide_second_gateway_id(self):
        self.topology['gateways'].append(dict(self.topology['gateways'][0], sshTarget='alias.invalid', id='node-2'))
        with self.assertRaisesRegex(ValueError, 'only one Gateway'):
            manage.group_hosts(self.topology, {'control.invalid': report('a'), 'gateway.invalid': report('b'), 'alias.invalid': report('b')})
    def test_one_gateway_id_cannot_span_two_hosts(self):
        self.topology['gateways'].append(dict(self.topology['gateways'][0], sshTarget='other.invalid'))
        with self.assertRaisesRegex(ValueError, 'multiple physical'):
            manage.group_hosts(self.topology, {'control.invalid': report('a'), 'gateway.invalid': report('b'), 'other.invalid': report('c')})
    def test_existing_host_binding_rejects_new_gateway_id(self):
        gateway = report('b'); gateway['roles']['gateway'] = {'nodeId': 'another-node'}
        with self.assertRaisesRegex(ValueError, 'different Gateway'):
            manage.group_hosts(self.topology, {'control.invalid': report('a'), 'gateway.invalid': gateway})
    def test_colocated_roles_use_distinct_paths_and_ports(self):
        self.topology['gateways'][0]['sshTarget'] = 'control.invalid'
        groups = manage.group_hosts(self.topology, {'control.invalid': report()})
        self.assertEqual(len(groups), 1)
        control = manage.configs(self.topology, groups[0], 'control'); gateway = manage.configs(self.topology, groups[0], 'gateway')
        self.assertNotEqual(control['dataDir'], str(Path(gateway['statsFile']).parent))
        self.assertNotEqual(control['tlsKey'], gateway['hostKey'])
        self.topology['gateways'][0]['port'] = 8443
        with self.assertRaisesRegex(ValueError, 'different ports'): manage.group_hosts(self.topology, {'control.invalid': report()})
    def test_port_occupation_rejected_unless_owned_active_service(self):
        state = report('a'); state['listeners'] = [8443]
        with self.assertRaisesRegex(ValueError, 'occupied'): manage.group_hosts(self.topology, {'control.invalid': state, 'gateway.invalid': report('b')})
        state['services']['control']['active'] = True; state['roles']['control'] = {'port': 8443}
        self.assertEqual(len(manage.group_hosts(self.topology, {'control.invalid': state, 'gateway.invalid': report('b')})), 2)
    def test_every_host_preflight_finishes_before_any_upload(self):
        ssh = FakeSSH(); ssh.fail_preflight = 'gateway.invalid'
        with self.assertRaises(RuntimeError): manage.preflight(self.topology, ssh)
        self.assertEqual([call[0] for call in ssh.calls], ['preflight', 'preflight'])
    def test_control_only_and_unmanaged_gateway_topologies(self):
        self.topology['gateways'] = []
        self.assertEqual(len(manage.preflight(self.topology, FakeSSH())), 1)
        self.topology['control']['managed'] = False
        with self.assertRaisesRegex(ValueError, 'active managed Control'): manage.preflight(self.topology, FakeSSH())
    def test_linux_support_matrix_is_explicit(self):
        for distro, versions in [('debian', ['12', '13']), ('ubuntu', ['24.04', '26.04'])]:
            for version in versions:
                for machine, architecture in [('x86_64', 'amd64'), ('aarch64', 'arm64')]:
                    self.assertEqual(remote.platform_facts(f'ID={distro}\nVERSION_ID="{version}"', machine), (distro, version, architecture))
        for release, machine in [('ID=debian\nVERSION_ID=11', 'x86_64'), ('ID=ubuntu\nVERSION_ID=24.04', 'i686')]:
            with self.assertRaises(ValueError): remote.platform_facts(release, machine)
    def test_management_ssh_enforces_known_host_and_token_stdin(self):
        with patch('manage.subprocess.run', return_value=subprocess.CompletedProcess([], 0, b'{}', b'')) as runner:
            manage.SSH().join('gateway.invalid', 'fixture-secret-ticket')
        args, kwargs = runner.call_args
        self.assertIn('StrictHostKeyChecking=yes', args[0]); self.assertNotIn('fixture-secret-ticket', repr(args))
        self.assertEqual(kwargs['input'], b'fixture-secret-ticket')
    def test_ssh_failures_never_expose_child_secret_output(self):
        with patch('manage.subprocess.run', return_value=subprocess.CompletedProcess([], 1, b'private-token', b'private-token')):
            with self.assertRaises(RuntimeError) as error: manage.SSH().join('gateway.invalid', 'private-token')
        self.assertNotIn('private-token', str(error.exception))
    def test_untrusted_configuration_rejects_credential_urls_and_flags(self):
        with self.assertRaises(ValueError): manage.ssh_target('-oProxyCommand=anything')
        with self.assertRaises(ValueError): manage.ssh_target('host;command')
        with self.assertRaises(ValueError): manage.port(True)
        value = topology(self.root); value['releaseRoot'] = '.'; value['outputDir'] = '.'; value['control']['publicUrl'] = 'https://user:password@host.invalid:8443'
        path = self.root / 'bad.json'; path.write_text(json.dumps(value))
        with self.assertRaises(ValueError): manage.load_topology(path)


class ReleaseTests(unittest.TestCase):
    def test_exact_manifest_and_architecture(self):
        with tempfile.TemporaryDirectory() as directory:
            root, _ = artifact(Path(directory) / 'artifact')
            verify_server(root, 'amd64')
            with self.assertRaises(ValueError): verify_server(root, 'arm64')
            (root / 'config.json').write_text('sensitive')
            with self.assertRaises(ValueError): verify_server(root)
    def test_nested_manifest_is_not_exempt_from_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root, _ = artifact(Path(directory) / 'artifact')
            (root / 'licenses').mkdir(); (root / 'licenses/manifest.json').write_text('unexpected')
            with self.assertRaises(ValueError): verify_server(root)
    def test_modified_binary_and_symbolic_link_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root, _ = artifact(Path(directory) / 'artifact')
            (root / 'reborn').write_bytes(b'changed')
            with self.assertRaises(ValueError): verify_server(root)
            (root / 'identity.key').symlink_to('/etc/passwd')
            with self.assertRaises(ValueError): verify_server(root)


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.root = Path(self.temp.name); self.topology = topology(self.root)
        self.artifact = artifact(self.root / 'releases/gbf-server-0.4.0-linux-amd64')
        self.ssh = FakeSSH(); self.groups = manage.preflight(self.topology, self.ssh)
    def tearDown(self): self.temp.cleanup()
    def test_success_preserves_pending_and_exports_only_public_profile(self):
        result = manage.apply(self.topology, self.groups, {'amd64': self.artifact}, self.ssh)
        self.assertEqual(result['newGatewayState'], 'pending')
        actions = [call[2].get('action') for call in self.ssh.calls if call[0] == 'admin']
        self.assertNotIn('node-set', actions)
        profile = json.loads((self.topology['outputDir'] / 'client-build.local.json').read_text())
        self.assertEqual(set(profile), {'deploymentId', 'url', 'caCertificateFile'})
        self.assertEqual(profile['caCertificateFile'], 'control-public.crt')
        self.assertEqual(profile['deploymentId'], self.topology['deploymentId'])
        for path in self.topology['outputDir'].iterdir(): self.assertNotIn('fixture-ticket-', path.read_text())
    def test_install_failure_rolls_back_prior_role(self):
        self.ssh.fail_role = 'gateway'
        with self.assertRaises(RuntimeError): manage.apply(self.topology, self.groups, {'amd64': self.artifact}, self.ssh)
        rollbacks = [call for call in self.ssh.calls if call[0] == 'rollback']
        self.assertEqual(len(rollbacks), 1); self.assertEqual(rollbacks[0][2]['snapshot'], '1' * 32)
    def test_expired_ticket_is_recoverable_without_enabling_node(self):
        self.ssh.fail_join = True
        with self.assertRaisesRegex(RuntimeError, 'rerun --apply'): manage.apply(self.topology, self.groups, {'amd64': self.artifact}, self.ssh)
        self.assertTrue(any('enrollment-pending' in path.read_text() for path in self.topology['outputDir'].glob('run-*.json')))
        self.ssh.fail_join = False
        manage.apply(self.topology, self.groups, {'amd64': self.artifact}, self.ssh, add_node='node-1')
        self.assertEqual(self.ssh.ticket, 2)
        self.assertIn('node-ticket', [call[2].get('action') for call in self.ssh.calls if call[0] == 'admin'])
    def test_already_registered_gateway_joins_idempotently_without_ticket(self):
        self.ssh.existing = {'id': 'node-1', 'host': 'gateway.invalid', 'port': 2222, 'registered': True}
        manage.apply(self.topology, self.groups, {'amd64': self.artifact}, self.ssh, add_node='node-1')
        self.assertEqual(self.ssh.ticket, 0)
        self.assertFalse([call for call in self.ssh.calls if call[0] == 'join'][0][2]['hasTicket'])
    def test_plan_uses_no_mutating_operations_or_output_files(self):
        value = {**self.topology, 'releaseRoot': str(self.topology['releaseRoot']), 'outputDir': str(self.topology['outputDir'])}
        config = self.root / 'topology.json'; config.write_text(json.dumps(value))
        with patch('manage.SSH', return_value=self.ssh), patch.object(sys, 'argv', ['manage.py', '--config', str(config), '--plan']), contextlib.redirect_stdout(io.StringIO()) as output:
            manage.main()
        self.assertFalse(self.topology['outputDir'].exists())
        self.assertFalse(json.loads(output.getvalue())['firewallChanges'])
        self.assertTrue(all(call[0] == 'preflight' for call in self.ssh.calls))
    def test_doctor_failed_checks_are_not_success_even_when_exit_status_is_zero(self):
        diagnostics = [{'check': name, 'status': 'passed'} for name in ['configuration', 'listener', 'master-key', 'tls-key', 'tls-certificate']]
        diagnostics.append({'check': 'database', 'status': 'failed'})
        with patch('remote.service_state', return_value={'active': True}), patch('remote.run', return_value=subprocess.CompletedProcess([], 0, json.dumps(diagnostics), '')):
            with self.assertRaisesRegex(ValueError, 'health checks failed'): remote.verify('control')
    def test_profile_id_survives_url_and_certificate_changes_and_pem_uses_lf(self):
        first = self.ssh.certificate('control.invalid').replace('\n', '\r\n')
        profile_path = manage.export_client(self.topology, first)
        original = json.loads(profile_path.read_text())
        self.assertEqual((self.topology['outputDir'] / 'control-public.crt').read_bytes(), first.replace('\r\n', '\n').encode('ascii'))
        self.topology['control']['publicUrl'] = 'https://new-control.invalid:8443'
        replacement = first.replace('ZmFrZS1jZXJ0aWZpY2F0ZQ==', 'cmVwbGFjZW1lbnQ=')
        manage.export_client(self.topology, replacement)
        updated = json.loads(profile_path.read_text())
        self.assertEqual(updated['deploymentId'], original['deploymentId'])
        self.assertEqual(updated['deploymentId'], 'my-deployment')
        self.assertNotEqual(updated['url'], original['url'])
        self.assertNotIn(b'\r', (self.topology['outputDir'] / 'control-public.crt').read_bytes())
    def test_export_client_reads_only_control_and_writes_public_profile(self):
        value = {**self.topology, 'releaseRoot': str(self.topology['releaseRoot']), 'outputDir': str(self.topology['outputDir'])}
        config = self.root / 'topology.json'; config.write_text(json.dumps(value))
        self.ssh.calls.clear()
        with patch('manage.SSH', return_value=self.ssh), patch.object(sys, 'argv', ['manage.py', '--config', str(config), '--export-client']), contextlib.redirect_stdout(io.StringIO()):
            manage.main()
        self.assertEqual([call[0] for call in self.ssh.calls], ['certificate'])
        example = json.loads(Path(__file__).with_name('client-build.example.json').read_text())
        exported = json.loads((self.topology['outputDir'] / 'client-build.local.json').read_text())
        self.assertEqual(set(example), set(exported))
    def test_remote_lock_rechecks_host_identity_before_apply(self):
        request = {'action': 'apply', 'machineIdHash': 'a' * 64, 'role': 'gateway'}
        with patch.object(remote, 'LOCK', self.root / 'lock'), patch('remote.preflight', return_value=report('b')), patch('remote.apply') as apply, patch('remote.sys.stdin', io.StringIO(json.dumps(request))):
            with self.assertRaisesRegex(ValueError, 'identity changed'): remote.main()
        apply.assert_not_called()
    def test_role_rollback_restores_binary_configuration_and_state(self):
        base, library, state, units = [self.root / name for name in ['etc', 'opt', 'state', 'units']]
        for directory in (base / 'gateway', library / 'gateway', state, units): directory.mkdir(parents=True)
        (base / 'gateway/config.json').write_text('old configuration')
        (base / 'gateway/identity.key').write_text('private fixture identity')
        (units / 'gbf-gateway.service').write_text('old unit')
        (library / 'gateway/current').symlink_to('/old/release')
        (state / 'state.json').write_text(json.dumps({'gateway': {'nodeId': 'node-1', 'version': '0.3.0'}}))
        with patch.object(remote, 'BASE', base), patch.object(remote, 'LIB', library), patch.object(remote, 'STATE', state), patch.object(remote, 'UNITS', units), patch('remote.service_state', return_value={'active': False, 'enabled': False}), patch('remote.run', return_value=subprocess.CompletedProcess([], 0, '', '')):
            token = remote.snapshot('gateway')
            (base / 'gateway/config.json').write_text('new configuration')
            (base / 'gateway/control.crt').write_text('new certificate absent from snapshot')
            (units / 'gbf-gateway.service').write_text('new unit')
            (library / 'gateway/current').unlink(); (library / 'gateway/current').symlink_to('/new/release')
            remote.restore(token)
        self.assertEqual((base / 'gateway/config.json').read_text(), 'old configuration')
        self.assertEqual((base / 'gateway/identity.key').read_text(), 'private fixture identity')
        self.assertFalse((base / 'gateway/control.crt').exists())
        self.assertEqual(str((library / 'gateway/current').readlink()), '/old/release')
        self.assertEqual(json.loads((state / 'state.json').read_text())['gateway']['version'], '0.3.0')
    def test_first_install_rollback_retains_only_gateway_identity_for_retry(self):
        base, library, state, units = [self.root / name for name in ['etc', 'opt', 'state', 'units']]
        for directory in (base / 'gateway', library / 'gateway', state, units): directory.mkdir(parents=True)
        with patch.object(remote, 'BASE', base), patch.object(remote, 'LIB', library), patch.object(remote, 'STATE', state), patch.object(remote, 'UNITS', units), patch('remote.service_state', return_value={'active': False, 'enabled': False}), patch('remote.run', return_value=subprocess.CompletedProcess([], 0, '', '')):
            token = remote.snapshot('gateway')
            for name in ['identity.key', 'host.key', 'config.json', 'control.crt', 'rules.json']: (base / 'gateway' / name).write_text('fixture')
            (state / 'state.json').write_text(json.dumps({'gateway': {'nodeId': 'node-1', 'installed': True}}))
            remote.restore(token)
        self.assertEqual({path.name for path in (base / 'gateway').iterdir()}, {'identity.key', 'host.key'})
        self.assertFalse(json.loads((state / 'state.json').read_text())['gateway']['installed'])
    def test_unit_definitions_keep_services_and_data_independent(self):
        control, gateway = remote.unit_text('control'), remote.unit_text('gateway')
        self.assertIn('User=gbf-control', control); self.assertIn('User=gbf-gateway', gateway)
        self.assertNotIn('/var/lib/gbf-reborn/control', gateway)
        for value in (control, gateway):
            for forbidden in ('sysctl', 'iptables', 'nft', 'sshd_config', 'BBR'): self.assertNotIn(forbidden, value)


if __name__ == '__main__': unittest.main()
