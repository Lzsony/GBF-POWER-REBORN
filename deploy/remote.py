"""Root-only remote worker. JSON requests arrive on stdin; responses contain no secrets."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import socket
import subprocess
import sys
import time
import uuid

BASE = Path('/etc/gbf-reborn')
LIB = Path('/opt/gbf-reborn')
STATE = Path('/var/lib/gbf-reborn-deploy')
LOCK = Path('/run/lock/gbf-reborn-deploy.lock')
UNITS = Path('/etc/systemd/system')
ROLES = ('control', 'gateway')


def run(args, check=True):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=120)
    if check and result.returncode:
        raise RuntimeError('Remote operation failed: ' + Path(str(args[0])).name)
    return result


def read_json(path, default):
    return json.loads(path.read_text()) if path.exists() else default


def atomic(path, value, mode=0o600):
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_symlink():
        raise ValueError('Refusing symlink output')
    temporary = path.with_name(path.name + '.pending-' + uuid.uuid4().hex)
    with temporary.open('x') as stream:
        stream.write(value)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.chmod(mode)
    os.replace(temporary, path)


def identity():
    raw = Path('/etc/machine-id').read_text().strip().lower()
    if not re.fullmatch('[0-9a-f]{32}', raw) or raw == '0' * 32:
        raise ValueError('Missing stable machine identity')
    return hashlib.sha256(raw.encode()).hexdigest()


def service_state(role):
    unit = 'gbf-' + role + '.service'
    return {'active': run(['systemctl', 'is-active', '--quiet', unit], False).returncode == 0,
            'enabled': run(['systemctl', 'is-enabled', '--quiet', unit], False).returncode == 0}


def platform_facts(os_text, machine):
    os_info = dict(line.split('=', 1) for line in os_text.splitlines() if '=' in line)
    distro, version = (os_info.get(k, '').strip('"') for k in ('ID', 'VERSION_ID'))
    if (distro, version) not in {('debian', '12'), ('debian', '13'), ('ubuntu', '24.04'), ('ubuntu', '26.04')}:
        raise ValueError('Unsupported Linux distribution')
    if machine not in ('x86_64', 'aarch64'):
        raise ValueError('Unsupported CPU architecture')
    return distro, version, {'x86_64': 'amd64', 'aarch64': 'arm64'}[machine]


def preflight():
    if os.geteuid() != 0:
        raise ValueError('Passwordless sudo is required for deployment')
    distro, version, architecture = platform_facts(Path('/etc/os-release').read_text(), os.uname().machine)
    if not Path('/run/systemd/system').is_dir():
        raise ValueError('A running systemd host is required')
    for binary in ('systemctl', 'openssl', 'ssh-keygen', 'ss', 'useradd', 'runuser', 'tar'):
        if not shutil.which(binary):
            raise ValueError('Missing host prerequisite: ' + binary)
    for path in (BASE, LIB, STATE):
        if path.is_symlink():
            raise ValueError('Managed root must not be a symlink')
    listeners = set()
    for line in run(['ss', '-ltnH']).stdout.splitlines():
        fields = line.split()
        if len(fields) >= 4:
            port = fields[3].rsplit(':', 1)[-1]
            if port.isdigit(): listeners.add(int(port))
    return {'machineIdHash': identity(), 'distribution': distro, 'version': version,
            'architecture': architecture,
            'listeners': sorted(listeners), 'roles': read_json(STATE / 'state.json', {}),
            'services': {role: service_state(role) for role in ROLES}}


def unit_text(role):
    return f'''[Unit]
Description=GBF POWER REBORN {role.title()}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=gbf-{role}
Group=gbf-{role}
ExecStart={LIB}/{role}/current/reborn {role} --config {BASE}/{role}/config.json
Restart=on-failure
RestartSec=5
RuntimeDirectory=gbf-{role}
RuntimeDirectoryMode=0700
UMask=0077
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/gbf-reborn/{role} /run/gbf-{role}
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
'''


def ensure_directories(role):
    user = 'gbf-' + role
    try: account = pwd.getpwnam(user)
    except KeyError:
        run(['useradd', '--system', '--user-group', '--home-dir', '/var/lib/gbf-reborn/' + role, '--shell', '/usr/sbin/nologin', user])
        account = pwd.getpwnam(user)
    for path in (BASE / role, Path('/var/lib/gbf-reborn') / role):
        if path.is_symlink(): raise ValueError('Service data must not be a symlink')
        path.mkdir(parents=True, exist_ok=True)
        path.chmod(0o700)
        os.chown(path, account.pw_uid, account.pw_gid)
    for path in (LIB / role, LIB / role / 'releases'):
        if path.is_symlink(): raise ValueError('Service executable directory must not be a symlink')
        path.mkdir(parents=True, exist_ok=True)
    return account


def snapshot(role):
    token = uuid.uuid4().hex
    root = STATE / 'backups' / token
    root.mkdir(parents=True, mode=0o700)
    paths = {'config': BASE / role, 'unit': UNITS / ('gbf-' + role + '.service')}
    for label, path in paths.items():
        if path.is_symlink(): raise ValueError('Unexpected managed symlink')
        if path.is_dir(): shutil.copytree(path, root / label)
        elif path.exists(): shutil.copyfile(path, root / label)
    current = LIB / role / 'current'
    record = {'role': role, 'previous': os.readlink(current) if current.is_symlink() else None,
              'service': service_state(role), 'state': read_json(STATE / 'state.json', {}).get(role)}
    atomic(root / 'snapshot.json', json.dumps(record))
    return token


def restore(token):
    if not re.fullmatch('[0-9a-f]{32}', token): raise ValueError('Invalid rollback identifier')
    root = STATE / 'backups' / token
    record = read_json(root / 'snapshot.json', {})
    role = record.get('role')
    if role not in ROLES: raise ValueError('Invalid rollback snapshot')
    run(['systemctl', 'stop', 'gbf-' + role], False)
    for label, path in {'config': BASE / role, 'unit': UNITS / ('gbf-' + role + '.service')}.items():
        backup = root / label
        if backup.is_dir():
            # Existing roles recover the exact snapshot. First installations
            # retain only locally generated identities so enrollment can retry.
            retained = set()
            if record['state'] is None:
                retained = {'master.key', 'tls.key', 'tls.crt'} if role == 'control' else {'host.key', 'identity.key'}
            owner = path.stat()
            expected = {item.name for item in backup.iterdir()}
            for item in path.iterdir():
                if item.name not in expected and item.name not in retained:
                    if item.is_dir() and not item.is_symlink(): shutil.rmtree(item)
                    else: item.unlink()
            for item in backup.iterdir():
                destination = path / item.name
                if destination.is_symlink(): destination.unlink()
                if item.is_dir():
                    if destination.exists(): shutil.rmtree(destination)
                    shutil.copytree(item, destination)
                else: shutil.copy2(item, destination)
                for copied in [destination] + (list(destination.rglob('*')) if destination.is_dir() else []):
                    os.chown(copied, owner.st_uid, owner.st_gid)
        elif backup.exists(): shutil.copy2(backup, path)
        elif label == 'unit' and path.exists(): path.unlink()
    current = LIB / role / 'current'
    if current.is_symlink(): current.unlink()
    if record['previous']:
        current.symlink_to(record['previous'])
    registry = read_json(STATE / 'state.json', {})
    if record['state'] is not None: registry[role] = record['state']
    elif role == 'gateway' and role in registry:
        registry[role]['installed'] = False
    else: registry.pop(role, None)
    atomic(STATE / 'state.json', json.dumps(registry))
    run(['systemctl', 'daemon-reload'])
    run(['systemctl', 'enable' if record['service']['enabled'] else 'disable', 'gbf-' + role], False)
    if record['service']['active']: run(['systemctl', 'start', 'gbf-' + role])
    return {'rolledBack': token, 'role': role}


def validate_role(request, report):
    role, config = request['role'], request['config']
    if role not in ROLES: raise ValueError('Invalid service role')
    old = report['roles'].get(role, {})
    if role == 'gateway' and old.get('nodeId') and old['nodeId'] != config['id']:
        raise ValueError('Physical host is already bound to another Gateway ID')
    port = int(config['listen'].rsplit(':', 1)[1])
    if port in report['listeners'] and not (report['services'][role]['active'] and old.get('port') == port):
        raise ValueError('Requested port is already in use')


def verify(role):
    if role not in ROLES: raise ValueError('Invalid service role')
    if not service_state(role)['active']: raise ValueError('Service is not active: ' + role)
    binary = LIB / role / 'current/reborn'
    result = run(['runuser', '-u', 'gbf-' + role, '--', binary, 'doctor', '--role', role, '--config', BASE / role / 'config.json'])
    diagnostics = json.loads(result.stdout)
    required = {'configuration', 'listener', 'master-key', 'tls-key', 'tls-certificate', 'database'} if role == 'control' else {'configuration', 'listener', 'ssh-host-key', 'node-identity', 'control-node-authentication', 'target-whitelist'}
    checks = {item['check']: item['status'] for item in diagnostics}
    if any(checks.get(name) != 'passed' for name in required):
        raise ValueError('Required service health checks failed: ' + role)
    return {'role': role, 'verified': True, 'checks': checks}


def wait_verified(role):
    for attempt in range(20):
        try: return verify(role)
        except (RuntimeError, ValueError):
            if attempt == 19: raise
            time.sleep(0.25)


def apply(request, report):
    role, config = request['role'], request['config']
    validate_role(request, report)
    release = Path(request['release'])
    if not str(release).startswith('/tmp/gbf-release-') or release.is_symlink():
        raise ValueError('Invalid staging path')
    # Independently verify the binary and rules after SSH transfer, before service mutation.
    manifest = read_json(release / 'manifest.json', {})
    if manifest.get('architecture') != report['architecture']:
        raise ValueError('Release architecture mismatch')
    version = manifest.get('version', '')
    if not re.fullmatch(r'0\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?', version): raise ValueError('Invalid version')
    for name in ('reborn', 'rules.json'):
        file = release / name
        if file.is_symlink() or hashlib.sha256(file.read_bytes()).hexdigest() != manifest['files'].get(name):
            raise ValueError('Uploaded release checksum mismatch')
    header = (release / 'reborn').read_bytes()[:20]
    if len(header) != 20 or header[:6] != b'\x7fELF\x02\x01' or int.from_bytes(header[18:20], 'little') != {'amd64': 62, 'arm64': 183}[report['architecture']]:
        raise ValueError('Uploaded binary architecture mismatch')
    account = ensure_directories(role)
    STATE.mkdir(parents=True, exist_ok=True, mode=0o700)
    token = snapshot(role)
    try:
        target = LIB / role / 'releases' / version
        if target.exists():
            if target.is_symlink() or hashlib.sha256((target / 'reborn').read_bytes()).hexdigest() != manifest['files']['reborn']:
                raise ValueError('Existing version has different executable bytes')
        else:
            temporary = target.with_name('.release-' + uuid.uuid4().hex)
            temporary.mkdir(mode=0o755)
            try:
                shutil.copyfile(release / 'reborn', temporary / 'reborn')
                (temporary / 'reborn').chmod(0o755)
                temporary.rename(target)
            finally:
                if temporary.exists(): shutil.rmtree(temporary)
        binary = target / 'reborn'
        if role == 'control':
            run(['runuser', '-u', 'gbf-control', '--', binary, 'init-control', '--dir', BASE / role, '--host', request['publicHost']])
        else:
            atomic(BASE / role / 'rules.json', (release / 'rules.json').read_text(), 0o644)
            atomic(BASE / role / 'control.crt', request['controlCertificate'], 0o644)
        atomic(BASE / role / 'config.json', json.dumps(config))
        for path in (BASE / role).iterdir():
            if path.is_symlink(): raise ValueError('Unexpected identity symlink')
            os.chown(path, account.pw_uid, account.pw_gid)
        if role == 'gateway': run(['runuser', '-u', 'gbf-gateway', '--', binary, 'init-gateway', '--config', BASE / role / 'config.json'])
        run(['runuser', '-u', 'gbf-' + role, '--', binary, 'check-config', '--role', role, '--config', BASE / role / 'config.json'])
        atomic(UNITS / ('gbf-' + role + '.service'), unit_text(role), 0o644)
        pending = LIB / role / ('current-' + uuid.uuid4().hex)
        pending.symlink_to(target)
        os.replace(pending, LIB / role / 'current')
        registry = read_json(STATE / 'state.json', {})
        registry[role] = {'port': int(config['listen'].rsplit(':', 1)[1]), 'version': version, 'installed': True}
        if role == 'gateway': registry[role]['nodeId'] = config['id']
        atomic(STATE / 'state.json', json.dumps(registry))
        run(['systemctl', 'daemon-reload'])
        run(['systemctl', 'enable', 'gbf-' + role])
        if role == 'control':
            run(['systemctl', 'restart', 'gbf-control'])
            wait_verified(role)
        else: run(['systemctl', 'stop', 'gbf-gateway'], False)
        return {'role': role, 'snapshot': token, 'version': version}
    except Exception:
        restore(token)
        raise


def main():
    request = json.load(sys.stdin)
    if request['action'] == 'preflight':
        print(json.dumps(preflight())); return
    LOCK.parent.mkdir(parents=True, exist_ok=True)
    with LOCK.open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        report = preflight()
        if report['machineIdHash'] != request['machineIdHash']:
            raise ValueError('Physical host identity changed after preflight')
        action = request['action']
        if action == 'apply': result = apply(request, report)
        elif action == 'rollback': result = restore(request['snapshot'])
        elif action == 'start':
            if request['role'] not in ROLES: raise ValueError('Invalid service role')
            run(['systemctl', 'restart', 'gbf-' + request['role']]); result = wait_verified(request['role'])
        elif action == 'verify': result = verify(request['role'])
        else: raise ValueError('Invalid remote action')
        print(json.dumps(result))


if __name__ == '__main__':
    try: main()
    except Exception as error:
        # Exceptions from subprocesses are never interpolated (they may contain stdin).
        print(json.dumps({'error': str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__}), file=sys.stderr)
        sys.exit(1)
