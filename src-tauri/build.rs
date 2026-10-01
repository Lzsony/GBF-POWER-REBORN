use std::{env, fs, path::PathBuf};
#[path = "../build-support/public_profile.rs"]
mod public_profile;
use public_profile::public_profile;

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
