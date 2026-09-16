#!/usr/bin/env python3
"""Render the original SVG icons using the installed Tauri CLI."""
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / 'node_modules/@tauri-apps/cli/tauri.js'
ICONS = ROOT / 'src-tauri/icons'
with tempfile.TemporaryDirectory(prefix='gpr-icons-') as temporary:
    output = Path(temporary)
    subprocess.run(['node', str(CLI), 'icon', str(ICONS / 'icon.svg'), '-o', str(output / 'app')], cwd=ROOT, check=True)
    subprocess.run(['node', str(CLI), 'icon', str(ICONS / 'tray.svg'), '-o', str(output / 'tray'), '--png', '64'], cwd=ROOT, check=True)
    for name in ['icon.png', 'icon.ico', 'icon.icns']:
        shutil.copyfile(output / 'app' / name, ICONS / name)
    shutil.copyfile(output / 'tray/64x64.png', ICONS / 'tray.png')
