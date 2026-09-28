// SPDX-License-Identifier: GPL-3.0-only

//! Defense-in-depth log redaction (docs/PLAN.md §14).
//!
//! This is a *safety net*, not the primary control — the primary rule is "never construct a
//! tracing field / log line containing a secret" in the first place (INV-4). No regex dependency
//! in Wave 1: a small manual scanner is enough to catch `Bearer <token>`, `sk-...`-style API keys
//! and JWT-shaped strings that slip into a message.

use std::io::Write as _;

/// Returns `input` with anything that looks like a bearer token / API key / JWT replaced by
/// `<redacted>`. Idempotent and safe to call on secret-free strings.
///
/// Scans runs of token characters rather than whitespace-separated words, so secrets embedded in
/// JSON / form bodies (`{"access_token":"..."}`, `refresh_token=...`) are caught too. A run is
/// redacted when it looks like a secret on its own, follows the keyword `Bearer`, or is the value
/// of a known secret-bearing key.
pub fn redact(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending: Option<Pending> = None;
    let mut rest = input;
    while !rest.is_empty() {
        let run_len = rest
            .char_indices()
            .find(|&(_, c)| !is_token_char(c))
            .map_or(rest.len(), |(i, _)| i);
        if run_len == 0 {
            let c = rest.chars().next().unwrap_or(' ');
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let run = &rest[..run_len];
        rest = &rest[run_len..];
        if run == REDACTED_CORE {
            out.push_str(run);
            pending = None;
            continue;
        }
        let force = pending.take().is_some();
        if force || looks_like_secret(run) {
            out.push_str("<redacted>");
        } else {
            out.push_str(run);
        }
        if run.eq_ignore_ascii_case("bearer") || is_secret_key(run) {
            pending = Some(Pending);
        }
    }
    out
}

/// Marker: the next token run is a secret value.
struct Pending;

/// Inner text of the `<redacted>` marker; skipping it keeps `redact` idempotent.
const REDACTED_CORE: &str = "redacted";

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '~' | '+' | '/')
}

/// Keys whose value is a credential (JSON fields, form fields, headers).
fn is_secret_key(run: &str) -> bool {
    let key = run.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "access_token"
            | "refresh_token"
            | "id_token"
            | "api_key"
            | "apikey"
            | "x-api-key"
            | "x-goog-api-key"
            | "client_secret"
            | "code_verifier"
            | "password"
            | "token"
            | "authorization"
    )
}

fn looks_like_secret(word: &str) -> bool {
    let stripped = word.trim_matches(|c: char| matches!(c, ',' | ';' | ')' | '"' | '\'' | '='));
    if stripped.len() < 16 {
        return false;
    }
    if stripped.starts_with("sk-") || stripped.starts_with("rk-") {
        return true;
    }
    // JWT-shaped: three dot-separated base64url segments.
    if stripped.split('.').count() == 3
        && stripped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return true;
    }
    false
}

// --- tracing integration ---------------------------------------------------------------------

/// A `tracing_subscriber::fmt::MakeWriter` wrapper that passes every formatted log line through
/// [`redact`] before it reaches the underlying writer.
///
/// This is defense in depth (docs/PLAN.md §14), not the primary control — call sites must still
/// never construct a tracing field/message containing a secret in the first place (INV-4). `app`
/// installs this by wrapping whatever writer it would otherwise pass to
/// `tracing_subscriber::fmt().with_writer(...)`, e.g.:
///
/// ```ignore
/// tracing_subscriber::fmt()
///     .with_writer(xlightcli_auth::redact::RedactingMakeWriter::new(std::io::stderr))
///     .init();
/// ```
#[derive(Debug, Clone)]
pub struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    pub fn new(inner: M) -> Self {
        Self { inner }
    }
}

impl<'a, M> tracing_subscriber::fmt::MakeWriter<'a> for RedactingMakeWriter<M>
where
    M: tracing_subscriber::fmt::MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
            buffer: Vec::new(),
        }
    }
}

/// `io::Write` returned by [`RedactingMakeWriter`]. Buffers everything written to it (one
/// `tracing-subscriber` formatted event, in practice) and redacts on `flush`/`Drop` so a secret
/// split across multiple `write` calls within the same event still gets caught.
#[derive(Debug)]
pub struct RedactingWriter<W: std::io::Write> {
    inner: W,
    buffer: Vec<u8>,
}

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buffer.is_empty() {
            let text = String::from_utf8_lossy(&self.buffer);
            let redacted = redact(&text);
            self.inner.write_all(redacted.as_bytes())?;
            self.buffer.clear();
        }
        self.inner.flush()
    }
}

impl<W: std::io::Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        // `tracing-subscriber` doesn't guarantee an explicit `flush()` per event; make sure a
        // buffered-but-unflushed line still gets redacted-and-written rather than silently lost.
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::{Arc, Mutex};

    use pretty_assertions::assert_eq;

    use super::*;

    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn redacting_make_writer_strips_secret_from_formatted_log_line() {
        let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
        let buf_for_writer = SharedBuf(Arc::clone(&captured));
        let make_writer = RedactingMakeWriter::new(move || buf_for_writer.clone());

        let subscriber = tracing_subscriber::fmt()
            .with_writer(make_writer)
            .without_time()
            .with_level(false)
            .with_target(false)
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("token leaked: Bearer XLC-SENTINEL-SECRET-abcdefgh1234");
        });

        let output = String::from_utf8(
            captured
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        )
        .unwrap();
        assert!(!output.contains("XLC-SENTINEL-SECRET"));
        assert!(output.contains("<redacted>"));
    }

    #[test]
    fn redacts_secrets_inside_json_and_form_bodies() {
        let json =
            r#"{"error":"x","access_token":"XLC-SENTINEL-SECRET","echo":"Bearer XLC-SENTINEL-2"}"#;
        let out = redact(json);
        assert!(!out.contains("XLC-SENTINEL"), "{out}");
        assert!(out.contains(r#""error":"x""#), "{out}");
        let form = "grant_type=refresh_token&refresh_token=XLC-SENTINEL-SECRET&client_id=abc";
        let out = redact(form);
        assert!(!out.contains("XLC-SENTINEL"), "{out}");
        assert!(out.contains("client_id=abc"), "{out}");
    }

    #[test]
    fn redact_is_idempotent() {
        let once = redact(r#"Authorization: Bearer abcdefghijklmnopqrstuvwxyz {"api_key":"k"}"#);
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn redacts_bearer_token() {
        let input = "Authorization: Bearer XLC-SENTINEL-SECRET-abcdefgh1234";
        let out = redact(input);
        assert!(!out.contains("SENTINEL"));
        assert!(out.contains("<redacted>"));
    }

    #[test]
    fn redacts_sk_prefixed_key() {
        let input = "api_key sk-abcdefghijklmnopqrstuvwxyz";
        let out = redact(input);
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn redacts_jwt_shaped_token() {
        let input = "token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dGhpc2lzYXNpZ25hdHVyZQ";
        let out = redact(input);
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn leaves_normal_text_untouched() {
        let input = "hello world, this is a normal log line without secrets";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn is_idempotent() {
        let input = "Authorization: Bearer XLC-SENTINEL-SECRET-abcdefgh1234";
        let once = redact(input);
        let twice = redact(&once);
        assert_eq!(once, twice);
    }
}
