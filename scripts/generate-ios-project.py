#!/usr/bin/env python3
"""Generate the checked-in Xcode project without a third-party project generator."""
from pathlib import Path
import hashlib
import json
import plistlib

ROOT = Path(__file__).resolve().parents[1]
IOS = ROOT / "ios"
objects = {}


def add(identity, isa, **fields):
    key = hashlib.sha256(identity.encode()).hexdigest()[:24].upper()
    objects[key] = {"isa": isa, **fields}
    return key


def encode(value, depth=0):
    if isinstance(value, dict):
        return "{\n" + "".join("\t" * (depth + 1) + json.dumps(k) + " = " + encode(v, depth + 1) + ";\n" for k, v in value.items()) + "\t" * depth + "}"
    if isinstance(value, list):
        return "(" + ", ".join(encode(v, depth) for v in value) + ")"
    return json.dumps(str(value))


def plist(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(plistlib.dumps(value, sort_keys=False))


def main():
    config = add("config", "PBXFileReference", lastKnownFileType="text.xcconfig", path="Config/Base.xcconfig", sourceTree="<group>")
    library = add("core-library", "PBXFileReference", lastKnownFileType="wrapper.xcframework", path="../artifacts/ios/GPRCore.xcframework", sourceTree="<group>")
    licenses = add("licenses", "PBXFileReference", lastKnownFileType="folder", path="../artifacts/ios/Licenses", sourceTree="<group>")
    sources = {}
    for path in sorted(IOS.glob("**/*.swift")):
        relative = path.relative_to(IOS).as_posix()
        if relative.startswith(("App/", "Shared/", "Tunnel/")):
            sources[relative] = add(relative, "PBXFileReference", lastKnownFileType="sourcecode.swift", path=relative, sourceTree="<group>")
    products = {}
    target_ids = {}
    for name, folder, extension in [("GPR", "App", False), ("GPRTunnel", "Tunnel", True)]:
        product = add(name + "-product", "PBXFileReference", explicitFileType="wrapper.app-extension" if extension else "wrapper.application", path=name + (".appex" if extension else ".app"), sourceTree="BUILT_PRODUCTS_DIR", includeInIndex=0)
        products[name] = product
        entries = [add(name + "-" + path, "PBXBuildFile", fileRef=reference) for path, reference in sources.items() if path.startswith((folder + "/", "Shared/"))]
        source_phase = add(name + "-sources", "PBXSourcesBuildPhase", buildActionMask=2147483647, files=entries, runOnlyForDeploymentPostprocessing=0)
        link = add(name + "-core", "PBXBuildFile", fileRef=library)
        framework_phase = add(name + "-frameworks", "PBXFrameworksBuildPhase", buildActionMask=2147483647, files=[link], runOnlyForDeploymentPostprocessing=0)
        configurations = []
        for mode in ["Debug", "Release"]:
            settings = {"PRODUCT_BUNDLE_IDENTIFIER": "$(GPR_BUNDLE_ID)" + (".tunnel" if extension else ""), "PRODUCT_NAME": "$(TARGET_NAME)", "INFOPLIST_FILE": f"{folder}/Info.plist", "CODE_SIGN_ENTITLEMENTS": f"{folder}/{name}.entitlements", "SWIFT_OPTIMIZATION_LEVEL": "-Onone" if mode == "Debug" else "-O", "APPLICATION_EXTENSION_API_ONLY": "YES" if extension else "NO", "SKIP_INSTALL": "YES" if extension else "NO"}
            configurations.append(add(name + mode, "XCBuildConfiguration", baseConfigurationReference=config, buildSettings=settings, name=mode))
        listing = add(name + "-configurations", "XCConfigurationList", buildConfigurations=configurations, defaultConfigurationIsVisible=0, defaultConfigurationName="Release")
        target_ids[name] = add(name + "-target", "PBXNativeTarget", buildConfigurationList=listing, buildPhases=[source_phase, framework_phase], buildRules=[], dependencies=[], name=name, productName=name, productReference=product, productType="com.apple.product-type.app-extension" if extension else "com.apple.product-type.application")
        info = {"CFBundleDisplayName": "GBF POWER REBORN" if not extension else "GPR Tunnel", "CFBundleExecutable": "$(EXECUTABLE_NAME)", "CFBundleIdentifier": "$(PRODUCT_BUNDLE_IDENTIFIER)", "CFBundleName": "$(PRODUCT_NAME)", "CFBundlePackageType": "XPC!" if extension else "APPL", "CFBundleShortVersionString": "$(MARKETING_VERSION)", "CFBundleVersion": "$(CURRENT_PROJECT_VERSION)", "GPRAppGroup": "$(GPR_APP_GROUP)", "GPRKeychainGroup": "$(AppIdentifierPrefix)$(GPR_BUNDLE_ID)", "GPRTunnelIdentifier": "$(GPR_BUNDLE_ID).tunnel"}
        if extension:
            info["NSExtension"] = {"NSExtensionPointIdentifier": "com.apple.networkextension.packet-tunnel", "NSExtensionPrincipalClass": "$(PRODUCT_MODULE_NAME).PacketTunnelProvider"}
        else:
            info.update({"LSRequiresIPhoneOS": True, "UILaunchScreen": {}, "UISupportedInterfaceOrientations": ["UIInterfaceOrientationPortrait"], "CFBundleLocalizations": ["zh-Hant", "zh-Hans", "ja", "en"], "ITSAppUsesNonExemptEncryption": True})
        plist(IOS / folder / "Info.plist", info)
        plist(IOS / folder / f"{name}.entitlements", {"com.apple.developer.networking.networkextension": ["packet-tunnel-provider"], "com.apple.security.application-groups": ["$(GPR_APP_GROUP)"], "keychain-access-groups": ["$(AppIdentifierPrefix)$(GPR_BUNDLE_ID)"]})
    embed = add("embed-tunnel", "PBXBuildFile", fileRef=products["GPRTunnel"], settings={"ATTRIBUTES": ["RemoveHeadersOnCopy"]})
    copy = add("extensions", "PBXCopyFilesBuildPhase", buildActionMask=2147483647, dstPath="", dstSubfolderSpec=13, files=[embed], name="Embed App Extensions", runOnlyForDeploymentPostprocessing=0)
    dependency = add("tunnel-dependency", "PBXTargetDependency", target=target_ids["GPRTunnel"])
    objects[target_ids["GPR"]]["buildPhases"].append(copy)
    license_file = add("license-resource", "PBXBuildFile", fileRef=licenses)
    resource_phase = add("resources", "PBXResourcesBuildPhase", buildActionMask=2147483647, files=[license_file], runOnlyForDeploymentPostprocessing=0)
    objects[target_ids["GPR"]]["buildPhases"].append(resource_phase)
    objects[target_ids["GPR"]]["dependencies"].append(dependency)
    product_group = add("products", "PBXGroup", children=list(products.values()), name="Products", sourceTree="<group>")
    group = add("main-group", "PBXGroup", children=[config, library, licenses, *sources.values(), product_group], sourceTree="<group>")
    configs = [add("project-" + mode, "XCBuildConfiguration", buildSettings={}, name=mode) for mode in ["Debug", "Release"]]
    listing = add("project-configs", "XCConfigurationList", buildConfigurations=configs, defaultConfigurationIsVisible=0, defaultConfigurationName="Release")
    project = add("project", "PBXProject", attributes={"LastUpgradeCheck": "1600", "BuildIndependentTargetsInParallel": "YES"}, buildConfigurationList=listing, compatibilityVersion="Xcode 14.0", developmentRegion="en", hasScannedForEncodings=0, knownRegions=["en", "Base", "zh-Hant", "zh-Hans", "ja"], mainGroup=group, productRefGroup=product_group, projectDirPath="", projectRoot="", targets=list(target_ids.values()))
    destination = IOS / "GPR.xcodeproj"
    destination.mkdir(exist_ok=True)
    (destination / "project.pbxproj").write_text("// !$*UTF8*$!\n" + encode({"archiveVersion": 1, "classes": {}, "objectVersion": 56, "objects": objects, "rootObject": project}) + "\n")
    scheme = destination / "xcshareddata/xcschemes/GPR.xcscheme"
    scheme.parent.mkdir(parents=True, exist_ok=True)
    ref = f'<BuildableReference BuildableIdentifier="primary" BlueprintIdentifier="{target_ids["GPR"]}" BuildableName="GPR.app" BlueprintName="GPR" ReferencedContainer="container:GPR.xcodeproj"/>'
    scheme.write_text(f'''<?xml version="1.0" encoding="UTF-8"?>
<Scheme LastUpgradeVersion="1600" version="1.3">
 <BuildAction parallelizeBuildables="YES" buildImplicitDependencies="YES"><BuildActionEntries><BuildActionEntry buildForTesting="YES" buildForRunning="YES" buildForProfiling="YES" buildForArchiving="YES" buildForAnalyzing="YES">{ref}</BuildActionEntry></BuildActionEntries></BuildAction>
 <LaunchAction buildConfiguration="Debug" selectedDebuggerIdentifier="Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier="Xcode.IDEFoundation.Launcher.LLDB" launchStyle="0" useCustomWorkingDirectory="NO" ignoresPersistentStateOnLaunch="NO" debugDocumentVersioning="YES" allowLocationSimulation="YES"><BuildableProductRunnable runnableDebuggingMode="0">{ref}</BuildableProductRunnable></LaunchAction>
 <ProfileAction buildConfiguration="Release" shouldUseLaunchSchemeArgsEnv="YES" savedToolIdentifier="" useCustomWorkingDirectory="NO" debugDocumentVersioning="YES"><BuildableProductRunnable runnableDebuggingMode="0">{ref}</BuildableProductRunnable></ProfileAction>
 <AnalyzeAction buildConfiguration="Debug"/>
 <ArchiveAction buildConfiguration="Release" revealArchiveInOrganizer="YES"/>
</Scheme>
''')


if __name__ == "__main__":
    main()
