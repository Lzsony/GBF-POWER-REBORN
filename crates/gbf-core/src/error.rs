use serde::Serialize;

/// Stable user-facing categories. Never embed raw URLs, credentials, or OS error strings.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    UrlInvalid,
    ProtocolUnsupported,
    UrlComponents,
    CredentialEncoding,
    CredentialMasked,
    UsernameRequired,
    UsernameColon,
    CredentialLength,
    ListenPortInvalid,
    CacheLimitInvalid,
    UpstreamHostInvalid,
    UpstreamPortInvalid,
    ProxyLoop,
    StopRequired,
    SwitchFailedRestored,
    SwitchRestoreFailed,
    BrowserOpenFailed,
    CacheMaintenanceBusy,
    CacheAuditFailed,
    CertificateRequired,
    SecretReadFailed,
    SecretWriteFailed,
    ConfigReadFailed,
    ConfigWriteFailed,
    ProxyBindFailed,
    DnsFailed,
    ConnectionTestFailed,
    CertificateInstallFailed,
    CertificateRemoveFailed,
    CertificateOperationTimeout,
    CertificateMissing,
    NativeOperationFailed,
    AutostartFailed,
    DirectoryOpenFailed,
    InvalidCommand,
    InternalFailure,
}
impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ErrorCode {}

#[derive(Clone, Debug, Serialize)]
pub struct CommandError {
    pub code: ErrorCode,
}
impl CommandError {
    pub fn from_error(error: impl Into<anyhow::Error>, fallback: ErrorCode) -> Self {
        let error = error.into();
        let code = error
            .downcast_ref::<ErrorCode>()
            .copied()
            .or_else(|| {
                error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<ErrorCode>().copied())
            })
            .unwrap_or(fallback);
        Self { code }
    }
}
impl From<ErrorCode> for CommandError {
    fn from(code: ErrorCode) -> Self {
        Self { code }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_errors_are_codes_only() {
        let e = CommandError::from_error(
            anyhow::anyhow!("socks5://user:password@host"),
            ErrorCode::InternalFailure,
        );
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"code":"INTERNAL_FAILURE"}"#
        );
        let e = CommandError::from_error(
            anyhow::Error::new(ErrorCode::ProxyLoop).context("private detail"),
            ErrorCode::InternalFailure,
        );
        assert_eq!(e.code, ErrorCode::ProxyLoop);
        let e = CommandError::from_error(
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::AddrInUse))
                .context(ErrorCode::ProxyBindFailed)
                .context("private OS detail"),
            ErrorCode::InternalFailure,
        );
        assert_eq!(e.code, ErrorCode::ProxyBindFailed);
    }
}
