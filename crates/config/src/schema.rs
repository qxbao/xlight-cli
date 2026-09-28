// SPDX-License-Identifier: GPL-3.0-only

//! Full Phase 1 configuration schema (docs/PLAN.md §12.4, §7.3, §9.2, §10.3, §11.2).
//!
//! [`Config`] is the fully-resolved, defaulted configuration the rest of the workspace consumes.
//! Every layer (defaults → global → project → project.local → env → CLI, PATTERNS.md §11)
//! deserializes into [`crate::partial::PartialConfig`] first; [`crate::loader::ConfigLoader`]
//! merges the layers (`crate::merge`) and calls [`crate::merge::resolve`] to fill in the defaults
//! below, producing this type.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use xlightcli_protocol::{ModelId, ProviderId, TransportId};

use crate::error::ConfigError;
pub use crate::flags::ExperimentalFlags;

/// `[provider.<id>]` (docs/PLAN.md §12.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDefaults {
    pub transport: Option<TransportId>,
    pub default_model: Option<ModelId>,
}

/// Execution mode (Shift+Tab, D-025): cycles `Default -> AcceptEdits -> Plan -> Default`. Session
/// state, not persisted config, but declared here (rather than in `tools`, which depends on this
/// crate) so `PermissionsConfig`/docs/commands.md §4 and the runtime can share one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionMode {
    #[default]
    Default,
    AcceptEdits,
    Plan,
}

impl ExecutionMode {
    /// Shift+Tab cycles `default -> accept-edits -> plan -> default` (docs/commands.md §4).
    pub fn next(self) -> Self {
        match self {
            Self::Default => Self::AcceptEdits,
            Self::AcceptEdits => Self::Plan,
            Self::Plan => Self::Default,
        }
    }
}

/// Permission preset (docs/PLAN.md §7.3, docs/commands.md §4). Ordered from most to least
/// restrictive; [`PermissionMode::rank`] gives the numeric order used by workspace-trust
/// filtering (`crate::trust`) to reject a project layer trying to raise the mode above `Ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    ReadOnly,
    Strict,
    #[default]
    Ask,
    AutoEdit,
    FullAuto,
}

impl PermissionMode {
    /// Higher = less restrictive. Used by `crate::trust` to enforce "a project layer must not
    /// raise the permission mode above `Ask`" (docs/PLAN.md §12.3).
    pub fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Strict => 1,
            Self::Ask => 2,
            Self::AutoEdit => 3,
            Self::FullAuto => 4,
        }
    }
}

impl std::str::FromStr for PermissionMode {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read-only" | "read_only" | "readonly" => Ok(Self::ReadOnly),
            "strict" => Ok(Self::Strict),
            "ask" => Ok(Self::Ask),
            "auto-edit" | "auto_edit" | "autoedit" => Ok(Self::AutoEdit),
            "full-auto" | "full_auto" | "fullauto" => Ok(Self::FullAuto),
            other => Err(ConfigError::InvalidRule(format!(
                "unknown permission mode {other:?}"
            ))),
        }
    }
}

/// Precedence when the same target matches more than one rule: **deny > ask > allow**
/// (docs/PLAN.md §7.3, docs/commands.md §4). `PermissionEngine` (`xlightcli-tools`) evaluates
/// rules parsed from [`PermissionsConfig`] in this order regardless of file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleEffect {
    Allow,
    Ask,
    Deny,
}

/// One resolved `action(target)` rule (docs/PLAN.md §7.3). `action` is a tool action id
/// (`read_file`, `write_file`, `command`, `read_url`, `mcp`, `unsandboxed`, docs/commands.md §4);
/// `target` is a glob, a command prefix, or `regex:<pattern>`; `*` matches anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub effect: RuleEffect,
    pub action: String,
    pub target: String,
}

impl PermissionRule {
    /// Parses the `action(target)` grammar. `target` may be empty (`action()`), but `action`
    /// must not be.
    pub fn parse(effect: RuleEffect, raw: &str) -> Result<Self, ConfigError> {
        let trimmed = raw.trim();
        let open = trimmed
            .find('(')
            .ok_or_else(|| ConfigError::InvalidRule(raw.to_string()))?;
        if !trimmed.ends_with(')') {
            return Err(ConfigError::InvalidRule(raw.to_string()));
        }
        let action = trimmed[..open].trim();
        let target = trimmed[open + 1..trimmed.len() - 1].trim();
        if action.is_empty() {
            return Err(ConfigError::InvalidRule(raw.to_string()));
        }
        Ok(Self {
            effect,
            action: action.to_string(),
            target: target.to_string(),
        })
    }

    /// Renders back to the `action(target)` textual form (round-trips with [`Self::parse`]).
    pub fn to_raw(&self) -> String {
        format!("{}({})", self.action, self.target)
    }
}

/// `[permissions]` (docs/PLAN.md §7.3). `allow`/`ask`/`deny` are lists of raw `action(target)`
/// strings — kept as flat string arrays (rather than a list of `{effect, action, target}` tables)
/// because that is the natural, terse TOML authoring shape; use [`Self::rules`] to get parsed
/// [`PermissionRule`]s for [`xlightcli_tools`]'s `PermissionEngine`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub mode: PermissionMode,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl PermissionsConfig {
    /// Parses every raw rule string, in deny-then-ask-then-allow order (matching evaluation
    /// precedence, docs/PLAN.md §7.3). A malformed rule is skipped rather than failing the whole
    /// config load — `ConfigLoader` surfaces parse failures separately via
    /// [`Self::rules_checked`] for callers (e.g. `core.config`/`core.permissions`) that want to
    /// report them to the user.
    pub fn rules(&self) -> Vec<PermissionRule> {
        self.rules_checked()
            .into_iter()
            .filter_map(Result::ok)
            .collect()
    }

    /// Same as [`Self::rules`] but keeps parse errors instead of discarding them.
    pub fn rules_checked(&self) -> Vec<Result<PermissionRule, ConfigError>> {
        self.deny
            .iter()
            .map(|raw| PermissionRule::parse(RuleEffect::Deny, raw))
            .chain(
                self.ask
                    .iter()
                    .map(|raw| PermissionRule::parse(RuleEffect::Ask, raw)),
            )
            .chain(
                self.allow
                    .iter()
                    .map(|raw| PermissionRule::parse(RuleEffect::Allow, raw)),
            )
            .collect()
    }
}

/// `[agents]` (docs/PLAN.md §10.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsConfig {
    #[serde(default = "default_max_active")]
    pub max_active: u32,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// A sub-agent may override the provider only when this is `true` (docs/PLAN.md §12.4).
    #[serde(default)]
    pub allow_provider_override: bool,
}

fn default_max_active() -> u32 {
    8
}
fn default_max_depth() -> u32 {
    4
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            max_active: default_max_active(),
            max_depth: default_max_depth(),
            allow_provider_override: false,
        }
    }
}

/// `[concurrency]` (docs/PLAN.md §10.3): one semaphore per resource type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrencyConfig {
    #[serde(default = "default_llm_requests")]
    pub llm_requests: u32,
    #[serde(default = "default_shell_jobs")]
    pub shell_jobs: u32,
    #[serde(default = "default_browser_jobs")]
    pub browser_jobs: u32,
}

fn default_llm_requests() -> u32 {
    4
}
fn default_shell_jobs() -> u32 {
    4
}
fn default_browser_jobs() -> u32 {
    1
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            llm_requests: default_llm_requests(),
            shell_jobs: default_shell_jobs(),
            browser_jobs: default_browser_jobs(),
        }
    }
}

/// `[budget.default]` (docs/PLAN.md §10.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetDefaults {
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u64,
    /// Minimal duration grammar (see [`ConfigError::InvalidDuration`]); parse with
    /// [`Self::max_wall_clock_duration`].
    #[serde(default = "default_max_wall_clock")]
    pub max_wall_clock: String,
}

fn default_max_turns() -> u32 {
    50
}
fn default_max_output_tokens() -> u64 {
    200_000
}
fn default_max_wall_clock() -> String {
    "30m".to_string()
}

impl Default for BudgetDefaults {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            max_output_tokens: default_max_output_tokens(),
            max_wall_clock: default_max_wall_clock(),
        }
    }
}

impl BudgetDefaults {
    /// Parses [`Self::max_wall_clock`]: an integer followed by `s`/`m`/`h`, or a bare integer
    /// (seconds). Deliberately not a full `humantime` grammar (Phase 1 scope decision, avoids a
    /// dependency for one field); extend here if a richer grammar is needed later.
    pub fn max_wall_clock_duration(&self) -> Result<std::time::Duration, ConfigError> {
        parse_duration(&self.max_wall_clock)
    }
}

fn parse_duration(s: &str) -> Result<std::time::Duration, ConfigError> {
    let s = s.trim();
    let err = || ConfigError::InvalidDuration(s.to_string());
    if let Some(digits) = s.strip_suffix('s') {
        return Ok(std::time::Duration::from_secs(
            digits.parse().map_err(|_| err())?,
        ));
    }
    if let Some(digits) = s.strip_suffix('m') {
        let mins: u64 = digits.parse().map_err(|_| err())?;
        return Ok(std::time::Duration::from_secs(mins * 60));
    }
    if let Some(digits) = s.strip_suffix('h') {
        let hours: u64 = digits.parse().map_err(|_| err())?;
        return Ok(std::time::Duration::from_secs(hours * 3600));
    }
    let secs: u64 = s.parse().map_err(|_| err())?;
    Ok(std::time::Duration::from_secs(secs))
}

/// `[budget]` — currently only the `default` sub-table; per-agent-profile overrides are a later
/// wave (docs/PLAN.md §10.3 only specifies `[budget.default]` for Phase 1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetConfig {
    #[serde(default)]
    pub default: BudgetDefaults,
}

/// `[context]` (docs/PLAN.md §9.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Fraction (0.0-1.0) of the model's context window at which `ContextManager` triggers
    /// compaction. Default 0.8 (80%, docs/PLAN.md §9.2).
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: f32,
}

fn default_compaction_threshold() -> f32 {
    0.8
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            compaction_threshold: default_compaction_threshold(),
        }
    }
}

/// `[tools]` (docs/PLAN.md §7.4, §7.5, PATTERNS.md §8/§9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// `OutputSpool` head buffer size (PATTERNS.md §9 example: 8 KiB).
    #[serde(default = "default_spool_head_bytes")]
    pub spool_head_bytes: u64,
    /// `OutputSpool` tail ring-buffer size (PATTERNS.md §9 example: 32 KiB).
    #[serde(default = "default_spool_tail_bytes")]
    pub spool_tail_bytes: u64,
    /// Default `shell` tool timeout, seconds (PATTERNS.md §8 `DEFAULT_SHELL_TIMEOUT`).
    #[serde(default = "default_shell_timeout_secs")]
    pub shell_timeout_secs: u64,
    /// Env vars explicitly allowed to pass through `EnvPolicy::Scrubbed` (docs/PLAN.md §7.4).
    /// Empty by default: there is no "inherit everything" API.
    #[serde(default)]
    pub env_passthrough: Vec<String>,
}

fn default_spool_head_bytes() -> u64 {
    8 * 1024
}
fn default_spool_tail_bytes() -> u64 {
    32 * 1024
}
fn default_shell_timeout_secs() -> u64 {
    120
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            spool_head_bytes: default_spool_head_bytes(),
            spool_tail_bytes: default_spool_tail_bytes(),
            shell_timeout_secs: default_shell_timeout_secs(),
            env_passthrough: Vec::new(),
        }
    }
}

/// Secret-store backend selector (docs/PLAN.md §6.1, D-019). A separate type from
/// `xlightcli_auth::StoreKind` because `config` must not depend on `auth` (CODEBASE.md §3); `app`
/// wiring converts one into the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthStoreKind {
    #[default]
    Keyring,
    File,
}

/// `[auth]` (docs/PLAN.md §6.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub store: AuthStoreKind,
}

/// `[rules]` (docs/PLAN.md §9.2): project-rule discovery sources, in priority order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesConfig {
    #[serde(default = "default_rule_sources")]
    pub sources: Vec<String>,
}

fn default_rule_sources() -> Vec<String> {
    vec![
        "AGENTS.md".to_string(),
        "AGENTS.override.md".to_string(),
        "CLAUDE.md".to_string(),
        "GEMINI.md".to_string(),
        ".agents/rules/*.md".to_string(),
        ".xlightcli/rules/*.md".to_string(),
    ]
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            sources: default_rule_sources(),
        }
    }
}

/// `[ui]` (docs/PLAN.md §12.4 `[keybind]`, §18.2): keybinding + theme placeholders. Real
/// keymap/theme types land with the `tui` implementation (Phase 1 Wave B); for now this is just
/// the persisted data shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiConfig {
    /// Action name -> key chord string (e.g. `"new_agent" -> "ctrl+n"`, docs/PLAN.md §12.4).
    #[serde(default)]
    pub keybind: BTreeMap<String, String>,
    #[serde(default)]
    pub theme: Option<String>,
}

/// Top-level, fully-resolved configuration (docs/PLAN.md §12.4). Produced by
/// [`crate::merge::resolve`] from a merged [`crate::partial::PartialConfig`]; never constructed
/// directly by application code except in tests / `Default::default()`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_provider: Option<ProviderId>,
    /// Per-provider transport/model defaults, keyed by provider id (`[provider.codex]`, ...).
    #[serde(default)]
    pub provider: BTreeMap<String, ProviderDefaults>,
    #[serde(default)]
    pub experimental: ExperimentalFlags,
    /// Kill switch (docs/PLAN.md §15): transport ids disabled regardless of opt-in state.
    #[serde(default)]
    pub disabled_transports: Vec<TransportId>,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub concurrency: ConcurrencyConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub rules: RulesConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

impl Config {
    /// `true` when `transport` is not present in `disabled_transports`.
    pub fn transport_allowed(&self, transport_id: &TransportId) -> bool {
        !self.disabled_transports.iter().any(|t| t == transport_id)
    }
}
