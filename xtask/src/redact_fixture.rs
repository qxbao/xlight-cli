// SPDX-License-Identifier: GPL-3.0-only

//! `xtask redact-fixture` — scrubs secrets/account identifiers from `.sse`/`.json` fixture files
//! before they can be committed (docs/PLAN.md §16, PATTERNS.md §5/§13).
//!
//! Two complementary passes:
//! - **Structural**: parses embedded JSON (a whole `.json` fixture, or each SSE `data: {...}`
//!   line) and replaces the value of any key in `SENSITIVE_JSON_KEYS` (case-insensitive) with a
//!   deterministic placeholder, regardless of what the value looks like.
//! - **Pattern**: a single combined regex over the resulting text catches bearer tokens, JWTs,
//!   `sk-…` API keys, emails, and UUIDs wherever they appear (including outside any JSON
//!   structure, e.g. in a raw header line).
//!
//! Placeholders are `REDACTED_<KIND>_<8 hex chars>`, where the hex suffix is a truncated SHA-256
//! of the *original* value: deterministic (same secret -> same placeholder, so relational
//! structure across repeated occurrences in one fixture is preserved) without being reversible.
//! Both passes are idempotent — re-running on already-redacted text is a no-op — which is what
//! makes `--check` usable as a pre-commit gate.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use regex::{Captures, Regex};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// JSON object keys (matched case-insensitively) whose string value is always replaced,
/// regardless of shape. Covers refresh/access tokens plus the account/identity fields called out
/// in the Wave 2 brief.
const SENSITIVE_JSON_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "api_key",
    "client_secret",
    "authorization",
    "account_id",
    "chatgpt_account_id",
    "project",
    "project_id",
    "email",
    "sub",
    "org_id",
    "organization_id",
    "session_key",
    "secret",
    "password",
    "cookie",
];

/// Runs `redact-fixture` on `file`. In `--check` mode, returns an error (without writing) if
/// redaction would change the file; otherwise writes the redacted content in place.
pub fn run(file: &Path, check: bool) -> anyhow::Result<()> {
    let original = fs::read_to_string(file)
        .with_context(|| format!("failed to read fixture {}", file.display()))?;
    let redacted = redact_text(&original);

    if redacted == original {
        print_unchanged(file);
        return Ok(());
    }
    if check {
        bail!(
            "xtask redact-fixture --check: {} is not fully redacted — run `cargo xtask \
             redact-fixture {}` before committing",
            file.display(),
            file.display()
        );
    }
    fs::write(file, &redacted)
        .with_context(|| format!("failed to write redacted fixture {}", file.display()))?;
    print_redacted(file);
    Ok(())
}

// `xtask` is a dev-only CLI, not a library (PATTERNS.md §14's "no println in library" doesn't
// apply); these are its user-facing output, kept as tiny named functions so `#[allow]` stays
// narrowly scoped.
#[allow(clippy::print_stdout)]
fn print_unchanged(file: &Path) {
    println!(
        "xtask redact-fixture: {} already redacted (no changes)",
        file.display()
    );
}

#[allow(clippy::print_stdout)]
fn print_redacted(file: &Path) {
    println!("xtask redact-fixture: redacted {}", file.display());
}

/// Deterministic placeholder for `original` under semantic label `kind` (e.g. `"bearer"`,
/// `"account_id"`). Same `(kind, original)` always yields the same string.
fn placeholder(kind: &str, original: &str) -> String {
    let digest = Sha256::digest(original.as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("REDACTED_{}_{short}", kind.to_uppercase())
}

fn redact_json_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = Map::with_capacity(map.len());
            for (key, val) in map {
                let lower = key.to_lowercase();
                let already_string_redacted =
                    matches!(&val, Value::String(s) if s.starts_with("REDACTED_"));
                let replacement =
                    if SENSITIVE_JSON_KEYS.contains(&lower.as_str()) && !already_string_redacted {
                        match val {
                            Value::String(s) => Value::String(placeholder(&lower, &s)),
                            other => redact_json_value(other),
                        }
                    } else {
                        redact_json_value(val)
                    };
                redacted.insert(key, replacement);
            }
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_json_value).collect()),
        other => other,
    }
}

#[allow(clippy::expect_used)] // hardcoded pattern known valid at compile time; failure is a programmer error, covered by tests
fn combined_pattern_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?P<bearer>Bearer\s+[A-Za-z0-9\-_.=]{8,})",
            r"|(?P<jwt>[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,})",
            r"|(?P<skkey>sk-[A-Za-z0-9]{10,})",
            r"|(?P<email>[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,})",
            r"|(?P<uuid>[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})",
        ))
        .expect("combined_pattern_regex: static pattern must compile")
    })
}

fn replace_match(caps: &Captures<'_>) -> String {
    if let Some(m) = caps.name("bearer") {
        let token = m.as_str().trim_start_matches("Bearer").trim_start();
        if token.starts_with("REDACTED_") {
            return m.as_str().to_string(); // already redacted: idempotent no-op
        }
        return format!("Bearer {}", placeholder("bearer", token));
    }
    if let Some(m) = caps.name("jwt") {
        return placeholder("jwt", m.as_str());
    }
    if let Some(m) = caps.name("skkey") {
        return placeholder("api_key", m.as_str());
    }
    if let Some(m) = caps.name("email") {
        return placeholder("email", m.as_str());
    }
    if let Some(m) = caps.name("uuid") {
        return placeholder("uuid", m.as_str());
    }
    caps.get(0)
        .map(|m| m.as_str())
        .unwrap_or_default()
        .to_string()
}

fn apply_pattern_redaction(input: &str) -> String {
    combined_pattern_regex()
        .replace_all(input, replace_match)
        .into_owned()
}

/// Structural + pattern redaction over the whole fixture text. Handles three shapes:
/// 1. A whole-file JSON document (typical `.json` fixture).
/// 2. SSE `data: {...}` lines (typical `.sse` fixture) — each parsed/redacted independently.
/// 3. Anything else — left untouched by the structural pass, still covered by the pattern pass.
fn redact_text(input: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(input) {
        let redacted = redact_json_value(value);
        if let Ok(pretty) = serde_json::to_string_pretty(&redacted) {
            return apply_pattern_redaction(&pretty);
        }
    }

    let mut out_lines: Vec<String> = Vec::with_capacity(input.lines().count());
    for line in input.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            let trimmed = rest.trim_start();
            if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                let redacted = redact_json_value(value);
                if let Ok(compact) = serde_json::to_string(&redacted) {
                    out_lines.push(format!("data: {compact}"));
                    continue;
                }
            }
        }
        out_lines.push(line.to_string());
    }
    let mut joined = out_lines.join("\n");
    if input.ends_with('\n') {
        joined.push('\n');
    }
    apply_pattern_redaction(&joined)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn redacts_bearer_token() {
        let input = "Authorization: Bearer sk-live-ABCDEFGHIJKLMNOP1234567890\n";
        let out = redact_text(input);
        assert!(!out.contains("ABCDEFGHIJKLMNOP1234567890"));
        assert!(out.contains("Bearer REDACTED_BEARER_"));
    }

    #[test]
    fn redacts_bare_jwt() {
        let input = "id_token=eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signaturepartxyz";
        let out = redact_text(input);
        assert!(!out.contains("eyJhbGciOiJSUzI1NiJ9"));
        assert!(out.contains("REDACTED_JWT_"));
    }

    #[test]
    fn redacts_sk_style_api_key() {
        let input = "key: sk-abcdefghijklmnopqrstuvwxyz\n";
        let out = redact_text(input);
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(out.contains("REDACTED_API_KEY_"));
    }

    #[test]
    fn redacts_email_addresses() {
        let input = "contact: user.name+test@example.com\n";
        let out = redact_text(input);
        assert!(!out.contains("user.name+test@example.com"));
        assert!(out.contains("REDACTED_EMAIL_"));
    }

    #[test]
    fn redacts_bare_uuids() {
        let input = "session=123e4567-e89b-12d3-a456-426614174000\n";
        let out = redact_text(input);
        assert!(!out.contains("123e4567-e89b-12d3-a456-426614174000"));
        assert!(out.contains("REDACTED_UUID_"));
    }

    #[test]
    fn redacts_known_json_keys_regardless_of_shape() {
        let input = r#"{"account_id":"acct_9f8e7d","chatgpt_account_id":"org-123abc","project":"proj_xyz","note":"keep me"}"#;
        let out = redact_text(input);
        assert!(!out.contains("acct_9f8e7d"));
        assert!(!out.contains("org-123abc"));
        assert!(!out.contains("proj_xyz"));
        assert!(out.contains("keep me"));
        assert!(out.contains("REDACTED_ACCOUNT_ID_"));
        assert!(out.contains("REDACTED_CHATGPT_ACCOUNT_ID_"));
        assert!(out.contains("REDACTED_PROJECT_"));
    }

    #[test]
    fn redacts_sse_data_lines_independently() {
        let input = "event: message\ndata: {\"access_token\":\"raw-secret-value\"}\n\n";
        let out = redact_text(input);
        assert!(!out.contains("raw-secret-value"));
        assert!(out.contains("event: message"));
        assert!(out.contains("REDACTED_ACCESS_TOKEN_"));
    }

    #[test]
    fn same_secret_redacts_to_the_same_placeholder_twice_in_one_fixture() {
        let input = r#"{"a":{"account_id":"same-id"},"b":{"account_id":"same-id"}}"#;
        let out = redact_text(input);
        let expected = placeholder("account_id", "same-id");
        assert_eq!(out.matches(expected.as_str()).count(), 2);
    }

    #[test]
    fn different_secrets_redact_to_different_placeholders() {
        let input = r#"{"a":{"account_id":"id-one"},"b":{"account_id":"id-two"}}"#;
        let out = redact_text(input);
        assert_ne!(
            placeholder("account_id", "id-one"),
            placeholder("account_id", "id-two")
        );
        assert!(out.contains(&placeholder("account_id", "id-one")));
        assert!(out.contains(&placeholder("account_id", "id-two")));
    }

    #[test]
    fn redaction_is_idempotent() {
        let input = "Authorization: Bearer sk-live-ABCDEFGHIJKLMNOP1234567890\n\
                      data: {\"account_id\":\"acct_9f8e7d\",\"email\":\"user@example.com\"}\n";
        let once = redact_text(input);
        let twice = redact_text(&once);
        assert_eq!(once, twice, "re-running redact-fixture must be a no-op");
    }

    #[test]
    fn check_mode_fails_on_unredacted_file_and_leaves_it_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sse");
        let original = "token=sk-abcdefghijklmnopqrstuvwxyz\n";
        fs::write(&path, original).unwrap();

        let err = run(&path, true).unwrap_err();
        assert!(err.to_string().contains("not fully redacted"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "--check must not write"
        );
    }

    #[test]
    fn check_mode_succeeds_on_already_redacted_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.sse");
        let raw = "token=sk-abcdefghijklmnopqrstuvwxyz\n";
        fs::write(&path, raw).unwrap();

        run(&path, false).unwrap(); // redact in place first
        run(&path, true).unwrap(); // now --check must pass
    }
}
