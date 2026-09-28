// SPDX-License-Identifier: GPL-3.0-only

//! `CliError` — maps every command failure to one of the four exit codes from docs/PLAN.md §18.3
//! / the Wave 2 brief: `0` ok, `1` error, `2` invalid input, `3` error after partial output.

use std::fmt;
use std::process::ExitCode;

#[derive(Debug)]
pub enum CliError {
    /// Bad argument / unknown provider-transport-etc: nothing was attempted.
    InvalidInput(String),
    /// Failed before producing any user-visible output.
    Other(String),
    /// Failed after already streaming some output (e.g. `dev probe` broke mid-stream).
    Partial(String),
}

impl CliError {
    /// Redacted at construction (not just wherever it's eventually printed/logged): messages
    /// routinely embed a `ProviderError`/`AuthError` `Display`, which in the worst case echoes
    /// back an upstream body that looks like a secret (INV-4 defense in depth, PATTERNS.md §13
    /// secret-sentinel scenario). Idempotent (`xlightcli_auth::redact::redact` is), so redacting
    /// an already-safe message is a harmless no-op.
    fn scrub(msg: impl Into<String>) -> String {
        xlightcli_auth::redact::redact(&msg.into())
    }

    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(Self::scrub(msg))
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(Self::scrub(msg))
    }

    pub fn partial(msg: impl Into<String>) -> Self {
        Self::Partial(Self::scrub(msg))
    }

    /// Numeric exit code (docs/PLAN.md §18.3). Split out from `exit_code()` because
    /// `std::process::ExitCode` doesn't implement `PartialEq`, so tests compare this instead.
    pub fn exit_code_number(&self) -> u8 {
        match self {
            Self::InvalidInput(_) => 2,
            Self::Other(_) => 1,
            Self::Partial(_) => 3,
        }
    }

    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from(self.exit_code_number())
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(m) | Self::Other(m) | Self::Partial(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for CliError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn exit_codes_match_docs_plan_18_3() {
        assert_eq!(CliError::invalid_input("x").exit_code_number(), 2);
        assert_eq!(CliError::other("x").exit_code_number(), 1);
        assert_eq!(CliError::partial("x").exit_code_number(), 3);
    }

    #[test]
    fn display_is_just_the_message() {
        assert_eq!(CliError::other("boom").to_string(), "boom");
    }
}
