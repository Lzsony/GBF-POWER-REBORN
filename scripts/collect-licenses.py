#!/usr/bin/env python3
"""Collect resolved dependency notices without exposing local source paths."""
import argparse
import hashlib
import json
import re
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def collect(destination):
    destination.mkdir(parents=True, exist_ok=True)
    supplemental_root = ROOT / 'docs/dependency-licenses'
    supplemental = json.loads((supplemental_root / 'manifest.json').read_text())
    fallback = {(p['ecosystem'], p['name'], p['version']): p for p in supplemental['packages']}
    for item in supplemental['packages']:
        for source in item['sources']:
            path = (supplemental_root / source['file']).resolve()
            if not path.is_relative_to(supplemental_root.resolve()):
                raise RuntimeError('Invalid supplemental source path')
            if hashlib.sha256(path.read_bytes()).hexdigest() != source['sha256']:
                raise RuntimeError('Supplemental notice checksum mismatch: ' + source['file'])
    for item in json.loads((supplemental_root / 'standard/sources.json').read_text()):
        path = supplemental_root / 'standard' / (item['license'] + '.txt')
        if hashlib.sha256(path.read_bytes()).hexdigest() != item['sha256']:
            raise RuntimeError('Standard license checksum mismatch: ' + item['license'])
    shutil.copytree(supplemental_root / 'standard', destination / 'standard', dirs_exist_ok=True)
    packages = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--format-version', '1', '--locked'], cwd=ROOT
    ))['packages']
    inventory = []

    def copy_notices(kind, name, version, license_name, folder):
        target = destination / kind / re.sub(r'[^a-zA-Z0-9._-]', '_', name + '-' + version)
        notices = []
        for path in sorted(folder.iterdir()):
            if re.match(r'^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)([._-]|$)', path.name, re.I) and path.is_file():
                target.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, target / path.name)
                notices.append(path.name)
            elif path.name.lower() == 'licenses' and path.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                shutil.copytree(path, target / path.name, dirs_exist_ok=True)
                notices.extend(str(p.relative_to(folder)) for p in path.rglob('*') if p.is_file())
        attribution = fallback.get((kind, name, version))
        if not notices and attribution:
            for name_in_manifest in attribution['files']:
                source = (supplemental_root / name_in_manifest).resolve()
                if not source.is_relative_to(supplemental_root.resolve()):
                    raise RuntimeError('Invalid supplemental notice path')
                target.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target / source.name)
                notices.append(source.name)
            (target / 'sources.json').write_text(json.dumps(attribution, ensure_ascii=False, indent=2) + '\n')
            notices.append('sources.json')
        if not notices:
            raise RuntimeError(f'Missing attribution: {kind}/{name}/{version}')
        inventory.append({'ecosystem': kind, 'name': name, 'version': version,
                          'license': license_name, 'notices': notices,
                          'supplemental': bool(attribution), 'standardTexts': 'standard/'})

    for item in packages:
        if item['source']:
            copy_notices('rust', item['name'], item['version'], item.get('license'),
                         Path(item['manifest_path']).parent)
    lock = json.loads((ROOT / 'package-lock.json').read_text())
    for path, item in sorted(lock['packages'].items()):
        if not path or item.get('dev'):
            continue
        folder = ROOT / path
        if not folder.is_dir():
            raise RuntimeError('Missing installed dependency: ' + path)
        metadata = json.loads((folder / 'package.json').read_text())
        copy_notices('npm', metadata['name'], metadata['version'], metadata.get('license'), folder)
    (destination / 'inventory.json').write_text(json.dumps(inventory, ensure_ascii=False, indent=2) + '\n')
    print(f'Collected license inventory for {len(inventory)} dependencies')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/licenses')
    collect(parser.parse_args().output.resolve())
