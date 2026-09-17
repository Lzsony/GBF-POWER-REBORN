"""Run the real POSIX bootstrap against isolated commands, never the host APT."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import dependencies
import manage
from test_deploy import FakeSSH, artifact, topology

FAKE = r'''
import hashlib,json,os,pathlib,subprocess,sys
root=pathlib.Path(os.environ['GPR_FIXTURE_ROOT']); name=pathlib.Path(sys.argv[0]).name; args=sys.argv[1:]
state_path=root/'state.json'; state=json.loads(state_path.read_text())
def save(): state_path.write_text(json.dumps(state))
def fail(message): print(message,file=sys.stderr);sys.exit(1)
if name=='id': print(0 if not state.get('nonroot') else 1000)
elif name=='uname': print(state.get('architecture','x86_64'))
elif name=='sha256sum': print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest()+'  -')
elif name=='stat': print('0' if args[1]=='%u' else '700')
elif name=='sleep': pass
elif name=='dpkg':
 if state.get('broken'): print('unconfigured package')
elif name=='dpkg-query':
 package=args[-1].split(':')[0]
 if package in state.get('installed',[]): print('installed',end='')
 else: sys.exit(1)
elif name=='dpkg-deb':
 print(pathlib.Path(args[1]).stem if args[-1]=='Package' else 'amd64')
elif name=='apt-get':
 mode='update' if 'update' in args else ('simulate' if '-s' in args else 'install')
 with (root/'calls').open('a') as f:f.write(json.dumps({'mode':mode,'args':args})+'\n')
 if state.get('fail')==mode:
  fail('Could not get lock /var/lib/dpkg/lock-frontend' if state.get('locked') else 'fixture package failure')
 if mode=='update': sys.exit(0)
 packages=args[args.index('install')+1:]
 if mode=='simulate':
  if state.get('upgrade'): print('Inst libc6 [2.1] (2.2 Debian)')
  if state.get('remove'): print('Remv existing [1]')
  for package in packages: print('Inst '+package+' (1 Debian)')
  sys.exit(0)
 if state.get('race'):
  state.setdefault('installed',[]).append(packages[0]);save()
 guard=next(a.split('=',1)[1] for a in args if a.startswith('DPkg::Pre-Install-Pkgs::='))
 result=subprocess.run([guard],input=''.join(str(root/(p+'.deb'))+'\n' for p in packages),text=True)
 if result.returncode:sys.exit(result.returncode)
 mapping={'python3':['python3'],'openssl':['openssl'],'openssh-client':['ssh-keygen'],'iproute2':['ss'], 'passwd':['useradd'],'util-linux':['runuser'],'tar':['tar']}
 if not state.get('still_missing'):
  for package in packages:
   for command in mapping.get(package,[]):
    (root/'bin'/command).write_text('#!/bin/sh\nexit 0\n');(root/'bin'/command).chmod(0o755)
   if package=='ca-certificates': (root/'ca.crt').write_text('public certificate fixture')
 state.setdefault('installed',[]).extend(packages);save()
'''


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name); self.bin = self.root / 'bin'; self.bin.mkdir()
        self.state = {}
        for tool in ['cat', 'tr', 'grep', 'awk', 'mkdir', 'rmdir', 'mktemp', 'chmod', 'rm']:
            self.bin.joinpath(tool).symlink_to(shutil.which(tool))
        for tool in ['id','uname','sha256sum','stat','sleep','dpkg','dpkg-query','dpkg-deb','apt-get']:
            path = self.bin / tool; path.write_text('#!' + sys.executable + '\n' + FAKE); path.chmod(0o755)
        for tool in ['systemctl','journalctl','timeout','python3','openssl','ssh-keygen','ss','useradd','runuser','tar']:
            path = self.bin / tool; path.write_text('#!/bin/sh\nexit 0\n'); path.chmod(0o755)
        (self.root / 'os-release').write_text('ID=debian\nVERSION_ID="13"\n')
        (self.root / 'machine-id').write_text('a' * 32 + '\n')
        (self.root / 'systemd').mkdir()
        (self.root / 'ca.crt').write_text('public certificate fixture')
        source = dependencies.BOOTSTRAP.read_text().replace('PATH=/usr/sbin:/usr/bin:/sbin:/bin', 'PATH=' + str(self.bin))
        replacements = {'GPR_OS_RELEASE': 'os-release','GPR_MACHINE_ID': 'machine-id', 'GPR_SYSTEMD': 'systemd',
            'GPR_CA_BUNDLE': 'ca.crt','GPR_LOCK':'lock','GPR_LOG_DIR':'logs','GPR_TEMP_ROOT':'.'}
        import re
        for key, value in replacements.items():
            source = re.sub('^' + key + '=.*$', key + '=' + str(self.root / value), source, flags=re.M)
        self.script = self.root / 'bootstrap.sh'; self.script.write_text(source)

    def tearDown(self): self.temp.cleanup()

    def invoke(self, mode='probe', identity=None):
        (self.root / 'state.json').write_text(json.dumps(self.state))
        identity = identity or hashlib.sha256(('a' * 32).encode()).hexdigest()
        result = subprocess.run(['/bin/sh', str(self.script), mode, identity], env={**os.environ, 'GPR_FIXTURE_ROOT':str(self.root)}, capture_output=True, text=True, timeout=30)
        self.state = json.loads((self.root / 'state.json').read_text())
        return result

    def calls(self):
        file = self.root / 'calls'
        return [json.loads(line) for line in file.read_text().splitlines()] if file.exists() else []

    def test_complete_probe_is_readonly_and_install_is_noop(self):
        before = set(self.root.iterdir())
        self.assertEqual(self.invoke().returncode, 0)
        self.assertEqual(set(self.root.iterdir()) - before, {self.root / 'state.json'})
        result = self.invoke('install')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('changed\tfalse', result.stdout)
        self.assertEqual(self.calls(), [])
        self.assertFalse((self.root / 'lock').exists())

    def test_missing_python_probe_does_not_execute_python_or_apt(self):
        (self.bin / 'python3').unlink()
        result = self.invoke()
        self.assertEqual(result.returncode, 0)
        report = dependencies.parse_report(result.stdout.encode())
        self.assertFalse(report['checksComplete'])
        self.assertEqual(report['missingDependencies'], [{'capability':'python3','package':'python3'}])
        self.assertEqual(self.calls(), [])

    def test_install_missing_python_and_multiple_dependencies_then_recheck(self):
        for missing in [('python3',), ('python3','openssl','ssh-keygen','ss','useradd','runuser','tar')]:
            with self.subTest(missing=missing):
                self.state = {}
                for command in missing: (self.bin / command).unlink()
                (self.root / 'ca.crt').unlink(missing_ok=True)
                result = self.invoke('install')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('changed\ttrue', result.stdout)
                for command in missing: self.assertTrue((self.bin / command).exists())
                self.assertTrue((self.root / 'ca.crt').exists())
                self.assertEqual([r['mode'] for r in self.calls()][-3:], ['update','simulate','install'])
                last = self.calls()[-1]['args']
                self.assertIn('--no-remove', last); self.assertIn('--no-upgrade', last)
                self.assertNotIn('upgrade', last)
                self.assertFalse((self.root / 'lock').exists())

    def test_invalid_environment_does_not_install(self):
        for value in ['ID=alpine\nVERSION_ID=3\n','ID=ubuntu\nVERSION_ID=20.04\n']:
            (self.root / 'os-release').write_text(value)
            self.assertIn('UNSUPPORTED_OS', self.invoke('install').stderr)
        (self.root / 'os-release').write_text('ID=debian\nVERSION_ID=13\n')
        self.state['architecture']='riscv64'
        self.assertIn('UNSUPPORTED_ARCH',self.invoke().stderr)
        self.state={}; (self.root / 'systemd').rmdir()
        self.assertIn('SYSTEMD_REQUIRED',self.invoke().stderr)
        self.assertEqual(self.calls(), [])

    def test_supported_distributions_and_arm64_are_discovered(self):
        for distro,version in [('debian','12'),('debian','13'),('ubuntu','24.04'),('ubuntu','26.04')]:
            (self.root/'os-release').write_text(f'ID={distro}\nVERSION_ID={version}\n')
            self.state['architecture']='aarch64'
            result=self.invoke()
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(dependencies.parse_report(result.stdout.encode())['architecture'],'arm64')
        self.assertEqual(self.calls(),[])

    def test_privilege_missing_base_tool_and_identity_change(self):
        self.state['nonroot']=True
        self.assertIn('ROOT_REQUIRED',self.invoke().stderr)
        self.state={}
        self.assertIn('HOST_IDENTITY_CHANGED',self.invoke('install','b'*64).stderr)
        (self.bin/'apt-get').unlink()
        self.assertIn('BASE_TOOLS_REQUIRED',self.invoke().stderr)
        self.assertEqual(self.calls(), [])

    def test_existing_broken_package_is_not_reinstalled(self):
        (self.bin/'openssl').unlink();self.state['installed']=['openssl']
        result=self.invoke('install')
        self.assertIn('INSTALLED_PACKAGE_BROKEN',result.stderr)
        self.assertEqual(self.calls(), [])

    def test_package_failures_and_locks(self):
        (self.bin/'openssl').unlink()
        for phase, expected in [('update','APT_UPDATE_FAILED'),('simulate','APT_SIMULATION_FAILED'),('install','APT_INSTALL_FAILED')]:
            self.state={'fail':phase}
            self.assertIn(expected,self.invoke('install').stderr)
            self.assertFalse((self.root/'lock').exists())
        self.state={'fail':'install','locked':True}
        self.assertIn('APT_LOCK_TIMEOUT',self.invoke('install').stderr)
        self.state={};(self.root/'lock').mkdir()
        self.assertIn('HOST_LOCK_TIMEOUT',self.invoke('install').stderr)
        self.assertTrue((self.root/'lock').exists(), 'Must not remove another owner lock')

    def test_simulation_rejects_upgrades_and_removals(self):
        (self.bin/'openssl').unlink()
        for change in ['upgrade','remove']:
            self.state={change:True}
            result=self.invoke('install')
            self.assertIn('EXISTING_PACKAGE_CHANGE',result.stderr)
            self.assertEqual(self.calls()[-1]['mode'],'simulate')
            self.assertFalse((self.bin/'openssl').exists())

    def test_real_transaction_guard_rejects_post_simulation_race(self):
        (self.bin/'openssl').unlink();self.state={'race':True}
        result=self.invoke('install')
        self.assertIn('EXISTING_PACKAGE_CHANGE',result.stderr)
        self.assertFalse((self.bin/'openssl').exists())

    def test_install_failure_keeps_partial_state_and_missing_recheck_stops(self):
        (self.bin/'openssl').unlink();self.state={'still_missing':True}
        result=self.invoke('install')
        self.assertIn('STILL_MISSING',result.stderr)
        self.assertIn('openssl',self.state['installed'])
        self.assertFalse((self.root/'lock').exists())


class FlowTests(unittest.TestCase):
    def test_deduplicate_physical_hosts_and_require_final_readiness(self):
        ssh=FakeSSH();reports={name:ssh.dependencies('control.invalid') for name in ['one','alias']}
        with patch.object(ssh,'dependencies',return_value=reports['one']) as install:
            dependencies.prepare(ssh,reports)
            self.assertEqual(install.call_count,1)
        with patch.object(ssh,'dependencies',return_value={**reports['one'],'missingDependencies':[{'capability':'ss','package':'iproute2'}]}):
            with self.assertRaisesRegex(RuntimeError,'--install-deps'):dependencies.prepare(ssh,reports)

    def test_plan_is_partial_and_no_python_worker_when_dependencies_missing(self):
        with tempfile.TemporaryDirectory() as folder:
            config=topology(Path(folder));artifact(config['releaseRoot']/'gbf-server-0.4.0-linux-amd64')
            ssh=FakeSSH();probe=ssh.dependencies
            def missing(target,**kwargs):
                self.assertFalse(kwargs.get('install',False))
                return {**probe(target),'missingDependencies':[{'capability':'python3','package':'python3'}],'checksComplete':False}
            with patch.object(ssh,'dependencies',side_effect=missing),patch('manage.SSH',return_value=ssh),patch('manage.load_topology',return_value=config), \
                    patch('sys.argv',['manage.py','--config','unused','--plan']),contextlib.redirect_stdout(io.StringIO()) as output:
                manage.main()
            value=json.loads(output.getvalue());self.assertEqual(value['preflight'],'incomplete');self.assertIn('listeners',value['pendingChecks'])
            self.assertEqual(ssh.calls,[]);self.assertFalse(config['outputDir'].exists())

    def test_install_deps_does_not_require_release_or_touch_services(self):
        with tempfile.TemporaryDirectory() as folder:
            config=topology(Path(folder));ssh=FakeSSH()
            with patch('manage.SSH',return_value=ssh),patch('manage.load_topology',return_value=config),patch('manage.releases') as releases, \
                    patch('sys.argv',['manage.py','--config','unused','--install-deps']),contextlib.redirect_stdout(io.StringIO()):
                manage.main()
            releases.assert_not_called();self.assertEqual(ssh.calls,[])

    def test_apply_validates_release_before_install_and_stops_on_prepare_failure(self):
        with tempfile.TemporaryDirectory() as folder:
            config=topology(Path(folder));ssh=FakeSSH()
            with patch('manage.SSH',return_value=ssh),patch('manage.load_topology',return_value=config),patch('dependencies.prepare') as prepare, \
                    patch('sys.argv',['manage.py','--config','unused','--apply']):
                with self.assertRaisesRegex(ValueError,'exactly one version'):manage.main()
                prepare.assert_not_called()
                artifact(config['releaseRoot']/'gbf-server-0.4.0-linux-amd64')
                prepare.side_effect=RuntimeError('install failed')
                with self.assertRaisesRegex(RuntimeError,'install failed'):manage.main()
                self.assertEqual(ssh.calls,[])

    def test_readonly_modes_do_not_install_and_export_only_checks_control(self):
        with tempfile.TemporaryDirectory() as folder:
            config=topology(Path(folder));ssh=FakeSSH();missing={**ssh.dependencies('control.invalid'),'missingDependencies':[{'capability':'ss','package':'iproute2'}]}
            for mode in ['--verify','--export-client']:
                with patch('manage.SSH',return_value=ssh),patch('manage.load_topology',return_value=config),patch.object(ssh,'dependencies',return_value=missing) as probe, \
                        patch('dependencies.prepare') as install,patch('sys.argv',['manage.py','--config','unused',mode]):
                    with self.assertRaisesRegex(RuntimeError,'--install-deps'):manage.main()
                    install.assert_not_called()
                    if mode=='--export-client': probe.assert_called_once_with('control.invalid')

    def test_dependency_timeout_is_separate_and_errors_do_not_expose_raw_output(self):
        ssh=manage.SSH()
        with patch.object(ssh,'command',return_value=('host\tdebian\t13\tamd64\t'+'a'*64+'\nchanged\tfalse\nready\n').encode()) as command:
            ssh.dependencies('node.invalid',install=True,identity='a'*64)
            self.assertEqual(command.call_args.kwargs['timeout'],915)
            self.assertIn('895',command.call_args.args[1])
        for rc,stderr,text in [(124,b'', '15 分鐘'),(1,b'GPR-DEPS:APT_LOCK_TIMEOUT\nsecret upstream url', '套件管理鎖')]:
            with patch('manage.subprocess.run',return_value=subprocess.CompletedProcess([],rc,b'',stderr)):
                with self.assertRaisesRegex(RuntimeError,text) as error:ssh.dependencies('node.invalid')
                self.assertNotIn('secret',str(error.exception))

    def test_rollback_does_not_install_missing_dependencies(self):
        with tempfile.TemporaryDirectory() as folder:
            config=topology(Path(folder));config['outputDir'].mkdir()
            token='a'*32
            (config['outputDir']/('run-'+token+'.local.json')).write_text(json.dumps({'steps':[{'target':'control.invalid','role':'control','snapshot':'b'*32}]}))
            ssh=FakeSSH();missing={**ssh.dependencies('control.invalid'),'missingDependencies':[{'capability':'python3','package':'python3'}]}
            with patch('manage.SSH',return_value=ssh),patch('manage.load_topology',return_value=config),patch.object(ssh,'dependencies',return_value=missing) as probe, \
                    patch('dependencies.prepare') as install,patch('sys.argv',['manage.py','--config','unused','--rollback',token]):
                with self.assertRaisesRegex(RuntimeError,'--install-deps'):manage.main()
                install.assert_not_called();probe.assert_called_once_with('control.invalid');self.assertEqual(ssh.calls,[])

    def test_install_manager_stops_before_services_when_dependencies_fail(self):
        with tempfile.TemporaryDirectory() as folder:
            config=topology(Path(folder));ssh=FakeSSH()
            with patch('manage.SSH',return_value=ssh),patch('manage.load_topology',return_value=config), \
                    patch('dependencies.prepare',side_effect=RuntimeError('dependency failure')),patch('sys.argv',['manage.py','--config','unused','--install-manager']):
                with self.assertRaisesRegex(RuntimeError,'dependency failure'):manage.main()
                self.assertEqual(ssh.calls,[])

    def test_local_requirements_are_limited_to_selected_operation(self):
        with patch('dependencies.shutil.which',side_effect=lambda name: '/bin/ssh' if name=='ssh' else None):
            dependencies.check_local()
            with self.assertRaisesRegex(RuntimeError,'ssh-keyscan'):dependencies.check_local(external=True)
        with patch('dependencies.sys.version_info',(3,8)):
            with self.assertRaisesRegex(RuntimeError,'3.9'):dependencies.check_local()


if __name__=='__main__':unittest.main()
