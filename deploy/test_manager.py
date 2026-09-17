"""Isolated CLI and installation tests; never operate real services."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

import gpr
import manage
import remote
from test_deploy import FakeSSH, artifact, topology


class CommandTests(unittest.TestCase):
    def invoke(self, argv, roles=('control', 'gateway'), results=None):
        with patch('gpr.os.geteuid', return_value=0), patch('gpr.installed_roles', return_value=list(roles)), \
                patch('gpr.subprocess.run') as run, contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            run.return_value = subprocess.CompletedProcess([], 0)
            if results is not None: run.side_effect = results
            code = gpr.main(argv)
            return code, [call.args[0] for call in run.call_args_list]

    def test_help_does_not_require_root(self):
        with patch('gpr.os.geteuid') as uid, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(gpr.main([]), 0)
            with self.assertRaises(SystemExit) as result: gpr.main(['--help'])
            self.assertEqual(result.exception.code, 0)
            uid.assert_not_called()

    def test_bad_arguments_never_elevate_or_execute(self):
        for argv in [['restart'], ['stop', 'gateway;true'], ['admin', '--json'], ['logs', '--lines', '0'], ['other']]:
            with self.subTest(argv=argv), patch('gpr.subprocess.run') as run, contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit): gpr.main(argv)
                run.assert_not_called()

    def test_missing_roles_never_execute(self):
        for argv, roles in [(['admin'], ['gateway']), (['start', 'gateway'], ['control']), (['status'], [])]:
            self.assertEqual(self.invoke(argv, roles), (1, []))

    def test_status_and_logs_default_to_installed_roles(self):
        code, calls = self.invoke(['status'], ['gateway'], [subprocess.CompletedProcess([], 3)])
        self.assertEqual(code, 3)
        self.assertEqual(calls, [[gpr.SYSTEMCTL, '--no-pager', '--full', 'status', 'gbf-gateway.service']])
        code, calls = self.invoke(['logs', '-f'])
        self.assertEqual(calls, [['/usr/bin/journalctl', '--no-pager', '-n', '100', '-u', 'gbf-control.service', '-u', 'gbf-gateway.service', '-f']])

    def test_admin_runs_as_control_account(self):
        _, calls = self.invoke(['admin'])
        self.assertEqual(calls, [['/usr/sbin/runuser', '-u', 'gbf-control', '--', '/opt/gbf-reborn/control/current/reborn', 'admin', '--socket', '/run/gbf-control/admin.sock']])

    def test_start_stop_restart_order(self):
        for action, expected in [('start', [('start', 'control'), ('start', 'gateway')]),
                                 ('stop', [('stop', 'gateway'), ('stop', 'control')]),
                                 ('restart', [('stop', 'gateway'), ('stop', 'control'), ('start', 'control'), ('start', 'gateway')])]:
            _, calls = self.invoke([action, 'all'])
            self.assertEqual(calls, [[gpr.SYSTEMCTL, a, 'gbf-' + r + '.service'] for a, r in expected])
        _, calls = self.invoke(['restart', 'gateway'])
        self.assertEqual(calls, [[gpr.SYSTEMCTL, 'restart', 'gbf-gateway.service']])

    def test_lifecycle_failure_stops_further_actions(self):
        code, calls = self.invoke(['restart', 'all'], results=[subprocess.CompletedProcess([], 5)])
        self.assertEqual(code, 5)
        self.assertEqual(len(calls), 1)

    def test_elevation_retains_arguments_and_failure_code(self):
        with patch('gpr.os.geteuid', return_value=1000), patch('gpr.subprocess.run', return_value=subprocess.CompletedProcess([], 1)) as run, \
                patch('gpr.installed_roles') as roles, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(gpr.main(['logs', 'gateway', '-f']), 1)
            run.assert_called_once_with(['/usr/bin/sudo', '-n', '--', '/usr/local/bin/gpr', 'logs', 'gateway', '-f'])
            roles.assert_not_called()

    def test_doctor_reports_failure_and_not_checked_separately(self):
        for status, expected in [('passed', 0), ('not_checked', 0), ('failed', 1), ('invalid', 1)]:
            result = subprocess.CompletedProcess([], 0, json.dumps([{'check': 'example', 'status': status, 'detail': 'test'}]))
            code, calls = self.invoke(['doctor', 'gateway'], results=[result])
            self.assertEqual(code, expected)
            self.assertIn('gbf-gateway', calls[0])
        for result in [subprocess.CompletedProcess([], 1, ''), subprocess.CompletedProcess([], 0, 'broken'), subprocess.CompletedProcess([], 0, '[]')]:
            self.assertEqual(self.invoke(['doctor', 'control'], results=[result])[0], 1)

    def test_installed_roles_ignore_rolled_back_and_incomplete_roles(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for role in gpr.ROLES:
                (root / role / 'current').mkdir(parents=True)
                (root / role / 'current/reborn').write_text('binary')
                (root / role / 'config.json').write_text('{}')
            registry = root / 'state.json'
            registry.write_text(json.dumps({'control': {'installed': True}, 'gateway': {'installed': False}}))
            with patch.object(gpr, 'REGISTRY', registry), patch.object(gpr, 'BASE', root), patch.object(gpr, 'LIB', root):
                self.assertEqual(gpr.installed_roles(), ['control'])
                (root / 'control/config.json').unlink()
                self.assertEqual(gpr.installed_roles(), [])


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.path = self.root / 'gpr'
        self.source = manage.MANAGER.read_text()
        self.request = {'source': self.source, 'sha256': hashlib.sha256(self.source.encode()).hexdigest()}
        self.stack = contextlib.ExitStack()
        self.stack.enter_context(patch.object(remote, 'MANAGER', self.path))
        # Ancestors outside the temporary directory are not managed by this test.
        self.stack.enter_context(patch('remote.ensure_manager_parent'))
        self.run = self.stack.enter_context(patch('remote.run'))
    def tearDown(self): self.stack.close(); self.temp.cleanup()

    def test_install_repeat_and_update_are_atomic_and_do_not_touch_services(self):
        self.assertTrue(remote.install_manager(self.request)['changed'])
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o755)
        inode = self.path.stat().st_ino
        self.assertFalse(remote.install_manager(self.request)['changed'])
        self.assertEqual(self.path.stat().st_ino, inode)
        source = self.source + '\n# updated\n'
        result = remote.install_manager({'source': source, 'sha256': hashlib.sha256(source.encode()).hexdigest()})
        self.assertTrue(result['changed']); self.assertEqual(self.path.read_text(), source)
        self.run.assert_not_called()

    def test_conflicting_files_and_symlinks_are_preserved(self):
        self.path.write_text('unrelated command'); self.path.chmod(0o755)
        with self.assertRaisesRegex(ValueError, 'another program'): remote.install_manager(self.request)
        self.assertEqual(self.path.read_text(), 'unrelated command')
        self.path.unlink(); target = self.root / 'target'; target.write_text(self.source)
        self.path.symlink_to(target)
        with self.assertRaisesRegex(ValueError, 'symlink'): remote.install_manager(self.request)
        self.assertTrue(self.path.is_symlink())

    def test_world_writable_command_is_rejected(self):
        self.path.write_text(self.source); self.path.chmod(0o777)
        with self.assertRaisesRegex(ValueError, 'unmanaged'): remote.install_manager(self.request)

    def test_failed_replace_preserves_previous_command_and_cleans_temp(self):
        self.path.write_text(self.source); self.path.chmod(0o755)
        source = self.source + '\n# next\n'
        with patch('remote.os.replace', side_effect=OSError('failure')):
            with self.assertRaises(OSError): remote.install_manager({'source': source, 'sha256': hashlib.sha256(source.encode()).hexdigest()})
        self.assertEqual(self.path.read_text(), self.source)
        self.assertEqual(list(self.root.glob('.gpr-pending-*')), [])

    def test_invalid_source_and_checksum_do_not_write(self):
        for value in [{'source': 'bad'}, {**self.request, 'sha256': '0' * 64}]:
            with self.assertRaises(ValueError): remote.install_manager(value)
            self.assertFalse(self.path.exists())


class ParentTests(unittest.TestCase):
    def test_unsafe_ancestor_is_rejected(self):
        for symlink, writable, owner in [(True, False, 0), (False, True, 0), (False, False, 1000)]:
            parent = Mock()
            parent.is_symlink.return_value = symlink
            parent.exists.return_value = True
            parent.is_dir.return_value = True
            parent.stat.return_value = Mock(st_uid=owner, st_mode=0o777 if writable else 0o755)
            with patch.object(remote, 'MANAGER', Mock(parents=[parent])), patch('remote.os.geteuid', return_value=0):
                with self.assertRaises(ValueError): remote.ensure_manager_parent()


class IntegrationTests(unittest.TestCase):
    def test_install_manager_cli_needs_no_release_and_no_service_mutation(self):
        fake = FakeSSH()
        with tempfile.TemporaryDirectory() as folder:
            config = topology(Path(folder))
            with patch('sys.argv', ['manage.py', '--config', 'unused.json', '--install-manager']), \
                    patch('manage.load_topology', return_value=config), patch('manage.SSH', return_value=fake), \
                    patch('manage.releases') as releases, contextlib.redirect_stdout(io.StringIO()):
                manage.main()
                releases.assert_not_called()
            self.assertEqual([a for a, _, _ in fake.calls], ['preflight', 'preflight', 'install-manager', 'install-manager'])
            self.assertFalse(config['outputDir'].exists())

    def test_apply_installs_manager_after_external_verification_and_failure_keeps_services(self):
        with tempfile.TemporaryDirectory() as folder:
            config = topology(Path(folder)); fake = FakeSSH()
            groups = manage.preflight(config, fake)
            release = artifact(config['releaseRoot'] / 'gbf-server-0.4.0-linux-amd64')
            original = fake.worker
            def worker(target, request):
                if request['action'] == 'install-manager': raise RuntimeError('conflict')
                return original(target, request)
            fake.worker = worker
            with self.assertRaisesRegex(RuntimeError, '--install-manager'):
                manage.apply(config, groups, {'amd64': release}, fake)
            actions = [a for a, _, _ in fake.calls]
            self.assertIn('external', actions); self.assertNotIn('rollback', actions)
            journal = json.loads(next(config['outputDir'].glob('run-*.local.json')).read_text())
            self.assertEqual(journal['state'], 'manager-install-failed')
            self.assertTrue((config['outputDir'] / 'client-build.local.json').exists())


if __name__ == '__main__': unittest.main()
