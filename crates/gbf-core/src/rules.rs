pub const API_HOSTS: &[&str] = &[
    "game.granbluefantasy.jp",
    "steam.granbluefantasy.com",
    "granbluefantasy.jp",
    "granbluefantasy.com",
];
pub const ASSET_HOSTS: &[&str] = &[
    "prd-game-a-granbluefantasy.akamaized.net",
    "prd-game-a-granbluefantasy-steam.akamaized.net",
];
pub const PROXY_DOMAINS: &[&str] = &[
    "gamewith.jp",
    "mbga.jp",
    "mobage.jp",
    "dmm.com",
    "dmm.co.jp",
    "dmmgames.com",
];
pub const TUNNEL_HOSTS: &[&str] = &[
    "prd-game-a-gbf.akamaized.net",
    "code.createjs.com",
    "code.jquery.com",
    "cdnjs.cloudflare.com",
    "cdn.jsdelivr.net",
    "fonts.fontplus.dev",
    "www.datadoghq-browser-agent.com",
];
pub fn normalize(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}
pub fn is_asset(host: &str) -> bool {
    ASSET_HOSTS.contains(&normalize(host).as_str())
}
pub fn is_target(host: &str) -> bool {
    let host = normalize(host);
    is_asset(&host)
        || API_HOSTS.contains(&host.as_str())
        || TUNNEL_HOSTS.contains(&host.as_str())
        || PROXY_DOMAINS
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GameLanguage {
    Japanese,
    English,
}
impl GameLanguage {
    pub fn from_path(path: &str) -> Option<Self> {
        if path.starts_with("/assets/") {
            Some(Self::Japanese)
        } else if path.starts_with("/assets_en/") {
            Some(Self::English)
        } else {
            None
        }
    }
    pub fn directory(self) -> &'static str {
        match self {
            Self::Japanese => "ja",
            Self::English => "en",
        }
    }
}
pub fn cache_path(path: &str) -> bool {
    GameLanguage::from_path(path).is_some()
        && !path.split('/').any(|part| part == "..")
        && [
            ".png", ".jpg", ".jpeg", ".webp", ".gif", ".js", ".css", ".mp3", ".ogg", ".m4a",
            ".woff", ".woff2", ".ttf", ".json",
        ]
        .iter()
        .any(|ext| path.to_ascii_lowercase().ends_with(ext))
}
pub fn pac(port: u16) -> String {
    let hosts: Vec<_> = API_HOSTS
        .iter()
        .chain(ASSET_HOSTS.iter())
        .chain(TUNNEL_HOSTS.iter())
        .copied()
        .collect();
    format!("// GBF POWER REBORN — SwitchyOmega / ZeroOmega PAC\nfunction FindProxyForURL(url, host) {{\n  host = host.toLowerCase().replace(/\\.+$/, '');\n  var hosts = {};\n  var domains = {};\n  for (var i = 0; i < hosts.length; i++) {{\n    if (host === hosts[i]) return 'PROXY 127.0.0.1:{}';\n  }}\n  for (var j = 0; j < domains.length; j++) {{\n    var suffix = '.' + domains[j];\n    if (host === domains[j] || host.slice(-suffix.length) === suffix) return 'PROXY 127.0.0.1:{}';\n  }}\n  return 'DIRECT';\n}}\n", serde_json::to_string(&hosts).unwrap(), serde_json::to_string(PROXY_DOMAINS).unwrap(), port, port)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_target_fixture_keeps_new_routes_out_of_asset_cache() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/target-rules.json"))
                .unwrap();
        for case in cases.as_array().unwrap() {
            let host = case["host"].as_str().unwrap();
            assert_eq!(is_target(host), case["target"].as_bool().unwrap(), "{host}");
            assert_eq!(is_asset(host), case["asset"].as_bool().unwrap(), "{host}");
        }
    }
    #[test]
    fn exact_hosts_only() {
        assert!(is_asset("PRD-GAME-A-GRANBLUEFANTASY.AKAMAIZED.NET."));
        assert!(!is_asset("game.granbluefantasy.jp"));
        assert!(!is_target("game.granbluefantasy.jp.evil.com"));
        assert!(is_target("login.mbga.jp"));
        assert!(!is_asset("login.mbga.jp"));
        assert!(!is_target("localhost"));
    }
    #[test]
    fn pac_has_no_fallback() {
        let p = pac(8123);
        assert!(p.contains("\"steam.granbluefantasy.com\""));
        assert!(!p.contains("\"game.granbluefantasy.com\""));
        assert!(is_target("steam.granbluefantasy.com"));
        assert!(!is_asset("steam.granbluefantasy.com"));
        assert!(!is_target("game.granbluefantasy.com"));
        assert!(p.contains("PROXY 127.0.0.1:8123'"));
        assert!(!p.contains("; DIRECT"));
    }
    #[test]
    fn paths_are_conservative() {
        assert!(cache_path("/assets/img/a.png"));
        assert!(!cache_path("/rest/raid/start.json"));
        assert!(!cache_path("/assets/../secret.json"));
    }
}
