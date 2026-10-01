#!/usr/bin/env python3
"""Collect the resolved iOS Rust dependency notices into the application bundle."""
import json
import hashlib
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/ios/Licenses"


def main():
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", "aarch64-apple-ios"], cwd=ROOT))
    packages = {item["id"]: item for item in metadata["packages"]}
    edges = {item["id"]: item["dependencies"] for item in metadata["resolve"]["nodes"]}
    pending = [key for key, item in packages.items() if item["name"] == "gbf-ios"]
    selected = set()
    while pending:
        key = pending.pop()
        if key not in selected:
            selected.add(key)
            pending.extend(edges.get(key, []))
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    supplemental = ROOT / "docs/dependency-licenses"
    fallback = {(item["name"], item["version"]): item for item in json.loads((supplemental / "manifest.json").read_text())["packages"] if item["ecosystem"] == "rust"}
    for item in fallback.values():
        for source in item["sources"]:
            path = (supplemental / source["file"]).resolve()
            if not path.is_relative_to(supplemental.resolve()) or hashlib.sha256(path.read_bytes()).hexdigest() != source["sha256"]:
                raise RuntimeError("Supplemental license source failed integrity check")
    shutil.copytree(supplemental / "standard", OUT / "standard")
    for name in ["LICENSE", "THIRD_PARTY_NOTICES.md"]:
        shutil.copyfile(ROOT / name, OUT / name)
    inventory = []
    for key in sorted(selected):
        package = packages[key]
        if package["source"] is None:
            continue
        source = Path(package["manifest_path"]).parent
        destination = OUT / (package["name"] + "-" + package["version"])
        notices = []
        for path in source.iterdir():
            if path.is_file() and re.match(r"^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)([._-]|$)", path.name, re.I):
                destination.mkdir(exist_ok=True)
                shutil.copyfile(path, destination / path.name)
                notices.append(path.name)
            elif path.is_dir() and path.name.lower() == "licenses":
                shutil.copytree(path, destination / path.name)
                notices.extend(str(p.relative_to(source)) for p in path.rglob("*") if p.is_file())
        if not notices and (package["name"], package["version"]) in fallback:
            destination.mkdir(exist_ok=True)
            for name in fallback[(package["name"], package["version"])]["files"]:
                path = (supplemental / name).resolve()
                if not path.is_relative_to(supplemental.resolve()):
                    raise RuntimeError("Invalid supplemental notice path")
                shutil.copyfile(path, destination / path.name)
                notices.append(path.name)
        if not notices:
            raise RuntimeError(f"Missing iOS dependency license: {package['name']} {package['version']}")
        inventory.append({"name": package["name"], "version": package["version"], "license": package.get("license"), "files": sorted(notices)})
    (OUT / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n")
    print(f"Collected {len(inventory)} iOS dependency notices")


if __name__ == "__main__":
    main()
