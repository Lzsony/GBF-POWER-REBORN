"""Public build-profile checks: generic builds, opt-in identity, and input rejection."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts.client_profile import profile_input, verify_binary_profile


ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / 'tests/fixtures/client-profile.example.json'
CERT = ROOT / 'tests/fixtures/control-ca-public.example.txt'


class PublicClientProfileTests(unittest.TestCase):
    def test_generic_build_has_no_deployment(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(profile_input(), {'tag': 'GBFP-PUBLIC-PROFILE-V1:generic', 'files': {}})

    def test_public_fixture_identifies_one_deployment(self):
        with patch.dict(os.environ, {'GPR_CLIENT_CONFIG': str(FIXTURE)}):
            profile = profile_input()
        self.assertEqual(profile['deploymentId'], 'test-deployment')
        self.assertEqual(profile['url'], 'https://example.invalid')
        self.assertEqual(len(profile['tag']), len('GBFP-PUBLIC-PROFILE-V1:') + 64)
        verify_binary_profile(profile['tag'].encode(), profile['tag'])
        with self.assertRaises(AssertionError):
            verify_binary_profile(b'GBFP-PUBLIC-PROFILE-V1:generic', profile['tag'])

    def test_ipv6_origin_is_canonical_and_malformed_der_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            certificate = root / 'public.txt'
            certificate.write_bytes(CERT.read_bytes())
            config = root / 'client.local.json'
            config.write_text(json.dumps({
                'deploymentId': 'ipv6-deployment',
                'url': 'https://[2001:0db8:0000:0000:0000:0000:0000:0001]:8443/',
                'caCertificateFile': 'public.txt',
            }))
            with patch.dict(os.environ, {'GPR_CLIENT_CONFIG': str(config)}):
                self.assertEqual(profile_input()['url'], 'https://[2001:db8::1]:8443')
                certificate.write_text('-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n')
                with self.assertRaises(ValueError):
                    profile_input()

    def test_private_or_unexpected_data_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'control-ca-public.example.txt').write_bytes(CERT.read_bytes())
            data = json.loads(FIXTURE.read_text())
            config = root / 'client.local.json'
            for patch_fields in (
                {'accessCode': 'should-not-be-embedded'},
                {'deploymentId': 'CON'},
                {'url': 'http://example.invalid/'},
                {'caCertificateFile': '../control-ca-public.example.txt'},
            ):
                config.write_text(json.dumps(data | patch_fields))
                with patch.dict(os.environ, {'GPR_CLIENT_CONFIG': str(config)}):
                    with self.assertRaises((ValueError, FileNotFoundError)):
                        profile_input()
            (root / 'private-key.txt').write_text('-----BEGIN PRIVATE KEY-----\nAA==\n-----END PRIVATE KEY-----\n')
            config.write_text(json.dumps(data | {'caCertificateFile': 'private-key.txt'}))
            with patch.dict(os.environ, {'GPR_CLIENT_CONFIG': str(config)}):
                with self.assertRaises(ValueError):
                    profile_input()


if __name__ == '__main__':
    unittest.main()
