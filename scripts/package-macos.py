#!/usr/bin/env python3
"""Build and verify the arm64 App/DMG; never install the app or reset user data."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / 'artifacts/macos'
MARKER = b'GBF-INTERNAL-TEST-BUILD-DO-NOT-DISTRIBUTE'


def run(*args, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def inventory(folder):
    entries = {}
    for item in sorted(folder.rglob('*')):
        name = str(item.relative_to(folder))
        if item.is_symlink():
            if not item.resolve().is_relative_to(folder.resolve()):
                raise RuntimeError('External bundle link: ' + name)
            entries[name] = {'link': os.readlink(item)}
        elif item.is_file():
            entries[name] = {'sha256': digest(item), 'bytes': item.stat().st_size}
    return entries


def source_inventory():
    names = run('git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z', capture_output=True).stdout.decode().split('\0')
    return {name: digest(ROOT / name) for name in sorted(set(names)) if name and (ROOT / name).is_file()}


def verify_app(app):
    info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
    assert info['CFBundleIdentifier'] == 'cc.lzsony.gbf-power-reborn'
    assert info['CFBundleShortVersionString'] == '0.2.0'
    assert info['LSMinimumSystemVersion'] == '13.0'
    binary = app / 'Contents/MacOS' / info['CFBundleExecutable']
    assert run('lipo', '-archs', str(binary), capture_output=True, text=True).stdout.strip() == 'arm64'
    assert MARKER not in binary.read_bytes(), 'Refusing internal-test binary'
    for name in ['LICENSE', 'OFL-NotoSansTC.txt', 'THIRD_PARTY_NOTICES.md', 'inventory.json']:
        assert list((app / 'Contents/Resources').rglob(name)), 'Missing license resource: ' + name
    for pattern in ['config.json', 'control.json', '*.key', '*.pem']:
        assert not list(app.rglob(pattern)), 'Runtime data in bundle: ' + pattern
    run('codesign', '--verify', '--deep', '--strict', '--verbose=2', str(app))
    signature = run('codesign', '-dv', str(app), capture_output=True, text=True).stderr
    assert 'Signature=adhoc' in signature, 'Expected ad-hoc signature'
    return inventory(app)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check-only', action='store_true')
    args = parser.parse_args()
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise SystemExit('Requires an Apple Silicon Mac')
    for tool in ['node', 'npm', 'cargo', 'rustc', 'xcodebuild', 'hdiutil', 'codesign', 'lipo']:
        if not shutil.which(tool):
            raise SystemExit('Missing tool: ' + tool)
    assert int(run('node', '--version', capture_output=True, text=True).stdout.strip()[1:].split('.')[0]) >= 22
    run('xcode-select', '-p')
    if args.check_only:
        print('PASS: macOS arm64 build prerequisites')
        return
    OUTPUT.mkdir(parents=True, exist_ok=True)
    lock = OUTPUT / '.package.lock'
    try:
        fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        raise SystemExit('Packaging lock exists; inspect the active process before retrying')
    with os.fdopen(fd, 'w') as stream:
        stream.write(str(os.getpid()))
    try:
        run('npm', 'ci')
        run('python3', 'scripts/collect-licenses.py')
        sources = source_inventory()
        with tempfile.TemporaryDirectory(prefix='.package-', dir=OUTPUT) as temporary:
            stage = Path(temporary)
            bundle = ROOT / 'target/release/bundle'
            for name in ['macos', 'dmg']:
                previous_build = bundle / name
                if previous_build.exists():
                    previous_build.rename(stage / ('previous-build-' + name))
            env = os.environ.copy()
            for key in ['CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET', 'TAURI_SIGNING_PRIVATE_KEY', 'APPLE_SIGNING_IDENTITY']:
                env.pop(key, None)
            configuration = json.loads((ROOT / 'src-tauri/tauri.conf.json').read_text())
            resources = configuration['bundle']['resources'].copy()
            resources['../artifacts/licenses/'] = 'licenses/dependencies/'
            override = {'bundle': {'resources': resources, 'macOS': {'signingIdentity': '-'}}}
            run('npm', 'run', 'tauri', '--', 'build', '--bundles', 'app,dmg', '--config', json.dumps(override), env=env)
            assert source_inventory() == sources, 'Source files changed during packaging; rebuild required'
            app = bundle / 'macos/GBF POWER REBORN.app'
            dmgs = list((bundle / 'dmg').glob('*.dmg'))
            assert len(dmgs) == 1, 'Expected exactly one fresh DMG'
            contents = verify_app(app)
            run('hdiutil', 'verify', str(dmgs[0]))
            mount = stage / 'mounted'
            mount.mkdir()
            attached = False
            try:
                run('hdiutil', 'attach', '-readonly', '-nobrowse', '-mountpoint', str(mount), str(dmgs[0]))
                attached = True
                assert inventory(mount / app.name) == contents, 'DMG content differs from the built app'
            finally:
                if attached:
                    run('hdiutil', 'detach', str(mount))
            shutil.copytree(app, stage / app.name, symlinks=True)
            dmg_name = 'GBF-POWER-REBORN-0.2.0-macos-arm64.dmg'
            shutil.copy2(dmgs[0], stage / dmg_name)
            manifest = {'baseCommit': run('git', 'rev-parse', 'HEAD', capture_output=True, text=True).stdout.strip(),
                        'version': '0.2.0', 'identifier': 'cc.lzsony.gbf-power-reborn',
                        'platform': platform.platform(), 'signature': 'ad-hoc', 'notarized': False,
                        'sourceFiles': sources, 'files': contents, 'dmgSha256': digest(stage / dmg_name)}
            (stage / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
            (stage / (dmg_name + '.sha256')).write_text(manifest['dmgSha256'] + '  ' + dmg_name + '\n')
            previous = OUTPUT / ('previous-' + time.strftime('%Y%m%d-%H%M%S'))
            for name in [app.name, dmg_name, dmg_name + '.sha256', 'manifest.json']:
                if (OUTPUT / name).exists():
                    previous.mkdir(exist_ok=True)
                    (OUTPUT / name).rename(previous / name)
                (stage / name).rename(OUTPUT / name)
            assert verify_app(OUTPUT / app.name) == contents
            print('PASS: verified App/DMG in ' + str(OUTPUT))
    finally:
        lock.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
