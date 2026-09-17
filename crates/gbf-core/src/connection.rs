use crate::config::{Mode, Settings};
use crate::error::ErrorCode;
use anyhow::{bail, Result};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Http,
    Socks5,
    Socks5h,
}
impl Protocol {
    fn scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Socks5 => "socks5",
            Self::Socks5h => "socks5h",
        }
    }
    pub fn mode(self) -> Mode {
        if self == Self::Http {
            Mode::Http
        } else {
            Mode::Socks5
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionMode {
    Direct,
    Proxy,
    Accelerate,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionInput {
    pub mode: ConnectionMode,
    #[serde(default)]
    pub selected_line_id: String,
    #[serde(default)]
    pub line_selection: crate::config::LineSelection,
    /// None preserves the existing endpoint AND its saved credential. Never send a display mask.
    pub proxy_url: Option<String>,
    pub listen_port: u16,
    pub https_cache: bool,
    pub cache_limit_gb: u32,
    pub autostart: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub mode: ConnectionMode,
    #[serde(default)]
    pub selected_line_id: String,
    #[serde(default)]
    pub line_selection: crate::config::LineSelection,
    pub proxy_url: String,
    pub has_authentication: bool,
    pub listen_port: u16,
    pub https_cache: bool,
    pub cache_limit_gb: u32,
    pub autostart: bool,
}

impl SettingsView {
    pub fn new(settings: &Settings) -> Self {
        Self {
            mode: match settings.mode {
                Mode::Direct => ConnectionMode::Direct,
                Mode::Accelerate => ConnectionMode::Accelerate,
                _ => ConnectionMode::Proxy,
            },
            selected_line_id: settings.selected_line_id.clone(),
            line_selection: settings.line_selection,
            proxy_url: display_url(settings, None),
            has_authentication: !settings.username.is_empty(),
            listen_port: settings.listen_port,
            https_cache: settings.https_cache,
            cache_limit_gb: settings.cache_limit_gb,
            autostart: settings.autostart,
        }
    }
}

pub fn display_url(settings: &Settings, password: Option<&str>) -> String {
    let host = settings.upstream_host.trim_matches(['[', ']']);
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.into()
    };
    let Ok(mut url) = url::Url::parse(&format!(
        "{}://{host}:{}",
        settings.proxy_protocol.scheme(),
        settings.upstream_port
    )) else {
        return String::new();
    };
    if !settings.username.is_empty() {
        let _ = url.set_username(
            &percent_encoding::utf8_percent_encode(
                &settings.username,
                percent_encoding::NON_ALPHANUMERIC,
            )
            .to_string(),
        );
        let _ = url.set_password(Some(
            &percent_encoding::utf8_percent_encode(
                password.unwrap_or("••••"),
                percent_encoding::NON_ALPHANUMERIC,
            )
            .to_string(),
        ));
    }
    let serialized = url.to_string();
    // Only the authority is stored/displayed; HTTP URL serialization supplies an optional slash.
    let authority = serialized.trim_end_matches('/');
    if password.is_none() {
        authority.replace("%E2%80%A2%E2%80%A2%E2%80%A2%E2%80%A2", "••••")
    } else {
        authority.into()
    }
}

pub fn apply_input(
    current: &Settings,
    input: &ConnectionInput,
) -> Result<(Settings, Option<String>)> {
    let mut next = current.clone();
    next.listen_port = input.listen_port;
    next.https_cache = input.https_cache;
    next.cache_limit_gb = input.cache_limit_gb;
    next.autostart = input.autostart;
    let password = if input.mode == ConnectionMode::Proxy {
        if let Some(raw) = &input.proxy_url {
            Some(parse_into(&mut next, raw)?)
        } else {
            None
        }
    } else {
        None
    };
    next.selected_line_id = input.selected_line_id.clone();
    next.line_selection = input.line_selection;
    next.mode = match input.mode {
        ConnectionMode::Direct => Mode::Direct,
        ConnectionMode::Accelerate => Mode::Accelerate,
        ConnectionMode::Proxy => next.proxy_protocol.mode(),
    };
    next.validate()?;
    Ok((next, password))
}

fn parse_into(settings: &mut Settings, raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(|c| c.is_control()) || raw.contains('\\') {
        bail!(ErrorCode::UrlInvalid);
    }
    let url = url::Url::parse(raw).map_err(|_| anyhow::anyhow!(ErrorCode::UrlInvalid))?;
    let protocol = match url.scheme() {
        "http" => Protocol::Http,
        "socks5" => Protocol::Socks5,
        "socks5h" => Protocol::Socks5h,
        _ => bail!(ErrorCode::ProtocolUnsupported),
    };
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        bail!(ErrorCode::UrlComponents);
    }
    // Validate percent escapes before decoding: malformed credentials must not silently change.
    let decode = |value: &str| -> Result<String> {
        let bytes = value.as_bytes();
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'%'
                && (i + 2 >= bytes.len()
                    || !bytes[i + 1].is_ascii_hexdigit()
                    || !bytes[i + 2].is_ascii_hexdigit())
            {
                bail!(ErrorCode::CredentialEncoding);
            }
        }
        let value = percent_decode_str(value)
            .decode_utf8()
            .map_err(|_| anyhow::anyhow!(ErrorCode::CredentialEncoding))?
            .into_owned();
        if value.contains("••••") || value.chars().any(char::is_control) {
            bail!(ErrorCode::CredentialMasked);
        }
        Ok(value)
    };
    let username = decode(url.username())?;
    let password = decode(url.password().unwrap_or(""))?;
    if username.is_empty() && !password.is_empty() {
        bail!(ErrorCode::UsernameRequired);
    }
    if username.contains(':') && protocol == Protocol::Http {
        bail!(ErrorCode::UsernameColon);
    }
    if protocol != Protocol::Http && (username.len() > 255 || password.len() > 255) {
        bail!(ErrorCode::CredentialLength);
    }
    settings.upstream_host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| anyhow::anyhow!(ErrorCode::UpstreamHostInvalid))?
        .trim_matches(['[', ']'])
        .into();
    settings.upstream_port =
        url.port()
            .unwrap_or(if protocol == Protocol::Http { 80 } else { 1080 });
    settings.username = username;
    settings.proxy_protocol = protocol;
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn draft(url: Option<&str>) -> ConnectionInput {
        ConnectionInput {
            mode: ConnectionMode::Proxy,
            selected_line_id: String::new(),
            line_selection: Default::default(),
            proxy_url: url.map(str::to_string),
            listen_port: 8123,
            https_cache: false,
            cache_limit_gb: 5,
            autostart: false,
        }
    }
    #[test]
    fn url_protocols_ipv6_and_encoding() {
        let (s, p) = apply_input(
            &Settings::default(),
            &draft(Some("socks5h://u%40x:p%3Ass%25@[::1]:1081")),
        )
        .unwrap();
        assert_eq!(s.username, "u@x");
        assert_eq!(s.upstream_host, "::1");
        assert_eq!(p.as_deref(), Some("p:ss%"));
        assert!(display_url(&s, None).contains("••••"));
        let full = display_url(&s, p.as_deref());
        let (round, p2) = apply_input(&s, &draft(Some(&full))).unwrap();
        assert_eq!(round.username, s.username);
        assert_eq!(p2, p);
        for (url, port) in [
            ("http://localhost", 80),
            ("http://localhost:80/", 80),
            ("socks5://localhost", 1080),
        ] {
            assert_eq!(
                apply_input(&s, &draft(Some(url))).unwrap().0.upstream_port,
                port
            );
        }
    }
    #[test]
    fn rejects_bad_urls_without_echoing_secrets() {
        for url in [
            "https://user:secret@host",
            "http://host/path",
            "http://host?q=secret",
            "http://host#secret",
            "http://host:0",
            "socks5://u:••••@host",
            "socks5://u:%FF@host",
            "socks5://u:%XX@host",
            "http://localhost:8123",
            "http://[::1]:8123",
            "localhost:7890",
        ] {
            let e = apply_input(&Settings::default(), &draft(Some(url))).expect_err(url);
            assert!(!e.to_string().contains("secret"));
        }
    }

    #[test]
    fn retaining_url_preserves_auth_and_direct_preserves_endpoint() {
        let (s, _) =
            apply_input(&Settings::default(), &draft(Some("socks5://u:p@host:1234"))).unwrap();
        let (next, pass) = apply_input(&s, &draft(None)).unwrap();
        assert_eq!(next.username, "u");
        assert!(pass.is_none());
        let mut direct = draft(None);
        direct.mode = ConnectionMode::Direct;
        let (next, _) = apply_input(&s, &direct).unwrap();
        assert_eq!(next.mode, Mode::Direct);
        assert_eq!(next.upstream_host, "host");
        assert_eq!(next.proxy_protocol, Protocol::Socks5);
        let (noauth, pass) = apply_input(&s, &draft(Some("http://host:1234"))).unwrap();
        assert!(noauth.username.is_empty());
        assert_eq!(pass, Some(String::new()));
    }
}
