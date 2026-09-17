#!/usr/bin/env python3
"""Build Linux releases without copying deployment configuration or identities."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'deploy'))
from check_release import inventory, verify_server
from target_rules import read_rules


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--go', default='go')
    parser.add_argument('--arch', choices=('amd64', 'arm64', 'all'), default='all')
    parser.add_argument('--output-dir', type=Path, default=ROOT / 'artifacts/server')
    args = parser.parse_args()
    version = json.loads((ROOT / 'package.json').read_text())['version']
    root = args.output_dir.resolve()
    root.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, 'CGO_ENABLED': '0', 'GOOS': 'linux', 'GOARCH': 'amd64'}
    raw = subprocess.check_output([args.go, 'list', '-mod=readonly', '-deps', '-json', '.'], cwd=ROOT / 'server', env=env, text=True)
    modules = {}
    decoder = json.JSONDecoder()
    while raw.strip():
        item, end = decoder.raw_decode(raw.lstrip())
        module = item.get('Module', {})
        if module and not module.get('Main'):
            modules[module['Path']] = module.get('Replace', module)
        raw = raw.lstrip()[end:]
    for arch in (('amd64', 'arm64') if args.arch == 'all' else (args.arch,)):
        target = root / f'gbf-server-{version}-linux-{arch}'
        if target.exists():
            raise ValueError('Release already exists; choose a new output directory')
        with tempfile.TemporaryDirectory(prefix='.server-build-', dir=root) as temporary:
            stage = Path(temporary)
            subprocess.run([args.go, 'build', '-mod=readonly', '-trimpath', f'-ldflags=-s -w -X main.version={version}', '-o', str(stage / 'reborn'), '.'], cwd=ROOT / 'server', env={**env, 'GOARCH': arch}, check=True)
            (stage / 'rules.json').write_text(json.dumps(read_rules(ROOT / 'crates/gbf-core/src/rules.rs'), indent=2) + '\n')
            shutil.copyfile(ROOT / 'LICENSE', stage / 'LICENSE')
            (stage / 'README.md').write_text(f'# GBF POWER REBORN Server {version}\n\nLinux {arch}；Control 與 Gateway 使用獨立服務與資料。部署由管理機執行 `deploy/manage.py`，先執行 `--plan`。本套件不包含節點配置、憑證私鑰或授權資料。\n')
            for name, module in modules.items():
                folder = Path(module['Dir'])
                notices = [p for p in folder.iterdir() if p.is_file() and not p.is_symlink() and p.name.upper().startswith(('LICENSE', 'COPYING', 'NOTICE'))]
                if not notices:
                    raise ValueError(f'Missing dependency license: {name}')
                dest = stage / 'licenses' / re.sub(r'[^A-Za-z0-9._-]', '_', name + '@' + module.get('Version', ''))
                dest.mkdir(parents=True)
                for notice in notices:
                    shutil.copyfile(notice, dest / notice.name)
            goroot = Path(subprocess.check_output([args.go, 'env', 'GOROOT'], text=True).strip())
            (stage / 'licenses/go').mkdir(parents=True)
            shutil.copyfile(goroot / 'LICENSE', stage / 'licenses/go/LICENSE')
            (stage / 'manifest.json').write_text(json.dumps({'version': version, 'protocol': 1, 'architecture': arch, 'files': inventory(stage)}, indent=2) + '\n')
            verify_server(stage, arch)
            stage.rename(target)
        print(f'Verified release: {target}')


if __name__ == '__main__':
    main()
