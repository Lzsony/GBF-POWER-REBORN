"""Reject mixed-profile and stale iOS native libraries before packaging."""
import importlib.util
import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
spec = importlib.util.spec_from_file_location("ios_build", ROOT / "scripts/build-ios.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class IOSCorePackagingTests(unittest.TestCase):
    def setUp(self):
        self.version = json.loads((ROOT / "package.json").read_text())["version"]
        self.profile = {"tag": "GBFP-PUBLIC-PROFILE-V1:" + "a" * 64}
        self.payload = self.profile["tag"].encode() + b"\0GBFP-IOS-CORE-V1:" + self.version.encode() + b"\0"

    def test_accepts_matching_core_and_service(self):
        build.verify_core(self.payload, self.profile)

    def test_rejects_stale_core_even_with_same_service(self):
        with self.assertRaisesRegex(ValueError, "Stale iOS core"):
            build.verify_core(self.payload.replace(self.version.encode(), b"0.0.0"), self.profile)

    def test_rejects_missing_or_mixed_service_profile(self):
        for data in [b"GBFP-IOS-CORE-V1:" + self.version.encode(), self.payload + b"GBFP-PUBLIC-PROFILE-V1:generic"]:
            with self.subTest(data=data), self.assertRaises(AssertionError):
                build.verify_core(data, self.profile)


if __name__ == "__main__":
    unittest.main()
