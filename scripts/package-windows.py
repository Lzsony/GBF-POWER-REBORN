#!/usr/bin/env python3
"""Create and verify the Windows x64 CI artifact (requires system WebView2)."""
import hashlib
import ctypes
import json
import platform
import shutil
import struct
import subprocess
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / 'artifacts/windows'
MARKER = b'GBF-INTERNAL-TEST-BUILD-DO-NOT-DISTRIBUTE'


def verify_executable(payload):
    assert payload[:2] == b'MZ', 'Invalid DOS signature'
    pe = struct.unpack_from('<I', payload, 0x3c)[0]
    assert payload[pe:pe + 4] == b'PE\0\0', 'Invalid PE signature'
    assert struct.unpack_from('<H', payload, pe + 4)[0] == 0x8664, 'Expected x64'
    assert MARKER not in payload, 'Refusing internal-test binary'


def verify_version(path):
    version = ctypes.WinDLL('version', use_last_error=True)
    size_fn = version.GetFileVersionInfoSizeW
    size_fn.argtypes = [ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_ulong)]
    size_fn.restype = ctypes.c_ulong
    read_fn = version.GetFileVersionInfoW
    read_fn.argtypes = [ctypes.c_wchar_p, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_void_p]
    read_fn.restype = ctypes.c_int
    query_fn = version.VerQueryValueW
    query_fn.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_uint)]
    query_fn.restype = ctypes.c_int
    size = size_fn(str(path), None)
    assert size, 'Missing Windows version resource'
    data = ctypes.create_string_buffer(size)
    assert read_fn(str(path), 0, size, data), 'Cannot read Windows version resource'

    def query(key):
        value = ctypes.c_void_p()
        length = ctypes.c_uint()
        assert query_fn(data, key, ctypes.byref(value), ctypes.byref(length)), key
        return value, length.value

    value, length = query('\\')
    assert length >= 52
    fixed = struct.unpack('<13I', ctypes.string_at(value, 52))
    assert fixed[0] == 0xFEEF04BD, 'Invalid fixed version resource'
    for high, low in [(fixed[2], fixed[3]), (fixed[4], fixed[5])]:
        assert (high >> 16, high & 0xffff, low >> 16, low & 0xffff) == (0, 3, 0, 0), 'Unexpected executable version'
    value, length = query('\\VarFileInfo\\Translation')
    assert length >= 4
    language, codepage = struct.unpack('<HH', ctypes.string_at(value, 4))
    value, _ = query(f'\\StringFileInfo\\{language:04x}{codepage:04x}\\ProductName')
    assert ctypes.wstring_at(value) == 'GBF POWER REBORN', 'Unexpected product name'


def main():
    if platform.system() != 'Windows':
        raise SystemExit('Run after the Windows native build')
    binary = ROOT / 'target/release/gbf-power-reborn.exe'
    verify_executable(binary.read_bytes())
    verify_version(binary)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.package-', dir=OUTPUT) as temporary:
        stage = Path(temporary)
        shutil.copyfile(binary, stage / 'GBF POWER REBORN.exe')
        licenses = stage / 'licenses'
        licenses.mkdir()
        for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md']:
            shutil.copyfile(ROOT / name, licenses / name)
        shutil.copyfile(ROOT / 'docs/OFL-NotoSansTC.txt', licenses / 'OFL-NotoSansTC.txt')
        subprocess.run(['python', str(ROOT / 'scripts/collect-licenses.py'), '--output', str(licenses / 'dependencies')], cwd=ROOT, check=True)
        (stage / 'README.txt').write_text('GBF POWER REBORN 0.3.0\nWindows x64 CI artifact. Requires installed Microsoft Edge WebView2 Runtime.\nNot Authenticode signed. Native UI, certificate trust and real gameplay require device acceptance.\n', encoding='utf-8')
        files = {str(p.relative_to(stage)).replace('\\', '/'): hashlib.sha256(p.read_bytes()).hexdigest()
                 for p in sorted(stage.rglob('*')) if p.is_file()}
        manifest = {'version': '0.3.0', 'architecture': 'x64', 'signed': False, 'files': files}
        (stage / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        pending = stage / 'delivery.zip'
        with zipfile.ZipFile(pending, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
            for name in [*files, 'manifest.json']:
                archive.write(stage / name, name)
        with zipfile.ZipFile(pending) as archive:
            assert set(archive.namelist()) == set(files) | {'manifest.json'}
            assert archive.testzip() is None
            for name, expected in files.items():
                assert hashlib.sha256(archive.read(name)).hexdigest() == expected, name
            verify_executable(archive.read('GBF POWER REBORN.exe'))
        output = OUTPUT / 'GBF-POWER-REBORN-0.3.0-windows-x64.zip'
        pending.replace(output)
        (OUTPUT / (output.name + '.sha256')).write_text(hashlib.sha256(output.read_bytes()).hexdigest() + '  ' + output.name + '\n')
        print('PASS: Windows x64 artifact and manifest verified')


if __name__ == '__main__':
    main()
