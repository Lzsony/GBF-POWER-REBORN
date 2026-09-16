fn main() {
    let port = std::env::args()
        .nth(1)
        .map(|v| v.parse::<u16>().expect("port"))
        .unwrap_or(8123);
    let r = serde_json::json!({
        "hosts": gbf_core::rules::API_HOSTS.iter().chain(gbf_core::rules::ASSET_HOSTS).chain(gbf_core::rules::TUNNEL_HOSTS).collect::<Vec<_>>(),
        "domains": gbf_core::rules::PROXY_DOMAINS,
        "ports": [80, 443],
        "pac": gbf_core::rules::pac(port),
    });
    println!("{}", r);
}
