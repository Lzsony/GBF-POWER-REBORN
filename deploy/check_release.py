"""Validate the exact inventory and architecture of a server release."""
import hashlib
import json
from pathlib import Path
import re

REQUIRED = {'reborn', 'rules.json', 'README.md', 'LICENSE'}


def inventory(root):
    result = {}
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            raise ValueError('Release may not contain symbolic links')
        if path.is_file() and path != root / 'manifest.json':
            result[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def verify_server(root, architecture=None):
    root = Path(root)
    if root.is_symlink():
        raise ValueError('Release directory must not be a symbolic link')
    manifest = json.loads((root / 'manifest.json').read_text())
    if not re.fullmatch(r'0\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?', manifest['version']):
        raise ValueError('Invalid release version')
    arch = manifest['architecture']
    if arch not in ('amd64', 'arm64') or (architecture and architecture != arch):
        raise ValueError('Unexpected server architecture')
    files = inventory(root)
    if files != manifest['files'] or not REQUIRED <= set(files):
        raise ValueError('Release inventory mismatch')
    if any(name not in REQUIRED and not name.startswith('licenses/') for name in files):
        raise ValueError('Unexpected release file')
    header = (root / 'reborn').read_bytes()[:20]
    if len(header) != 20 or header[:6] != b'\x7fELF\x02\x01' or int.from_bytes(header[18:20], 'little') != {'amd64': 62, 'arm64': 183}[arch]:
        raise ValueError('Invalid Linux ELF architecture')
    rules = json.loads((root / 'rules.json').read_text())
    if set(rules) != {'hosts', 'domains', 'ports'} or rules['ports'] != [80, 443]:
        raise ValueError('Invalid release target rules')
    for name in rules['hosts'] + rules['domains']:
        if not re.fullmatch(r'[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?', name) or '..' in name:
            raise ValueError('Invalid release target hostname')
    return manifest
