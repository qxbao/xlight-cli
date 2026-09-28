// SPDX-License-Identifier: GPL-3.0-only

//! `tracing-subscriber` wiring (PATTERNS.md §14): writes redacted logs to
//! `$XDG_STATE_HOME/xlightcli/logs/xlightcli.log`; `-v` additionally echoes debug-level logs to
//! stderr. TUI/user-facing output never goes through `tracing` (that's `crate::output`).
//!
//! `xlightcli-auth` doesn't yet expose a purpose-built redacting `MakeWriter` (Wave 2 brief:
//! "through the auth crate's redaction writer if available (else apply
//! `xlightcli_auth::redact::redact`)"), so `RedactingWriter` below wraps any writer and applies
//! `xlightcli_auth::redact::redact` to every formatted line as defense in depth (INV-4) — the
//! primary control is still "never put a secret in a tracing field" in the first place.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Wraps a `MakeWriter` so every write is redacted first.
#[derive(Clone)]
struct RedactingMakeWriter<W> {
    inner: W,
}

struct RedactingWriteHandle<H> {
    inner: H,
}

impl<H: io::Write> io::Write for RedactingWriteHandle<H> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let redacted = xlightcli_auth::redact::redact(&text);
        self.inner.write_all(redacted.as_bytes())?;
        // Report the caller's original length: tracing's writer contract only checks this
        // against `buf`, and a redacted line is never longer 1:1 per input byte anyway.
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<'a, W> MakeWriter<'a> for RedactingMakeWriter<W>
where
    W: MakeWriter<'a>,
{
    type Writer = RedactingWriteHandle<W::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriteHandle {
            inner: self.inner.make_writer(),
        }
    }
}

fn ensure_dir_exists(dir: &Path) {
    if let Err(err) = std::fs::create_dir_all(dir) {
        crate::output::warn(&format!(
            "could not create log dir {}: {err}",
            dir.display()
        ));
    }
}

fn open_log_file(dir: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("xlightcli.log"))
}

/// Builds the (file + optional stderr) layered subscriber. Split out from `init`/`init_with_dir`
/// so tests can scope it to just one test via `tracing::subscriber::set_default` instead of
/// racing every other test in the binary for the process-global slot `try_init` claims.
fn build_subscriber(dir: &Path, verbose: bool) -> impl tracing::Subscriber + Send + Sync + 'static {
    ensure_dir_exists(dir);
    let log_path = dir.join("xlightcli.log");

    let file_layer = match open_log_file(dir) {
        Ok(file) => Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(RedactingMakeWriter {
                    inner: Mutex::new(file),
                })
                .with_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug")),
                ),
        ),
        Err(err) => {
            crate::output::warn(&format!(
                "could not open log file {}: {err} (file logging disabled)",
                log_path.display()
            ));
            None
        }
    };

    let stderr_layer = verbose.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(RedactingMakeWriter { inner: io::stderr })
            .with_filter(EnvFilter::new("debug"))
    });

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
}

/// Initializes the global `tracing` subscriber, writing to
/// `$XDG_STATE_HOME/xlightcli/logs/xlightcli.log` (docs/CODEBASE.md §6).
pub fn init(verbose: bool) {
    init_with_dir(&xlightcli_config::paths::logs_dir(), verbose);
}

/// Same as [`init`], but with an explicit log directory — split out so tests/callers can point it
/// at a temp dir without mutating process-wide env vars (which would need `unsafe`, forbidden by
/// `[workspace.lints]`; see `xlightcli_config::paths` for the same pattern). Idempotent
/// (`try_init` — a second call anywhere in the process is a harmless no-op).
pub fn init_with_dir(dir: &Path, verbose: bool) {
    let _ = build_subscriber(dir, verbose).try_init();
}

/// Test-only: scopes `build_subscriber` to the current task/thread for the lifetime of the
/// returned guard, via `tracing::subscriber::set_default` rather than the process-global
/// `try_init` (which only the first caller in the whole test binary would actually win).
#[cfg(test)]
pub fn scoped_for_test(dir: &Path) -> tracing::subscriber::DefaultGuard {
    tracing::subscriber::set_default(build_subscriber(dir, false))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn redacting_write_handle_scrubs_secrets_before_writing() {
        let mut buf = Vec::new();
        {
            let mut handle = RedactingWriteHandle { inner: &mut buf };
            io::Write::write_all(
                &mut handle,
                b"Authorization: Bearer XLC-SENTINEL-abcdefgh1234\n",
            )
            .unwrap();
        }
        let text = String::from_utf8(buf).unwrap();
        assert!(!text.contains("SENTINEL"));
        assert!(text.contains("<redacted>"));
    }
}
