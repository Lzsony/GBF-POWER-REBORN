#!/usr/bin/env python3
"""Build the Rust XCFramework and iPhone app; unsigned output is not installable."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile

from client_profile import profile_input, verify_binary_profile

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "artifacts/ios"


def run(args, **kwargs):
    subprocess.run([str(x) for x in args], cwd=ROOT, check=True, **kwargs)


def verify_core(payload, profile):
    verify_binary_profile(payload, profile["tag"])
    version = json.loads((ROOT / "package.json").read_text())["version"]
    tags = set(re.findall(rb"GBFP-IOS-CORE-V1:[0-9]+\.[0-9]+\.[0-9]+", payload))
    if tags != {f"GBFP-IOS-CORE-V1:{version}".encode()}:
        raise ValueError("Stale iOS core version; rebuild the XCFramework")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=["all", "device", "simulator"], default="all")
    parser.add_argument("--configuration", choices=["Debug", "Release"], default="Release")
    parser.add_argument("--unsigned", action="store_true")
    parser.add_argument("--core-only", action="store_true")
    parser.add_argument("--skip-core", action="store_true")
    parser.add_argument("--team")
    args = parser.parse_args()
    profile = profile_input()
    if not args.unsigned and profile["tag"].endswith(":generic"):
        parser.error("Signed builds require GPR_CLIENT_CONFIG with a public service profile")
    OUT.mkdir(parents=True, exist_ok=True)
    platforms = [("device", "aarch64-apple-ios", "iphoneos"), ("simulator", "aarch64-apple-ios-sim", "iphonesimulator")]
    platforms = [row for row in platforms if args.platform in ("all", row[0])]
    framework = OUT / "GPRCore.xcframework"
    if not args.skip_core:
        sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
        inputs = []
        headers = OUT / "include"
        headers.mkdir(exist_ok=True)
        shutil.copyfile(ROOT / "ios/Shared/GPRCore.h", headers / "GPRCore.h")
        for _, target, _ in platforms:
            env = os.environ.copy()
            env["IPHONEOS_DEPLOYMENT_TARGET"] = "17.0"
            command = ["cargo", "build", "--locked", "--release", "-p", "gbf-ios", "--target", target]
            if not (sysroot / "lib/rustlib" / target / "lib").is_dir():
                if not (sysroot / "lib/rustlib/src/rust/library").is_dir():
                    parser.error(f"Install the {target} Rust target or rust-src")
                env["RUSTC_BOOTSTRAP"] = "1"
                command += ["-Z", "build-std=std,panic_abort"]
            run(command, env=env)
            library = ROOT / "target" / target / "release/libgbf_ios.a"
            verify_core(library.read_bytes(), profile)
            inputs += ["-library", library, "-headers", headers]
        with tempfile.TemporaryDirectory(prefix="xcframework-", dir=OUT) as temporary:
            built = Path(temporary) / "GPRCore.xcframework"
            run(["xcodebuild", "-create-xcframework", *inputs, "-output", built])
            if framework.exists():
                shutil.rmtree(framework)
            shutil.move(built, framework)
    for library in framework.glob("**/*.a"):
        verify_core(library.read_bytes(), profile)
    if not framework.is_dir():
        parser.error("Missing GPRCore.xcframework; build the core first")
    if args.core_only:
        return
    run(["python3", "scripts/collect-ios-licenses.py"])
    manifest = {"profileTag": profile["tag"], "configuration": args.configuration, "signed": not args.unsigned, "files": {}}
    for kind, _, sdk in platforms:
        derived = OUT / f"derived-{kind}"
        command = ["xcodebuild", "-project", "ios/GPR.xcodeproj", "-scheme", "GPR", "-configuration", args.configuration, "-sdk", sdk, "-destination", f"generic/platform={'iOS' if kind == 'device' else 'iOS Simulator'}", "-derivedDataPath", derived]
        # Provisioning updates can leave an old extension seal around a new profile.
        # Signed delivery must not reuse that unsigned / incremental product tree.
        command += ["build"] if args.unsigned else ["clean", "build"]
        if args.unsigned:
            command += ["CODE_SIGNING_ALLOWED=NO"]
        elif args.team:
            command += [f"DEVELOPMENT_TEAM={args.team}"]
        run(command)
        app = derived / f"Build/Products/{args.configuration}-{sdk}/GPR.app"
        for binary in [app / "GPR", app / "PlugIns/GPRTunnel.appex/GPRTunnel"]:
            # Xcode Debug builds may put the application code in a debug dylib;
            # the executable is then only the debugger's launch stub.
            debug_library = binary.with_name(binary.name + ".debug.dylib")
            linked = debug_library if args.configuration == "Debug" and debug_library.exists() else binary
            verify_core(linked.read_bytes(), profile)
            manifest["files"][str(binary.relative_to(OUT))] = hashlib.sha256(binary.read_bytes()).hexdigest()
            if linked != binary:
                manifest["files"][str(linked.relative_to(OUT))] = hashlib.sha256(linked.read_bytes()).hexdigest()
        if not args.unsigned:
            run(["codesign", "--verify", "--deep", "--strict", app])
            run(["codesign", "--verify", "--strict", app / "PlugIns/GPRTunnel.appex"])
    manifest_name = "manifest.json" if args.configuration == "Release" else "manifest-debug.json"
    (OUT / manifest_name).write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")


if __name__ == "__main__":
    main()
