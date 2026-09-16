"""Extract explicitly named Rust rule arrays; keep tunnel domains distinct."""
import re


def read_rules(path):
    source = path.read_text()
    def array(name):
        match = re.search(r'pub const ' + name + r':\s*&\[&str\]\s*=\s*&\[(.*?)\];', source, re.S)
        if not match:
            raise ValueError(f'Missing rule array: {name}')
        return re.findall(r'"([a-z0-9.-]+)"', match[1])
    hosts = array('API_HOSTS') + array('ASSET_HOSTS') + array('TUNNEL_HOSTS')
    domains = array('PROXY_DOMAINS')
    if not hosts or len(set(hosts)) != len(hosts) or len(set(domains)) != len(domains):
        raise ValueError('Empty or duplicate target rules')
    return {'hosts': hosts, 'domains': domains, 'ports': [80, 443]}
