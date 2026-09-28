// SPDX-License-Identifier: GPL-3.0-only

//! Fixture-driven translator tests (PATTERNS.md §13): `tests/fixtures/*.sse` → `Translator` →
//! `insta` snapshot. Lives inside the crate (not `tests/*.rs`) because `Translator` is
//! `pub(crate)` (INV-3: no wire type — including the translator itself — may appear in a `pub`
//! signature); an external integration-test crate could not see it.
//!
//! Every fixture has a `# synthetic fixture modeled on <source>` header and no real tokens/ids.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use xlightcli_protocol::{AgentEvent, ModelId, ProviderId, TransportId};

use super::response::Translator;

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Like `{:?}`, but redacts `RateLimitInfo::reset_at`. It's computed as
/// `OffsetDateTime::now_utc()` plus a delta (`wire::response::Translator::on_rate_limits`), so a
/// literal timestamp would make the snapshot flaky (different wall-clock time on every test run).
fn format_event(event: AgentEvent) -> String {
    match &event {
        AgentEvent::RateLimit(info) => format!(
            "RateLimit(RateLimitInfo {{ limit: {:?}, remaining: {:?}, reset_at: {} }})",
            info.limit,
            info.remaining,
            if info.reset_at.is_some() {
                "Some(<redacted: now + delta>)"
            } else {
                "None"
            }
        ),
        other => format!("{other:?}"),
    }
}

/// Runs `name` fully through a fresh `Translator`, returning every event `feed()` produced plus
/// the final `finish()` event appended (or an error, formatted as `Debug`, in its place) — mirrors
/// exactly what a real `TransportAdapter::stream()` yields (PATTERNS.md §6).
async fn translate_fixture(name: &str) -> Vec<String> {
    let sse_events = xlightcli_provider::testing::parse_sse_fixture(&fixture_path(name))
        .await
        .unwrap();
    let mut translator = Translator::new(
        ProviderId::new("codex"),
        TransportId::new("chatgpt"),
        ModelId::new("gpt-5-codex"),
    );
    let mut out = Vec::new();
    let mut failed = false;
    for event in sse_events {
        match translator.feed(event) {
            Ok(events) => out.extend(events.into_iter().map(format_event)),
            Err(err) => {
                out.push(format!("Err({err:?})"));
                failed = true;
                break;
            }
        }
    }
    if !failed {
        match translator.finish() {
            Ok(event) => out.push(format!("{event:?}")),
            Err(err) => out.push(format!("Err({err:?})")),
        }
    }
    out
}

#[tokio::test]
async fn plain_text_fixture() {
    let events = translate_fixture("text.sse").await;
    insta::assert_debug_snapshot!(events);
}

#[tokio::test]
async fn reasoning_encrypted_fixture() {
    let events = translate_fixture("reasoning_encrypted.sse").await;
    insta::assert_debug_snapshot!(events);
}

#[tokio::test]
async fn tool_call_fixture() {
    let events = translate_fixture("tool_call.sse").await;
    insta::assert_debug_snapshot!(events);
}

#[tokio::test]
async fn parallel_tool_calls_fixture() {
    let events = translate_fixture("parallel_tool_calls.sse").await;
    insta::assert_debug_snapshot!(events);
}

#[tokio::test]
async fn error_mid_stream_fixture() {
    let events = translate_fixture("error_mid_stream.sse").await;
    insta::assert_debug_snapshot!(events);
}

#[tokio::test]
async fn rate_limit_fixture() {
    let events = translate_fixture("rate_limit.sse").await;
    insta::assert_debug_snapshot!(events);
}
