// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-config` — full Phase 1 config schema, layered loading, and workspace trust
//! (CODEBASE.md §2, docs/PLAN.md §12, PATTERNS.md §11).
//!
//! **Status (Phase 1 Wave A — contracts):** the schema (`schema`), per-layer partial types
//! (`partial`), the deterministic merge/resolve algorithm (`merge`), origin tracking (`origin`),
//! the env-var layer (`env`), workspace trust (`trust`), and `ConfigLoader` (`loader`) are all
//! real, tested code — not stubs. What's still Phase 1 Wave B scope: wiring `ConfigLoader` into
//! `app`, `core.config`/`core.permissions` command implementations, and richer per-leaf-key
//! `Origin` tracking if `config show --origin` needs it (see `origin` module doc for the current
//! section-level scope decision).
//!
//! This crate depends only on `xlightcli-protocol` (dependency rules, CODEBASE.md §3).

pub mod env;
pub mod error;
pub mod flags;
pub mod loader;
pub mod merge;
pub mod origin;
pub mod partial;
pub mod paths;
pub mod schema;
pub mod trust;

pub use error::ConfigError;
pub use flags::ExperimentalFlags;
pub use loader::{ConfigLoader, LoadedConfig};
pub use origin::{Origin, OriginMap};
pub use partial::PartialConfig;
pub use schema::{
    AgentsConfig, AuthConfig, AuthStoreKind, BudgetConfig, BudgetDefaults, ConcurrencyConfig,
    Config, ContextConfig, ExecutionMode, PermissionMode, PermissionRule, PermissionsConfig,
    ProviderDefaults, RuleEffect, RulesConfig, ToolsConfig, UiConfig,
};
pub use trust::TrustStore;
