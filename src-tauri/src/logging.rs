use anyhow::Result;
use std::{collections::BTreeMap, fmt, fs, path::Path};
use tracing::{
    field::{Field, Visit},
    Event, Subscriber,
};
use tracing_subscriber::{
    fmt::{format::Writer, FmtContext, FormatEvent, FormatFields},
    registry::LookupSpan,
};

// Only these diagnostic values are allowed into the log. Never format raw errors.
const FIELDS: &[&str] = &[
    "phase",
    "elapsed_ms",
    "http_status",
    "outcome",
    "connected",
    "mode",
    "error_code",
    "request_id",
    "attempt",
    "failure_kind",
    "cache_state",
    "route",
];
#[derive(Default)]
struct Fields {
    event: String,
    values: BTreeMap<String, String>,
}
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.event = value.to_owned();
        } else if FIELDS.contains(&field.name()) {
            self.values
                .insert(field.name().into(), value.replace(['\r', '\n'], " "));
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.event = format!("{value:?}").trim_matches('"').to_owned();
        } else if FIELDS.contains(&field.name()) {
            self.values.insert(
                field.name().into(),
                format!("{value:?}").replace(['\r', '\n'], " "),
            );
        }
    }
}
fn description(event: &str) -> Option<(&'static str, Option<&'static str>)> {
    Some(match event {
        "proxy_started" => ("Proxy started", None),
        "proxy_stopped" => ("Proxy stopped", None),
        "cache_write_failed" => (
            "Could not save a cache entry to disk",
            Some("CACHE_WRITE_FAILED"),
        ),
        "cache_access_flush_failed" => (
            "Could not save cache access metadata",
            Some("CACHE_METADATA_WRITE_FAILED"),
        ),
        "listener_accept_failed" => (
            "Could not accept an incoming connection",
            Some("CONNECTION_ACCEPT_FAILED"),
        ),
        "proxy_request_failed" => (
            "Could not complete a proxy request",
            Some("PROXY_REQUEST_FAILED"),
        ),
        "proxy_request_retry" => ("Retrying a static asset request", None),
        "proxy_request_recovered" => ("Static asset request recovered", None),
        "proxy_upstream_http_error" => ("Upstream returned an HTTP error", None),
        "asset_tls_connection_failed" => (
            "Could not complete an asset TLS connection",
            Some("ASSET_TLS_FAILED"),
        ),
        "public_page_probe" => ("Public page connectivity check completed", None),
        "window_hide_failed" | "minimize_hide_failed" => (
            "Could not hide the application window",
            Some("WINDOW_HIDE_FAILED"),
        ),
        _ => return None,
    })
}
pub struct Readable;
impl<S, N> FormatEvent<S, N> for Readable
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut fields = Fields::default();
        event.record(&mut fields);
        // Unknown messages are omitted rather than risking raw diagnostic/secret output.
        let Some((message, error)) = description(&fields.event) else {
            return Ok(());
        };
        if let Some(error) = error {
            fields
                .values
                .entry("error_code".into())
                .or_insert(error.into());
        }
        let diagnostic_error = match fields.event.as_str() {
            "public_page_probe" => match fields.values.get("outcome").map(String::as_str) {
                Some("Timeout") => Some("PROBE_TIMEOUT"),
                Some("Failure") if fields.values.contains_key("http_status") => {
                    Some("PROBE_HTTP_STATUS")
                }
                Some("Failure") => Some("PROBE_CONNECTION_FAILED"),
                _ => None,
            },
            _ => None,
        };
        if let Some(error) = diagnostic_error {
            fields.values.insert("error_code".into(), error.into());
        }
        let timestamp = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| fmt::Error)?;
        write!(
            writer,
            "{timestamp} {} [{}] {message}",
            event.metadata().level(),
            fields.event
        )?;
        if !fields.values.is_empty() {
            write!(writer, " |")?;
        }
        for (key, value) in fields.values {
            write!(writer, " {key}={value}")?;
        }
        writeln!(writer)
    }
}

pub fn prepare(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    prune(root, 7)
}
fn prune(root: &Path, limit: usize) -> Result<()> {
    let mut files = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|entry| {
        entry.file_type().is_ok_and(|t| t.is_file())
            && entry.file_name().to_string_lossy().starts_with("events.")
            && entry.file_name().to_string_lossy().ends_with(".log")
    });
    files.sort_by_key(|entry| entry.file_name());
    for entry in files.iter().take(files.len().saturating_sub(limit)) {
        fs::remove_file(entry.path())?;
    }
    Ok(())
}
pub fn writer(root: &Path) -> Result<tracing_appender::rolling::RollingFileAppender> {
    prepare(root)?;
    let writer = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("events")
        .filename_suffix("log")
        .max_log_files(7)
        .build(root)?;
    prune(root, 7)?;
    Ok(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retention_is_bounded_and_unknown_files_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.txt"), "keep").unwrap();
        for day in 2..=10 {
            fs::write(
                root.path().join(format!("events.2026-09-{day:02}.log")),
                "event",
            )
            .unwrap();
        }
        prepare(root.path()).unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 8);
        assert!(root.path().join("notes.txt").exists());
    }
    #[test]
    fn diagnostic_fields_exclude_secrets_and_unknown_messages() {
        #[derive(Clone)]
        struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer(Default::default());
        let output = buffer.0.clone();
        let subscriber = tracing_subscriber::fmt()
            .event_format(Readable)
            .with_writer(move || buffer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                mode = "direct",
                password = "private",
                url = "https://secret",
                "proxy_started"
            );
            tracing::warn!("cache_write_failed");
            tracing::info!(
                phase = "initial",
                outcome = "Timeout",
                elapsed_ms = 5000,
                "public_page_probe"
            );
            tracing::warn!(
                request_id = "012345-1",
                phase = "upstream_send",
                failure_kind = "CONNECTION_RESET",
                cache_state = "miss",
                route = "upstream",
                http_status = 502,
                elapsed_ms = 123,
                error = "https://secret?token=private",
                "proxy_request_failed"
            );
            tracing::warn!("unknown secret text");
        });
        let text = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(text.contains("Z INFO [proxy_started] Proxy started |"));
        assert!(text.contains("error_code=CACHE_WRITE_FAILED"));
        assert!(text.contains("mode=direct"));
        assert!(text.contains("error_code=PROBE_TIMEOUT"));
        assert!(!text.contains("private") && !text.contains("secret"));
        assert!(text.contains("request_id=012345-1"));
        assert!(text.contains("failure_kind=CONNECTION_RESET"));
        assert!(text.contains("phase=upstream_send"));
        assert!(text.contains("cache_state=miss"));
        assert!(text.contains("http_status=502"));
        assert_eq!(text.lines().count(), 4);
    }
}
