//! Explicit native opener verification. Opens only the two fixed public game pages.
#[path = "../src/sites.rs"]
mod sites;
use tauri_plugin_opener::OpenerExt;
fn main() {
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = "cc.lzsony.gbf-power-reborn.site-links-test".into();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            for site in ["mobage", "steam"] {
                app.opener().open_url(sites::url(site)?, None::<&str>)?;
                println!("Default browser opened: {site}");
            }
            app.handle().exit(0);
            Ok(())
        })
        .run(context)
        .expect("native opener test failed");
}
