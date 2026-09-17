#!/usr/bin/env python3
"""Manage self-hosted Control and Gateway services over verified SSH."""
import argparse
import hashlib
import io
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import ssl
import subprocess
import sys
import tarfile
import uuid
from urllib.parse import urlparse
from urllib.request import build_opener, ProxyHandler, HTTPSHandler

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_release import verify_server
import dependencies

ROOT = Path(__file__).resolve().parents[1]
REMOTE = Path(__file__).with_name('remote.py')
MANAGER = Path(__file__).with_name('gpr.py')
SERVICE_BINARY = '/opt/gbf-reborn/{role}/current/reborn'
SERVICE_CONFIG = '/etc/gbf-reborn/{role}/config.json'


def validate_host(value):
    if not isinstance(value, str) or not value or any(c.isspace() for c in value):
        raise ValueError('Invalid public host')
    try: ipaddress.ip_address(value); return
    except ValueError: pass
    if len(value) > 253 or any(not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?', part) for part in value.split('.')):
        raise ValueError('Invalid public hostname')


def ssh_target(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.@:-]{0,252}', value):
        raise ValueError('Invalid management SSH target')
    return value


def port(value):
    if type(value) is not int or not 1 <= value <= 65535:
        raise ValueError('Invalid service port')
    return value


def deployment_id(value):
    reserved = {'con', 'prn', 'aux', 'nul', *(f'com{i}' for i in range(1, 10)), *(f'lpt{i}' for i in range(1, 10))}
    if not isinstance(value, str) or not re.fullmatch(r'[a-z0-9][a-z0-9_-]{0,63}', value) or value in reserved:
        raise ValueError('Invalid deploymentId: use a 1-64 character lowercase slug, excluding reserved device names')
    return value


def load_topology(path):
    value = json.loads(path.read_text())
    if set(value) != {'schemaVersion', 'deploymentId', 'releaseRoot', 'outputDir', 'control', 'gateways'} or value['schemaVersion'] != 1:
        raise ValueError('Unsupported topology fields or schema')
    deployment_id(value['deploymentId'])
    control = value['control']
    if set(control) != {'sshTarget', 'publicUrl', 'port', 'managed'} or type(control['managed']) is not bool:
        raise ValueError('Invalid Control configuration')
    ssh_target(control['sshTarget']); port(control['port'])
    url = urlparse(control['publicUrl'])
    if url.scheme != 'https' or not url.hostname or url.username or url.password or url.path not in ('', '/') or url.query or url.fragment or (url.port or 443) != control['port']:
        raise ValueError('Control requires an HTTPS origin with its configured port')
    validate_host(url.hostname)
    if not isinstance(value['gateways'], list): raise ValueError('Gateways must be a list')
    for node in value['gateways']:
        if set(node) != {'sshTarget', 'id', 'name', 'publicHost', 'port'}:
            raise ValueError('Invalid Gateway fields')
        ssh_target(node['sshTarget']); port(node['port']); validate_host(node['publicHost'])
        if not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}', node['id']): raise ValueError('Invalid Gateway ID')
        if not isinstance(node['name'], str) or not node['name'] or len(node['name'].encode()) > 160 or any(ord(c) < 32 for c in node['name']):
            raise ValueError('Invalid Gateway name')
    for key in ('releaseRoot', 'outputDir'):
        value[key] = (path.parent / value[key]).resolve()
    return value


class SSH:
    def command(self, target, arguments, data=None, *, timeout=300, dependency=False):
        args = ['ssh', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'UpdateHostKeys=no', '-o', 'ConnectTimeout=15', '--', ssh_target(target), shlex.join([str(a) for a in arguments])]
        try:
            result = subprocess.run(args, input=data, capture_output=True, timeout=timeout)
        except (OSError, subprocess.TimeoutExpired):
            if dependency: raise RuntimeError('依賴準備的 SSH 連線失敗或超時；遠端安裝最長 15 分鐘，請檢查套件狀態後重試。') from None
            raise RuntimeError('Management SSH was unavailable or timed out') from None
        if result.returncode:
            if dependency:
                if result.returncode in (124, 137):
                    raise RuntimeError('依賴準備超過 15 分鐘；已停止，請檢查 dpkg 狀態後再執行 --install-deps。')
                marker = re.search(rb'GPR-DEPS:([A-Z_]+)', result.stderr)
                reason = dependencies.ERRORS.get(marker[1].decode()) if marker else None
                log = re.search(rb'^log\t(/var/log/gbf-reborn-dependencies/install\.[A-Za-z0-9]+)$', result.stdout, re.M)
                raise RuntimeError((reason or '依賴檢查失敗；請檢查管理 SSH、sudo -n 及基本系統工具。') +
                    (' 伺服器日誌：' + log[1].decode() if log else ''))
            # Do not expose child output: admin/join may handle enrollment tickets.
            raise RuntimeError('Verified SSH operation failed; check host prerequisites and service status')
        return result.stdout

    def dependencies(self, target, install=False, identity=None):
        command = ['sudo', '-n']
        if install:
            if not isinstance(identity, str) or not re.fullmatch('[0-9a-f]{64}', identity):
                raise ValueError('Missing verified dependency host identity')
            command += ['timeout', '--kill-after=5s', '895']
        command += ['/bin/sh', '-s', '--', 'install' if install else 'probe']
        if install: command.append(identity)
        result = self.command(target, command, dependencies.BOOTSTRAP.read_bytes(),
            timeout=915 if install else 300, dependency=True)
        return dependencies.parse_report(result)

    def worker(self, target, request):
        result = self.command(target, ['sudo', '-n', 'python3', '-c', REMOTE.read_text()], json.dumps(request).encode())
        return json.loads(result)

    def admin(self, target, request):
        return json.loads(self.command(target, ['sudo', '-n', '-u', 'gbf-control', SERVICE_BINARY.format(role='control'), 'admin', '--socket', '/run/gbf-control/admin.sock', '--json'], json.dumps(request).encode()))

    def certificate(self, target):
        value = self.command(target, ['sudo', '-n', 'cat', '/etc/gbf-reborn/control/tls.crt']).decode()
        ssl.PEM_cert_to_DER_cert(value)
        return value

    def upload(self, target, root):
        destination = '/tmp/gbf-release-' + uuid.uuid4().hex
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode='w') as archive:
            for path in sorted(root.rglob('*')):
                if path.is_file(): archive.add(path, arcname=path.relative_to(root), recursive=False)
        code = """import os,sys,tarfile,pathlib
root=pathlib.Path(sys.argv[1]);root.mkdir(mode=0o700)
with tarfile.open(fileobj=sys.stdin.buffer,mode='r|') as archive:
 for member in archive:
  path=root/member.name
  if not member.isfile() or member.name.startswith('/') or '..' in pathlib.PurePosixPath(member.name).parts: raise ValueError('Invalid release archive')
  path.parent.mkdir(parents=True,exist_ok=True)
  with path.open('xb') as output: output.write(archive.extractfile(member).read())
"""
        self.command(target, ['sudo', '-n', 'python3', '-c', code, destination], stream.getvalue())
        return destination

    def cleanup(self, target, destination):
        if not re.fullmatch('/tmp/gbf-release-[0-9a-f]{32}', destination): raise ValueError('Invalid temporary release path')
        self.command(target, ['sudo', '-n', 'rm', '-rf', '--', destination])

    def external(self, topology, groups, certificate):
        context = ssl.create_default_context(cadata=certificate)
        opener = build_opener(ProxyHandler({}), HTTPSHandler(context=context))
        with opener.open(topology['control']['publicUrl'].rstrip('/') + '/health/live', timeout=10) as response:
            if response.status != 200 or json.load(response).get('ok') is not True:
                raise ValueError('Control public TLS health check failed')
        for group in groups:
            node = group['roles'].get('gateway')
            if node is None: continue
            public = json.loads(self.command(group['target'], ['sudo', '-n', '-u', 'gbf-gateway', SERVICE_BINARY.format(role='gateway'), 'public-keys', '--config', SERVICE_CONFIG.format(role='gateway')]))
            result = subprocess.run(['ssh-keyscan', '-T', '5', '-t', 'ed25519', '-p', str(node['port']), node['publicHost']], capture_output=True, text=True, timeout=15)
            expected = ' '.join(public['hostKey'].split()[:2])
            observed = {' '.join(line.split()[1:3]) for line in result.stdout.splitlines() if not line.startswith('#') and len(line.split()) >= 3}
            if result.returncode or expected not in observed:
                raise ValueError('Gateway public SSH host key does not match its installed identity')
        return {'controlTls': 'passed', 'gatewayHostKeys': 'passed'}

    def join(self, target, token):
        self.command(target, ['sudo', '-n', '-u', 'gbf-gateway', SERVICE_BINARY.format(role='gateway'), 'join', '--config', SERVICE_CONFIG.format(role='gateway')], token.encode())


def group_hosts(topology, reports):
    """Deduplicate verified physical hosts, never SSH alias strings alone."""
    groups = {}
    ids = {}
    entries = [('control', topology['control'])] + [('gateway', node) for node in topology['gateways']]
    for role, item in entries:
        target = item['sshTarget']; report = reports[target]; identity = report['machineIdHash']
        if not re.fullmatch('[0-9a-f]{64}', identity): raise ValueError('Invalid physical host fingerprint')
        group = groups.setdefault(identity, {'target': target, 'machineIdHash': identity, 'aliases': [], 'architecture': report['architecture'], 'roles': {}})
        if target not in group['aliases']: group['aliases'].append(target)
        if group['architecture'] != report['architecture']: raise ValueError('Aliases returned inconsistent host facts')
        spec = {key: value for key, value in item.items() if key != 'sshTarget'}
        if role in group['roles'] and group['roles'][role] != spec:
            raise ValueError('A physical host may have only one Gateway ID and configuration')
        group['roles'][role] = spec
        if role == 'gateway':
            if item['id'] in ids and ids[item['id']] != identity: raise ValueError('Gateway ID is assigned to multiple physical hosts')
            ids[item['id']] = identity
            existing = report.get('roles', {}).get('gateway', {})
            if existing.get('nodeId') and existing['nodeId'] != item['id']: raise ValueError('Host already has a different Gateway ID')
        if report.get('listeners') is not None and item['port'] in report['listeners'] and not (report.get('services', {}).get(role, {}).get('active') and report.get('roles', {}).get(role, {}).get('port') == item['port']):
            raise ValueError('Requested service port is occupied')
    for group in groups.values():
        if len(group['roles']) == 2 and group['roles']['control']['port'] == group['roles']['gateway']['port']:
            raise ValueError('Control and Gateway must use different ports on one host')
    return list(groups.values())


def configs(topology, group, role):
    spec = group['roles'][role]
    if role == 'control':
        return {'listen': ':' + str(spec['port']), 'publicUrl': spec['publicUrl'].rstrip('/'), 'dataDir': '/var/lib/gbf-reborn/control', 'masterKey': '/etc/gbf-reborn/control/master.key', 'adminSocket': '/run/gbf-control/admin.sock', 'tlsCert': '/etc/gbf-reborn/control/tls.crt', 'tlsKey': '/etc/gbf-reborn/control/tls.key'}
    return {'id': spec['id'], 'name': spec['name'], 'listen': ':' + str(spec['port']), 'host': spec['publicHost'], 'port': spec['port'], 'username': 'reborn', 'controlUrl': topology['control']['publicUrl'].rstrip('/'), 'controlCa': '/etc/gbf-reborn/gateway/control.crt', 'hostKey': '/etc/gbf-reborn/gateway/host.key', 'identityKey': '/etc/gbf-reborn/gateway/identity.key', 'rules': '/etc/gbf-reborn/gateway/rules.json', 'statsFile': '/var/lib/gbf-reborn/gateway/stats.json'}


def preflight(topology, ssh):
    targets = dict.fromkeys([topology['control']['sshTarget']] + [node['sshTarget'] for node in topology['gateways']])
    reports = {target: ssh.worker(target, {'action': 'preflight'}) for target in targets}
    groups = group_hosts(topology, reports)
    if not topology['control']['managed'] and not reports[topology['control']['sshTarget']]['services']['control']['active']:
        raise ValueError('Gateway-only deployment requires an active managed Control service')
    return groups


def dependency_reports(topology, ssh):
    targets = dict.fromkeys([topology['control']['sshTarget']] + [node['sshTarget'] for node in topology['gateways']])
    return {target: ssh.dependencies(target) for target in targets}


def checked_preflight(topology, ssh, reports):
    groups = preflight(topology, ssh)
    for group in groups:
        for target in group['aliases']:
            if reports[target]['machineIdHash'] != group['machineIdHash'] or reports[target]['architecture'] != group['architecture']:
                raise RuntimeError('Host identity or architecture changed after dependency discovery')
    return groups


def releases(topology, groups):
    found = {}
    for architecture in {group['architecture'] for group in groups}:
        candidates = []
        for path in topology['releaseRoot'].glob('gbf-server-*-linux-' + architecture):
            manifest = verify_server(path, architecture)
            candidates.append((path, manifest))
        if len(candidates) != 1: raise ValueError('Release root must contain exactly one version per required architecture')
        found[architecture] = candidates[0]
    if len({manifest['version'] for _, manifest in found.values()}) != 1: raise ValueError('All deployment architectures must use the same version')
    return found


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + '.pending')
    with temporary.open('w', encoding='utf-8', newline='\n') as stream:
        json.dump(value, stream, indent=2); stream.write('\n'); stream.flush(); os.fsync(stream.fileno())
    os.replace(temporary, path)


def export_client(topology, certificate):
    certificate = certificate.replace('\r\n', '\n').replace('\r', '\n').strip() + '\n'
    ssl.PEM_cert_to_DER_cert(certificate)
    profile = {'deploymentId': deployment_id(topology['deploymentId']), 'url': topology['control']['publicUrl'].rstrip('/'), 'caCertificateFile': 'control-public.crt'}
    topology['outputDir'].mkdir(parents=True, exist_ok=True)
    (topology['outputDir'] / 'control-public.crt').write_bytes(certificate.encode('ascii'))
    destination = topology['outputDir'] / 'client-build.local.json'
    write_json(destination, profile)
    return destination


def deployment_plan(groups):
    return [{'machineIdHash': group['machineIdHash'], 'sshTargets': group['aliases'], 'architecture': group['architecture'], 'roles': list(group['roles']), 'requiredInboundTcpPorts': sorted(spec['port'] for spec in group['roles'].values())} for group in groups]


def install_managers(groups, ssh):
    source = MANAGER.read_text()
    digest = hashlib.sha256(source.encode()).hexdigest()
    results = []
    for group in groups:
        try:
            results.append(ssh.worker(group['target'], {'action': 'install-manager', 'machineIdHash': group['machineIdHash'],
                'source': source, 'sha256': digest}))
        except Exception:
            raise RuntimeError('Manager installation failed on ' + group['target'] +
                '; check SSH/sudo, /usr/local/bin ownership and any existing gpr file or symlink. Resolve the conflict and retry --install-manager; services were not restarted.') from None
    return results


def apply(topology, groups, artifacts, ssh, add_node=None):
    run_id = uuid.uuid4().hex
    journal_path = topology['outputDir'] / ('run-' + run_id + '.local.json')
    journal = {'id': run_id, 'steps': [], 'state': 'installing'}
    write_json(journal_path, journal)
    control = next(group for group in groups if 'control' in group['roles'])

    def install(group, role, certificate=None):
        root, manifest = artifacts[group['architecture']]
        stage = ssh.upload(group['target'], root)
        try:
            request = {'action': 'apply', 'machineIdHash': group['machineIdHash'], 'role': role, 'config': configs(topology, group, role), 'release': stage}
            if role == 'control': request['publicHost'] = urlparse(topology['control']['publicUrl']).hostname
            else: request['controlCertificate'] = certificate
            result = ssh.worker(group['target'], request)
            journal['steps'].append({'target': group['target'], 'machineIdHash': group['machineIdHash'], 'role': role, 'snapshot': result['snapshot']})
            write_json(journal_path, journal)
        finally: ssh.cleanup(group['target'], stage)

    try:
        if topology['control']['managed'] and not add_node: install(control, 'control')
        certificate = ssh.certificate(control['target'])
        for group in groups:
            if 'gateway' in group['roles'] and (add_node is None or group['roles']['gateway']['id'] == add_node): install(group, 'gateway', certificate)
    except Exception:
        journal['state'] = 'install-failed'
        journal['rollbackFailures'] = rollback_steps(journal['steps'], ssh)
        write_json(journal_path, journal)
        raise RuntimeError('Installation failed. Rollback record: ' + journal_path.name + ('; some hosts require rollback retry' if journal['rollbackFailures'] else '; prior installations restored')) from None
    # Installation is durable before enrollment. A failed/expired ticket can be
    # retried with a fresh ticket without deleting server-generated identities.
    journal['state'] = 'enrolling'; write_json(journal_path, journal)
    try:
        for group in groups:
            node = group['roles'].get('gateway')
            if node is None or (add_node is not None and node['id'] != add_node): continue
            snapshot = ssh.admin(control['target'], {'action': 'snapshot'})
            existing = next((item for item in snapshot['nodes'] if item['id'] == node['id']), None)
            if existing and (existing['host'], existing['port']) != (node['publicHost'], node['port']): raise ValueError('Gateway ID belongs to a different public endpoint')
            if existing and existing.get('registered'): token = ''
            elif existing: token = ssh.admin(control['target'], {'action': 'node-ticket', 'id': node['id']})['token']
            else: token = ssh.admin(control['target'], {'action': 'node-add', 'id': node['id'], 'name': node['name'], 'host': node['publicHost'], 'port': node['port'], 'capacityBps': 0})['token']
            try: ssh.join(group['target'], token)
            finally: token = ''
            ssh.worker(group['target'], {'action': 'start', 'machineIdHash': group['machineIdHash'], 'role': 'gateway'})
        selected = [group for group in groups if add_node is None or 'control' in group['roles'] or group['roles'].get('gateway', {}).get('id') == add_node]
        ssh.external(topology, selected, certificate)
        profile_path = export_client(topology, certificate)
    except Exception:
        journal['state'] = 'enrollment-pending'; write_json(journal_path, journal)
        raise RuntimeError('Enrollment or verification incomplete. Correct connectivity or ticket validity and rerun --apply; identities and pending nodes are retained. Rollback record: ' + journal_path.name) from None
    try:
        install_managers(selected, ssh)
    except Exception:
        journal['state'] = 'manager-install-failed'; write_json(journal_path, journal)
        raise RuntimeError('Services are deployed and verified; manager installation failed. Resolve /usr/local/bin/gpr ownership or name conflicts, then rerun --install-manager. Services were not rolled back. Journal: ' + journal_path.name) from None
    journal['state'] = 'complete'; write_json(journal_path, journal)
    return {'runId': run_id, 'clientProfile': str(profile_path), 'newGatewayState': 'pending', 'requiredPorts': deployment_plan(groups)}


def rollback_steps(steps, ssh):
    failures = []
    for step in reversed(steps):
        try: ssh.worker(step['target'], {'action': 'rollback', 'machineIdHash': step['machineIdHash'], 'snapshot': step['snapshot']})
        except Exception: failures.append({'role': step['role'], 'machineIdHash': step['machineIdHash']})
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--plan', action='store_true')
    mode.add_argument('--apply', action='store_true')
    mode.add_argument('--verify', action='store_true')
    mode.add_argument('--export-client', action='store_true')
    mode.add_argument('--install-manager', action='store_true', help='Install gpr without updating or restarting services')
    mode.add_argument('--install-deps', action='store_true', help='Install only missing Linux prerequisites; no release or service changes')
    mode.add_argument('--rollback', metavar='RUN_ID')
    parser.add_argument('--add-node', metavar='NODE_ID')
    args = parser.parse_args()
    dependencies.check_local(external=args.apply or args.verify)
    topology = load_topology(args.config.resolve())
    ssh = SSH()
    if args.add_node and (not args.apply or args.add_node not in {node['id'] for node in topology['gateways']}):
        raise ValueError('--add-node requires --apply and a Gateway ID in the topology')
    if args.export_client:
        dependencies.require_ready({topology['control']['sshTarget']: ssh.dependencies(topology['control']['sshTarget'])})
        certificate = ssh.certificate(topology['control']['sshTarget'])
        print(json.dumps({'clientProfile': str(export_client(topology, certificate))})); return
    if args.rollback:
        if not re.fullmatch('[0-9a-f]{32}', args.rollback): raise ValueError('Invalid rollback run ID')
        journal = json.loads((topology['outputDir'] / ('run-' + args.rollback + '.local.json')).read_text())
        dependencies.require_ready({target: ssh.dependencies(target) for target in dict.fromkeys(step['target'] for step in journal['steps'])})
        failures = rollback_steps(journal['steps'], ssh)
        if failures: raise RuntimeError('Some hosts could not roll back; rerun the same rollback ID')
        print(json.dumps({'rolledBack': args.rollback})); return
    reports = dependency_reports(topology, ssh)
    # These groups know topology/identity/architecture only, not listener state.
    discovered = group_hosts(topology, reports)
    artifacts = releases(topology, discovered) if args.apply or args.plan else None
    dependency_plan = [{'sshTarget': target, **report} for target, report in reports.items()]
    if args.plan and any(report['missingDependencies'] for report in reports.values()):
        print(json.dumps({'plan': deployment_plan(discovered), 'dependencies': dependency_plan,
            'preflight': 'incomplete', 'pendingChecks': ['listeners', 'installedRoles', 'serviceConfiguration'],
            'version': next(iter(artifacts.values()))[1]['version'], 'firewallChanges': False}, indent=2)); return
    if args.apply or args.install_manager or args.install_deps:
        reports = dependencies.prepare(ssh, reports)
    else:
        dependencies.require_ready(reports)
    if args.install_deps:
        print(json.dumps({'dependencies': [{'sshTarget': target, **report} for target, report in reports.items()]})); return
    groups = checked_preflight(topology, ssh, reports)
    if args.install_manager:
        print(json.dumps({'managers': install_managers(groups, ssh)})); return
    if args.verify:
        for group in groups:
            for role in group['roles']: ssh.worker(group['target'], {'action': 'verify', 'machineIdHash': group['machineIdHash'], 'role': role})
        control = next(group for group in groups if 'control' in group['roles'])
        external = ssh.external(topology, groups, ssh.certificate(control['target']))
        print(json.dumps({'verifiedHosts': len(groups), 'external': external})); return
    if args.plan:
        print(json.dumps({'plan': deployment_plan(groups), 'dependencies': dependency_plan, 'preflight': 'complete', 'version': next(iter(artifacts.values()))[1]['version'], 'newGatewayState': 'pending', 'firewallChanges': False}, indent=2)); return
    print(json.dumps(apply(topology, groups, artifacts, ssh, args.add_node), indent=2))


if __name__ == '__main__':
    try: main()
    except Exception as error:
        print(str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__, file=sys.stderr)
        sys.exit(1)
