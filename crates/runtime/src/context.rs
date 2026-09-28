// SPDX-License-Identifier: GPL-3.0-only

//! `ContextManager` (docs/PLAN.md §9.2): rules + history + tools -> `TurnRequest`, token
//! estimation, compaction trigger.
//!
//! **Status (Phase 1 Wave A):** [`estimate_tokens`] and [`ContextManager::should_compact`] are
//! real (pure, easily tested) logic. [`ContextManager::build_turn_request`] — the part that
//! actually reads rule files, pages in history from `Storage`, and assembles a
//! `xlightcli_protocol::TurnRequest` — is a Wave B stub.

use xlightcli_protocol::TurnRequest;

use crate::error::RuntimeError;
use crate::session::Session;

/// Token-count heuristic: characters / 4 (docs/PLAN.md §9.2 "a chars/4 heuristic calibrated by
/// provider usage is acceptable"). Deliberately not a real tokenizer (`tiktoken-rs` or similar) —
/// avoids a heavy/model-specific dependency; `ContextManager` is expected to recalibrate this
/// against a transport's actual reported `Usage` once turns start flowing (Wave B).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Rules + history + tools -> `TurnRequest`, plus compaction triggering (docs/PLAN.md §9.2).
#[derive(Debug, Clone)]
pub struct ContextManager {
    compaction_threshold: f32,
}

impl ContextManager {
    pub fn new(compaction_threshold: f32) -> Self {
        Self {
            compaction_threshold,
        }
    }

    pub fn from_config(cfg: &xlightcli_config::ContextConfig) -> Self {
        Self::new(cfg.compaction_threshold)
    }

    /// `true` once `used_tokens` reaches the configured fraction of `context_window`
    /// (docs/PLAN.md §9.2, default 80%). Always `false` for an unknown (`0`) context window
    /// rather than dividing by zero.
    pub fn should_compact(&self, used_tokens: u64, context_window: u64) -> bool {
        if context_window == 0 {
            return false;
        }
        (used_tokens as f32 / context_window as f32) >= self.compaction_threshold
    }

    /// Builds a `TurnRequest` for `session` from project rules, paged history, and the active
    /// tool registry (docs/PLAN.md §9.1 `context_manager.build`). Wave B.
    pub async fn build_turn_request(
        &self,
        _storage: &xlightcli_storage::Storage,
        _tools: &xlightcli_tools::ToolRegistry,
        _session: &Session,
    ) -> Result<TurnRequest, RuntimeError> {
        Err(RuntimeError::NotImplemented(
            "ContextManager::build_turn_request",
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn estimate_tokens_rounds_up() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn should_compact_respects_threshold() {
        let manager = ContextManager::new(0.8);
        assert!(!manager.should_compact(79, 100));
        assert!(manager.should_compact(80, 100));
    }

    #[test]
    fn should_compact_is_false_for_unknown_context_window() {
        let manager = ContextManager::new(0.8);
        assert!(!manager.should_compact(1_000_000, 0));
    }
}
