// SPDX-License-Identifier: GPL-3.0-only

//! Sole place in `app` allowed to write straight to stdout/stderr (PATTERNS.md §14: libraries
//! never print; the binary's user-facing rendering lives here — the Wave 2 brief's "small output
//! module" with print lints allowed only here).
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{BufRead as _, Write as _};

use xlightcli_protocol::{AgentEvent, RateLimitInfo, StopReason, Usage};

use crate::cmd::error::CliError;

/// Assistant text delta: stdout (the model's actual answer), unbuffered so streaming feels live
/// and so `dev probe ... | some-pipe` sees only the answer text.
pub fn text_delta(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

/// Reasoning delta: dimmed (ANSI SGR 2), to stderr — commentary, not the model's answer (Wave 2
/// brief: "ReasoningDelta dimmed to stderr").
pub fn reasoning_delta(text: &str) {
    eprint!("\x1b[2m{text}\x1b[0m");
    let _ = std::io::stderr().flush();
}

pub fn tool_call_started(name: &str, id: &str) {
    eprintln!("\n[tool_call] {name} ({id})");
}

pub fn rate_limit(info: &RateLimitInfo) {
    eprintln!(
        "\n[rate_limit] remaining={:?} limit={:?} reset_at={:?}",
        info.remaining, info.limit, info.reset_at
    );
}

pub fn usage(usage: &Usage) {
    eprintln!(
        "\n[usage] input={} output={} cached_input={} reasoning={}",
        usage.input_tokens, usage.output_tokens, usage.cached_input_tokens, usage.reasoning_tokens
    );
}

pub fn completed(stop: &StopReason) {
    eprintln!("[done] stop={stop:?}");
}

pub fn info(msg: &str) {
    println!("{msg}");
}

pub fn warn(msg: &str) {
    eprintln!("warning: {msg}");
}

/// Prints a command failure to stderr. Redacted (defense in depth, INV-4): `msg` often embeds a
/// `ProviderError`/`AuthError` `Display`, and those can carry an upstream body excerpt that in the
/// worst case echoes back something secret-shaped (PATTERNS.md §13's secret-sentinel scenario).
pub fn error(msg: &str) {
    eprintln!("error: {}", xlightcli_auth::redact::redact(msg));
}

/// Reads a `y`/`yes` (case-insensitive) confirmation from stdin, defaulting to `false` on
/// anything else (including EOF) — used by `auth import`'s confirmation prompt.
pub fn confirm(prompt: &str) -> Result<bool, CliError> {
    print!("{prompt} [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|e| CliError::other(format!("failed to write prompt: {e}")))?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| CliError::other(format!("failed to read stdin: {e}")))?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Renders one `AgentEvent` from a `dev probe` stream. Returns `true` if it produced output
/// visible to the user (stdout or stderr) — used by the streaming loop to decide between
/// `CliError::Other` and `CliError::Partial` if the stream later errors out.
pub fn render_event(event: &AgentEvent) -> bool {
    match event {
        AgentEvent::TurnStarted { .. } => false,
        AgentEvent::TextDelta { text, .. } => {
            text_delta(text);
            true
        }
        AgentEvent::ReasoningDelta { text, .. } => {
            reasoning_delta(text);
            true
        }
        AgentEvent::ToolCallStarted { name, id, .. } => {
            tool_call_started(name, id.as_str());
            true
        }
        AgentEvent::Usage(u) => {
            usage(u);
            true
        }
        AgentEvent::RateLimit(r) => {
            rate_limit(r);
            true
        }
        AgentEvent::Completed { stop, usage: u, .. } => {
            completed(stop);
            usage(u);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::{Message, ModelId, Role, ToolCallId};

    use super::*;

    #[test]
    fn turn_started_produces_no_visible_output() {
        assert!(!render_event(&AgentEvent::TurnStarted {
            model: ModelId::new("m")
        }));
    }

    #[test]
    fn text_delta_counts_as_output() {
        assert!(render_event(&AgentEvent::TextDelta {
            index: 0,
            text: "hi".into()
        }));
    }

    #[test]
    fn tool_call_started_counts_as_output() {
        assert!(render_event(&AgentEvent::ToolCallStarted {
            index: 0,
            id: ToolCallId::new("call-1"),
            name: "read_file".into(),
        }));
    }

    #[test]
    fn completed_counts_as_output() {
        assert!(render_event(&AgentEvent::Completed {
            message: Message {
                role: Role::Assistant,
                content: vec![],
            },
            stop: StopReason::EndTurn,
            usage: Usage::default(),
        }));
    }
}
