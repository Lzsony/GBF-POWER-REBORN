use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    env, fs,
    net::{Ipv4Addr, Ipv6Addr},
    path::{Component, Path, PathBuf},
};

fn public_profile() -> Result<Option<(String, String, String, String)>, String> {
    let Some(config_path) = env::var_os("GPR_CLIENT_CONFIG") else {
        return Ok(None);
    };
    let config_path = PathBuf::from(config_path);
    if config_path.as_os_str().is_empty() {
        return Err("GPR_CLIENT_CONFIG must name a JSON file".into());
    }
    let config_path = if config_path.is_absolute() {
        config_path
    } else {
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join(config_path)
    };
    println!("cargo:rerun-if-changed={}", config_path.display());
    let raw = fs::read_to_string(&config_path)
        .map_err(|_| "Cannot read GPR_CLIENT_CONFIG".to_string())?;
    let object = serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|_| "GPR_CLIENT_CONFIG is not valid JSON".to_string())?;
    let object = object
        .as_object()
        .ok_or("Client config must be a JSON object")?;
    let expected = ["deploymentId", "url", "caCertificateFile"];
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != expected.into_iter().collect::<BTreeSet<_>>()
    {
        return Err(
            "Client config must contain only deploymentId, url, and caCertificateFile".into(),
        );
    }
    let field = |name| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("Client config field {name} must be a string"))
    };
    let deployment_id = field("deploymentId")?.to_string();
    let reserved = matches!(deployment_id.as_str(), "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| {
            deployment_id.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
            })
        });
    if deployment_id.is_empty()
        || deployment_id.len() > 64
        || !deployment_id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
        || !deployment_id.as_bytes()[0].is_ascii_lowercase()
            && !deployment_id.as_bytes()[0].is_ascii_digit()
        || reserved
    {
        return Err("deploymentId must be 1-64 lowercase ASCII letters, digits, underscores, or hyphens (not a reserved device name)".into());
    }
    let raw_url = field("url")?;
    let url = url::Url::parse(raw_url).map_err(|_| "Client Control URL is invalid".to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.port() == Some(0)
        || raw_url.contains('@')
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("Client Control URL must be an HTTPS origin without credentials, path, query, or fragment".into());
    }
    let authority = raw_url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .ok_or("Client Control URL must start with https://")?;
    let raw_host = if authority.starts_with('[') {
        authority
            .find(']')
            .map(|end| &authority[..=end])
            .ok_or("Invalid bracketed Control host")?
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    let canonical_host = match url.host().ok_or("Client Control URL has no host")? {
        url::Host::Domain(domain) => raw_host.eq_ignore_ascii_case(domain),
        url::Host::Ipv4(address) => raw_host.parse::<Ipv4Addr>().is_ok_and(|v| v == address),
        url::Host::Ipv6(address) => raw_host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .and_then(|host| host.parse::<Ipv6Addr>().ok())
            .is_some_and(|v| v == address),
    };
    if !raw_url.is_ascii() || !canonical_host {
        return Err("Client Control host must use a canonical ASCII DNS name or IP address".into());
    }
    let origin = url.origin().ascii_serialization();
    let cert_file = Path::new(field("caCertificateFile")?);
    if cert_file.as_os_str().is_empty()
        || cert_file.is_absolute()
        || cert_file
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(
            "caCertificateFile must be a relative file name inside the config directory".into(),
        );
    }
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    let base = parent
        .canonicalize()
        .map_err(|_| "Client config directory is unavailable")?;
    let cert_path = parent
        .join(cert_file)
        .canonicalize()
        .map_err(|_| "Cannot read caCertificateFile".to_string())?;
    if !cert_path.starts_with(&base) {
        return Err("caCertificateFile must remain inside the config directory".into());
    }
    println!("cargo:rerun-if-changed={}", cert_path.display());
    let pem =
        fs::read_to_string(&cert_path).map_err(|_| "Cannot read caCertificateFile".to_string())?;
    let pem = pem.replace("\r\n", "\n").trim().to_string() + "\n";
    if pem.len() > 32_768
        || !pem.starts_with("-----BEGIN CERTIFICATE-----\n")
        || !pem.ends_with("-----END CERTIFICATE-----\n")
        || pem.matches("-----BEGIN ").count() != 1
    {
        return Err("caCertificateFile must contain exactly one public PEM certificate".into());
    }
    let certs = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "caCertificateFile contains an invalid certificate".to_string())?;
    if certs.len() != 1 || certs[0].is_empty() {
        return Err("caCertificateFile must contain exactly one public PEM certificate".into());
    }
    rustls::RootCertStore::empty()
        .add(certs[0].clone())
        .map_err(|_| "caCertificateFile is not a valid X.509 certificate".to_string())?;
    let mut hash = Sha256::new();
    hash.update(deployment_id.as_bytes());
    hash.update([0]);
    hash.update(origin.as_bytes());
    hash.update([0]);
    hash.update(certs[0].as_ref());
    let fingerprint = format!("{:x}", hash.finalize());
    Ok(Some((deployment_id, origin, pem, fingerprint)))
}

fn main() {
    println!("cargo:rerun-if-env-changed=GPR_CLIENT_CONFIG");
    let profile = public_profile().unwrap_or_else(|error| panic!("{error}"));
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("embedded_public_profile.rs");
    let generated = match profile {
        Some((deployment_id, url, ca_pem, fingerprint)) => format!(
            "pub const EMBEDDED_PUBLIC_PROFILE: Option<(&str, &str, &str)> = Some(({deployment_id:?}, {url:?}, {ca_pem:?}));\n\
             pub const BUILD_PROFILE_TAG: &str = \"GBFP-PUBLIC-PROFILE-V1:{fingerprint}\";\n"
        ),
        None => "pub const EMBEDDED_PUBLIC_PROFILE: Option<(&str, &str, &str)> = None;\n\
                 pub const BUILD_PROFILE_TAG: &str = \"GBFP-PUBLIC-PROFILE-V1:generic\";\n".into(),
    };
    fs::write(output, generated).expect("Write embedded public profile");
    println!("cargo:rerun-if-changed=windows.manifest");
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows.manifest")),
    ))
    .expect("Tauri build");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // tauri-build embeds resources in the product binary, not example harnesses.
        // The lifecycle example also imports the Common Controls v6 subclass API.
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("windows.manifest");
        println!("cargo:rustc-link-arg-examples=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-examples=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
