use gbf_core::error::ErrorCode;
pub fn url(site: &str) -> Result<&'static str, ErrorCode> {
    match site {
        "mobage" => Ok("https://game.granbluefantasy.jp/"),
        "steam" => Ok("https://steam.granbluefantasy.com/"),
        _ => Err(ErrorCode::InvalidCommand),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_sites_only() {
        assert_eq!(url("mobage").unwrap(), "https://game.granbluefantasy.jp/");
        assert_eq!(url("steam").unwrap(), "https://steam.granbluefantasy.com/");
        assert!(url("https://example.com/").is_err());
        assert!(url("file:///etc/passwd").is_err());
    }
}
