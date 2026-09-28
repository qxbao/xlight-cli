// SPDX-License-Identifier: GPL-3.0-only

//! `ContextManager` (docs/PLAN.md §9.2): rules + history + tools -> `TurnRequest`, token
//! estimation, compaction trigger.
//!
//! **Status (Phase 1 Wave B):** [`estimate_tokens`] and [`ContextManager::should_compact`] are
//! the same pure logic from Wave A. [`ContextManager::build_turn_request`] is now real: it reads
//! project rule files (`[rules].sources`, docs/PLAN.md §9.2), pages the most recent messages in
//! from `Storage`, and asks the `ToolRegistry` for its `ToolDefinition`s.
//!
//! **Scope decisions (Wave B, documented):**
//! - Rule discovery reads from the process's current working directory rather than the session's
//!   `WorkspaceRecord.root` — `Storage`'s public API doesn't yet expose a `get_workspace` accessor
//!   (only `create_workspace`), and extending it is outside this crate's Wave B scope (`runtime`
//!   only). [`ContextManager::with_rule_root`] lets a caller (or a test) override this explicitly;
//!   a future wave can wire the real workspace root through once storage exposes it.
//! - History paging loads the most recent [`HISTORY_PAGE_LIMIT`] messages rather than the whole
//!   session (PATTERNS.md §15 "loading the whole old session into `Vec<Message>` on resume" is the
//!   anti-pattern this avoids) — older turns are only reachable through [`Self::compact_messages`]
//!   once a session crosses the compaction threshold.
//! - Compaction (`Self::compact_messages`) trims older messages into a single deterministic
//!   placeholder `Message` — not a real LLM-summarized turn, and not persisted into a `summaries`
//!   row (`Storage` has no CRUD for that table yet, docs/PLAN.md §11.2's `summaries` table has no
//!   public write path in Wave A). This still exercises the trigger/threshold logic end to end;
//!   real summarization + persistence is a follow-up.

use std::path::{Path, PathBuf};

use xlightcli_protocol::{ContentBlock, Message, Role, SessionId, TurnRequest};
use xlightcli_protocol::{ProviderOptions, SystemPrompt};

use crate::error::RuntimeError;
use crate::session::Session;

/// Token-count heuristic: characters / 4 (docs/PLAN.md §9.2 "a chars/4 heuristic calibrated by
/// provider usage is acceptable"). Deliberately not a real tokenizer (`tiktoken-rs` or similar) —
/// avoids a heavy/model-specific dependency; `ContextManager` is expected to recalibrate this
/// against a transport's actual reported `Usage` once turns start flowing (Wave B).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Token estimate for one canonical `Message` (sums every text-bearing block; tool
/// input/output/opaque blobs are counted via their JSON text so a large tool result still nudges
/// the estimate even though core never inspects its meaning).
pub fn estimate_message_tokens(message: &Message) -> u64 {
    message
        .content
        .iter()
        .map(|block| {
            estimate_tokens(&match block {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::Reasoning { text, .. } => text.clone().unwrap_or_default(),
                ContentBlock::ToolUse { input, .. } => input.to_string(),
                ContentBlock::ToolResult { content, .. } => content
                    .iter()
                    .map(|part| match part {
                        xlightcli_protocol::ToolResultPart::Text { text } => text.clone(),
                        xlightcli_protocol::ToolResultPart::Image { .. } => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                ContentBlock::Image { .. } => String::new(),
            })
        })
        .sum()
}

/// How many of the most recent messages [`Storage::load_messages`] pages in per
/// `build_turn_request` call (see the module doc's scope decision).
const HISTORY_PAGE_LIMIT: u32 = 200;

/// How many of the most recent messages [`ContextManager::compact_messages`] keeps intact when
/// compaction triggers; everything older is folded into one placeholder message.
const KEEP_RECENT_MESSAGES: usize = 8;

/// Base identity text every turn's system prompt starts with, ahead of any project rules.
const BASE_SYSTEM_PROMPT: &str = "You are xlightcli, an autonomous coding-agent terminal. Use the \
available tools to inspect and modify the workspace on the user's behalf; always check \
permissions before a side effect and prefer the smallest change that satisfies the request.";

/// Rules + history + tools -> `TurnRequest`, plus compaction triggering (docs/PLAN.md §9.2).
#[derive(Debug, Clone)]
pub struct ContextManager {
    compaction_threshold: f32,
    rule_sources: Vec<String>,
    /// `None` means "discover rules relative to the process's current working directory" (see the
    /// module doc's scope decision); `Some(root)` overrides that, e.g. for tests.
    rule_root: Option<PathBuf>,
}

impl ContextManager {
    pub fn new(compaction_threshold: f32) -> Self {
        Self {
            compaction_threshold,
            rule_sources: xlightcli_config::RulesConfig::default().sources,
            rule_root: None,
        }
    }

    pub fn from_config(cfg: &xlightcli_config::ContextConfig) -> Self {
        Self::new(cfg.compaction_threshold)
    }

    /// [Wave B addition] builds from the full, resolved `Config` so rule discovery honors
    /// `[rules].sources` instead of always falling back to the hardcoded default list.
    pub fn from_full_config(cfg: &xlightcli_config::Config) -> Self {
        Self {
            compaction_threshold: cfg.context.compaction_threshold,
            rule_sources: cfg.rules.sources.clone(),
            rule_root: None,
        }
    }

    /// [Wave B addition] overrides where rule files are discovered from (see the module doc's
    /// scope decision on `Storage` not yet exposing a workspace-root accessor).
    pub fn with_rule_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.rule_root = Some(root.into());
        self
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

    fn rule_root(&self) -> PathBuf {
        self.rule_root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// Resolves `[rules].sources` patterns (docs/PLAN.md §9.2: `AGENTS.md`, `.agents/rules/*.md`,
    /// ...) against `root`, in source order: a plain filename must exist as a file at `root`; a
    /// `<dir>/<glob>` pattern lists `root/<dir>` and keeps entries whose file name matches the
    /// glob (sorted, for deterministic ordering), non-recursively.
    fn discover_rule_files(root: &Path, sources: &[String]) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for pattern in sources {
            match pattern.rsplit_once('/') {
                Some((dir, glob_part)) if glob_part.contains('*') => {
                    let Ok(matcher) = globset::Glob::new(glob_part) else {
                        continue;
                    };
                    let matcher = matcher.compile_matcher();
                    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
                        continue;
                    };
                    let mut matched: Vec<PathBuf> = entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .filter(|path| {
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| matcher.is_match(name))
                        })
                        .collect();
                    matched.sort();
                    found.extend(matched);
                }
                Some((dir, name)) => {
                    let candidate = root.join(dir).join(name);
                    if candidate.is_file() {
                        found.push(candidate);
                    }
                }
                None => {
                    let candidate = root.join(pattern);
                    if candidate.is_file() {
                        found.push(candidate);
                    }
                }
            }
        }
        found
    }

    /// Assembles the system prompt: [`BASE_SYSTEM_PROMPT`] followed by the contents of every
    /// discovered rule file, each clearly labeled with its source path.
    async fn build_system_prompt(&self) -> String {
        let root = self.rule_root();
        let mut sections = vec![BASE_SYSTEM_PROMPT.to_string()];
        for path in Self::discover_rule_files(&root, &self.rule_sources) {
            if let Ok(contents) = tokio::fs::read_to_string(&path).await {
                sections.push(format!(
                    "--- Project rules: {} ---\n{}",
                    path.display(),
                    contents.trim_end()
                ));
            }
        }
        sections.join("\n\n")
    }

    /// Builds a `TurnRequest` for `session` from project rules, paged history, and the active
    /// tool registry (docs/PLAN.md §9.1 `context_manager.build`).
    pub async fn build_turn_request(
        &self,
        storage: &xlightcli_storage::Storage,
        tools: &xlightcli_tools::ToolRegistry,
        session: &Session,
    ) -> Result<TurnRequest, RuntimeError> {
        let system_text = self.build_system_prompt().await;

        let history = storage
            .load_messages(
                session.id,
                xlightcli_storage::MessagePage {
                    before_id: None,
                    limit: HISTORY_PAGE_LIMIT,
                },
            )
            .await?;
        let messages: Vec<Message> = history
            .into_iter()
            .map(|record| Message {
                role: record.role,
                content: record.content,
            })
            .collect();

        Ok(TurnRequest {
            model: session.model.clone(),
            system: SystemPrompt::new(system_text),
            messages,
            tools: tools.definitions(),
            reasoning: None,
            max_output_tokens: None,
            provider_options: ProviderOptions::new(),
        })
    }

    /// Total token estimate for a would-be request (system prompt + every message) — what the
    /// agent loop compares against a transport's `context_window` via [`Self::should_compact`].
    pub fn estimate_request_tokens(&self, request: &TurnRequest) -> u64 {
        estimate_tokens(&request.system.text)
            + request
                .messages
                .iter()
                .map(estimate_message_tokens)
                .sum::<u64>()
    }

    /// Compacts `messages` in place for one outgoing request: keeps the most recent
    /// [`KEEP_RECENT_MESSAGES`], replaces everything older with a single synthetic placeholder
    /// message (see the module doc's scope decision — this is deterministic, not LLM-summarized,
    /// and not persisted).
    pub fn compact_messages(&self, messages: Vec<Message>, session_id: SessionId) -> Vec<Message> {
        if messages.len() <= KEEP_RECENT_MESSAGES {
            return messages;
        }
        let split_at = messages.len() - KEEP_RECENT_MESSAGES;
        let (older, recent) = messages.split_at(split_at);
        let placeholder = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!(
                    "[xlightcli: {} earlier message(s) in session {session_id} were compacted to \
                     stay under the context window. A deterministic placeholder replaces them — \
                     Wave B does not yet run a real summarization turn or persist a `summaries` \
                     row.]",
                    older.len()
                ),
            }],
        };
        let mut out = Vec::with_capacity(recent.len() + 1);
        out.push(placeholder);
        out.extend_from_slice(recent);
        out
    }
}

#[cfg(test)]
mod build_turn_request_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_protocol::{ModelId, ProviderId, TransportId};

    use super::*;

    async fn test_storage() -> (xlightcli_storage::Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = xlightcli_storage::Storage::open(dir.path().join("test.db"))
            .await
            .unwrap();
        (storage, dir)
    }

    async fn test_session(storage: &xlightcli_storage::Storage) -> Session {
        let workspace = storage
            .create_workspace(PathBuf::from("/repo"), None)
            .await
            .unwrap();
        let session_id = storage
            .create_session(
                workspace,
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                None,
            )
            .await
            .unwrap();
        let record = storage.get_session(session_id).await.unwrap().unwrap();
        Session::from_record(record)
    }

    #[tokio::test]
    async fn build_turn_request_includes_base_prompt_and_history() {
        let (storage, _dir) = test_storage().await;
        let session = test_session(&storage).await;
        let agent = storage
            .register_agent(session.id, None, serde_json::json!({}))
            .await
            .unwrap();
        storage
            .append_message(
                session.id,
                agent,
                1,
                Role::User,
                vec![ContentBlock::Text {
                    text: "hello there".into(),
                }],
            )
            .await
            .unwrap();

        let manager = ContextManager::new(0.8).with_rule_root(std::env::temp_dir().join(format!(
            "xlightcli-context-test-empty-{}",
            uuid::Uuid::new_v4()
        )));
        let tools = xlightcli_tools::ToolRegistry::new();
        let request = manager
            .build_turn_request(&storage, &tools, &session)
            .await
            .unwrap();

        assert!(request.system.text.contains("xlightcli"));
        assert_eq!(request.messages.len(), 1);
        assert!(matches!(
            &request.messages[0].content[0],
            ContentBlock::Text { text } if text == "hello there"
        ));
    }

    #[tokio::test]
    async fn build_turn_request_reads_matching_rule_files() {
        let (storage, _dir) = test_storage().await;
        let session = test_session(&storage).await;

        let rule_root = tempfile::tempdir().unwrap();
        std::fs::write(rule_root.path().join("AGENTS.md"), "Be concise.").unwrap();
        std::fs::create_dir_all(rule_root.path().join(".agents/rules")).unwrap();
        std::fs::write(
            rule_root.path().join(".agents/rules/style.md"),
            "Prefer early returns.",
        )
        .unwrap();

        let manager = ContextManager::new(0.8).with_rule_root(rule_root.path());
        let tools = xlightcli_tools::ToolRegistry::new();
        let request = manager
            .build_turn_request(&storage, &tools, &session)
            .await
            .unwrap();

        assert!(request.system.text.contains("Be concise."));
        assert!(request.system.text.contains("Prefer early returns."));
    }

    #[tokio::test]
    async fn build_turn_request_includes_tool_definitions() {
        let (storage, _dir) = test_storage().await;
        let session = test_session(&storage).await;
        let manager = ContextManager::new(0.8).with_rule_root(std::env::temp_dir().join(format!(
            "xlightcli-context-test-empty-{}",
            uuid::Uuid::new_v4()
        )));
        let tools = xlightcli_tools::ToolRegistry::with_builtins();
        let request = manager
            .build_turn_request(&storage, &tools, &session)
            .await
            .unwrap();
        assert_eq!(request.tools.len(), tools.len());
    }

    #[test]
    fn compact_messages_keeps_recent_and_folds_the_rest() {
        let manager = ContextManager::new(0.8);
        let messages: Vec<Message> = (0..20)
            .map(|i| Message::user_text(format!("message {i}")))
            .collect();
        let compacted = manager.compact_messages(messages, SessionId::new());
        assert_eq!(compacted.len(), KEEP_RECENT_MESSAGES + 1);
        assert!(matches!(
            &compacted[0].content[0],
            ContentBlock::Text { text } if text.contains("compacted")
        ));
        assert!(matches!(
            &compacted.last().unwrap().content[0],
            ContentBlock::Text { text } if text == "message 19"
        ));
    }

    #[test]
    fn compact_messages_is_a_no_op_under_the_keep_threshold() {
        let manager = ContextManager::new(0.8);
        let messages: Vec<Message> = (0..3)
            .map(|i| Message::user_text(format!("message {i}")))
            .collect();
        let compacted = manager.compact_messages(messages.clone(), SessionId::new());
        assert_eq!(compacted, messages);
    }

    #[test]
    fn estimate_request_tokens_sums_system_and_messages() {
        let manager = ContextManager::new(0.8);
        let request = TurnRequest::simple(ModelId::new("gpt-5"), "abcd");
        let estimate = manager.estimate_request_tokens(&request);
        // "abcd" -> 1 token; empty default system prompt -> 0.
        assert_eq!(estimate, 1);
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
