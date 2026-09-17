"""Validate public client build inputs and identify the embedded deployment."""
from __future__ import annotations

import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import ssl
from urllib.parse import urlsplit

TAG_PREFIX = 'GBFP-PUBLIC-PROFILE-V1:'
TAG_PATTERN = re.compile(rb'GBFP-PUBLIC-PROFILE-V1:(?:generic|[0-9a-f]{64})')


def profile_input():
    value = os.environ.get('GPR_CLIENT_CONFIG')
    if value is None:
        return {'tag': TAG_PREFIX + 'generic', 'files': {}}
    if not value:
        raise ValueError('GPR_CLIENT_CONFIG must name a JSON file')
    config = Path(value).resolve(strict=True)
    fields = json.loads(config.read_text(encoding='utf-8'))
    if not isinstance(fields, dict) or set(fields) != {'deploymentId', 'url', 'caCertificateFile'}:
        raise ValueError('Client config must contain only deploymentId, url, and caCertificateFile')
    if not all(isinstance(value, str) for value in fields.values()):
        raise ValueError('Client config values must be strings')
    deployment_id = fields['deploymentId']
    if not re.fullmatch(r'[a-z0-9][a-z0-9_-]{0,63}', deployment_id) or deployment_id in {'con', 'prn', 'aux', 'nul', *(f'com{i}' for i in range(1, 10)), *(f'lpt{i}' for i in range(1, 10))}:
        raise ValueError('Invalid deploymentId')
    address = urlsplit(fields['url'])
    if address.scheme != 'https' or not address.hostname or not address.hostname.isascii() or address.username is not None or address.password is not None or address.path not in ('', '/') or address.query or address.fragment:
        raise ValueError('Control URL must be an HTTPS origin')
    try:
        port = address.port
    except ValueError as error:
        raise ValueError('Invalid Control URL port') from error
    if port == 0:
        raise ValueError('Invalid Control URL port')
    hostname = address.hostname.lower()
    if ':' in hostname:
        hostname = f'[{ipaddress.IPv6Address(hostname).compressed}]'
    origin = 'https://' + hostname + (f':{port}' if port and port != 443 else '')
    cert_name = Path(fields['caCertificateFile'])
    if cert_name.is_absolute() or not cert_name.parts or any(part in ('.', '..') for part in cert_name.parts):
        raise ValueError('caCertificateFile must be inside the config directory')
    cert = (config.parent / cert_name).resolve(strict=True)
    if not cert.is_relative_to(config.parent):
        raise ValueError('caCertificateFile leaves the config directory')
    pem = cert.read_text(encoding='utf-8').strip() + '\n'
    if len(pem) > 32768 or not pem.startswith('-----BEGIN CERTIFICATE-----\n') or not pem.endswith('-----END CERTIFICATE-----\n') or pem.count('-----BEGIN ') != 1:
        raise ValueError('Expected one public PEM certificate')
    der = ssl.PEM_cert_to_DER_cert(pem)
    if not der:
        raise ValueError('Invalid public PEM certificate')
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    try:
        context.load_verify_locations(cadata=pem)
    except ssl.SSLError as error:
        raise ValueError('Invalid X.509 public certificate') from error
    digest = hashlib.sha256(deployment_id.encode() + b'\0' + origin.encode() + b'\0' + der).hexdigest()
    files = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in (config, cert)}
    return {'tag': TAG_PREFIX + digest, 'files': files, 'deploymentId': deployment_id, 'url': origin}


def verify_binary_profile(payload: bytes, expected: str):
    tags = {match.decode('ascii') for match in TAG_PATTERN.findall(payload)}
    if tags != {expected}:
        raise AssertionError('Embedded client profile does not match the packaging input')
