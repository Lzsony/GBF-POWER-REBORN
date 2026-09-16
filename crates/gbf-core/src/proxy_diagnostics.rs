//! Bounded diagnostics: never format a source error or request data.
use rand_core::{OsRng, RngCore};
use std::{
    error::Error,
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::Instant,
};

pub(crate) const REQUEST_ID_HEADER: &str = "x-gbf-proxy-request-id";

#[derive(Clone)]
pub(crate) struct Diagnostic {
    pub id: String,
    pub attempt: u8,
    pub phase: &'static str,
    pub cache: &'static str,
    pub route: &'static str,
    pub mode: &'static str,
    started: Instant,
}

impl Diagnostic {
    pub fn new() -> Self {
        static RUN: OnceLock<String> = OnceLock::new();
        static SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let run = RUN.get_or_init(|| {
            let mut bytes = [0u8; 16];
            OsRng.fill_bytes(&mut bytes);
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        });
        Self {
            id: format!("{run}-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed)),
            attempt: 1,
            phase: "internal",
            cache: "not_applicable",
            route: "unselected",
            mode: "unknown",
            started: Instant::now(),
        }
    }

    pub fn failure(&self, error: &(dyn Error + 'static), status: Option<u16>) {
        let kind = classify(error, self.phase);
        tracing::warn!(
            request_id = %self.id, attempt = self.attempt, phase = self.phase, cache_state = self.cache,
            route = self.route, mode = self.mode, failure_kind = kind,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            http_status = status, "proxy_request_failed"
        );
    }

    pub fn recovery(&self, event: &'static str, reason: &'static str) {
        match event {
            "proxy_request_retry" => tracing::info!(request_id = %self.id, attempt = self.attempt,
                phase = self.phase, cache_state = self.cache, failure_kind = reason,
                elapsed_ms = self.started.elapsed().as_millis() as u64, "proxy_request_retry"),
            _ => tracing::info!(request_id = %self.id, attempt = self.attempt,
                phase = self.phase, cache_state = self.cache, outcome = reason,
                elapsed_ms = self.started.elapsed().as_millis() as u64, "proxy_request_recovered"),
        }
    }

    pub fn upstream_status(&self, status: u16) {
        tracing::warn!(
            request_id = %self.id, attempt = self.attempt, phase = self.phase, cache_state = self.cache,
            route = self.route, mode = self.mode, failure_kind = "UPSTREAM_HTTP_STATUS",
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            http_status = status, "proxy_upstream_http_error"
        );
    }
}

pub(crate) fn classify(error: &(dyn Error + 'static), phase: &str) -> &'static str {
    let mut cursor = Some(error);
    let mut fallback = "UNKNOWN";
    while let Some(cause) = cursor {
        if cause.is::<tokio::time::error::Elapsed>() {
            return "TIMEOUT";
        }
        if let Some(e) = cause.downcast_ref::<rustls::Error>() {
            return match e {
                rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented => {
                    "TLS_VERIFICATION"
                }
                _ => "TLS_PROTOCOL",
            };
        }
        if let Some(e) = cause.downcast_ref::<io::Error>() {
            // io::Error::source can skip its wrapped error itself. Inspect get_ref
            // so rustls certificate errors inside InvalidData retain their type.
            if let Some(inner) = e.get_ref() {
                let nested = classify(inner, "internal");
                if nested != "UNKNOWN" {
                    return nested;
                }
            }
            let kind = match e.kind() {
                io::ErrorKind::TimedOut => Some("TIMEOUT"),
                io::ErrorKind::ConnectionRefused => Some("CONNECTION_REFUSED"),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => {
                    Some("CONNECTION_RESET")
                }
                io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe => Some("EARLY_EOF"),
                _ => None,
            };
            if let Some(kind) = kind {
                return kind;
            }
        }
        if let Some(e) = cause.downcast_ref::<hyper::Error>() {
            if e.is_timeout() {
                return "TIMEOUT";
            }
            if e.is_incomplete_message() || e.is_closed() {
                fallback = "EARLY_EOF";
            } else if e.is_parse() {
                fallback = "HTTP_PROTOCOL";
            } else if e.is_canceled() {
                fallback = "REQUEST_CANCELED";
            }
        }
        if let Some(e) = cause.downcast_ref::<h2::Error>() {
            // Do not format GOAWAY debug payloads, which are arbitrary peer data.
            match e.reason() {
                Some(h2::Reason::REFUSED_STREAM) => return "HTTP2_REFUSED_STREAM",
                Some(h2::Reason::CANCEL) => return "HTTP2_CANCEL",
                _ if e.is_go_away() => return "HTTP2_GOAWAY",
                _ if e.is_reset() => return "HTTP2_RESET",
                Some(_) => return "HTTP_PROTOCOL",
                None => (),
            }
        }
        if let Some(e) = cause.downcast_ref::<reqwest::Error>() {
            if e.is_timeout() {
                return "TIMEOUT";
            }
            // More specific typed sources take precedence over these broad categories.
            if e.is_connect() {
                fallback = "CONNECT_FAILED";
            } else if e.is_body() || e.is_decode() {
                fallback = "BODY_READ_FAILED";
            }
        }
        if cause.is::<tokio::task::JoinError>() {
            fallback = "INTERNAL";
        }
        cursor = cause.source();
    }
    if phase == "cache_revalidation" {
        "CACHE_REVALIDATION"
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_share_run_but_are_unique() {
        let a = Diagnostic::new();
        let b = Diagnostic::new();
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.split('-').next(), b.id.split('-').next());
        assert!(http::HeaderValue::from_str(&a.id).is_ok());
    }
    #[test]
    fn typed_causes_and_unknown_do_not_use_error_text() {
        for (kind, expected) in [
            (io::ErrorKind::ConnectionRefused, "CONNECTION_REFUSED"),
            (io::ErrorKind::ConnectionReset, "CONNECTION_RESET"),
            (io::ErrorKind::UnexpectedEof, "EARLY_EOF"),
            (io::ErrorKind::TimedOut, "TIMEOUT"),
        ] {
            let wrapped = anyhow::Error::new(io::Error::new(kind, "https://secret?token=private"))
                .context("secret wrapper");
            assert_eq!(classify(wrapped.as_ref(), "upstream_send"), expected);
        }
        assert_eq!(
            classify(&io::Error::other("TLS timeout password"), "upstream_send"),
            "UNKNOWN"
        );
        assert_eq!(
            classify(&io::Error::other("secret"), "cache_revalidation"),
            "CACHE_REVALIDATION"
        );
        assert_eq!(
            classify(
                &rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
                "upstream_send"
            ),
            "TLS_VERIFICATION"
        );
    }

    #[test]
    fn http2_reasons_are_bounded_categories() {
        for (reason, kind) in [
            (h2::Reason::REFUSED_STREAM, "HTTP2_REFUSED_STREAM"),
            (h2::Reason::CANCEL, "HTTP2_CANCEL"),
            (h2::Reason::PROTOCOL_ERROR, "HTTP_PROTOCOL"),
        ] {
            assert_eq!(classify(&h2::Error::from(reason), "upstream_send"), kind);
        }
    }
}
