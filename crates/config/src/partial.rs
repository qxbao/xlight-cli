// SPDX-License-Identifier: GPL-3.0-only

//! `PartialConfig` — every field `Option`, one instance per layer (PATTERNS.md §11). Deserialized
//! directly from a layer's TOML (or built by hand for the env/CLI-override layers); merged in
//! layer order by `crate::merge` into a single `PartialConfig`, then [`crate::merge::resolve`]d
//! into the full [`crate::schema::Config`].
//!
//! Mirrors `crate::schema` field-for-field so a missing key at any nesting level round-trips to
//! `None` (relying on serde's built-in "missing `Option<T>` field -> `None`" behavior) and a
//! present key always overrides a shallower layer's value.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use xlightcli_protocol::{ModelId, ProviderId, TransportId};

use crate::schema::{AuthStoreKind, PermissionMode};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialProviderDefaults {
    pub transport: Option<TransportId>,
    pub default_model: Option<ModelId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialExperimentalFlags {
    pub claude_subscription: Option<bool>,
    pub antigravity_subscription: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialPermissionsConfig {
    pub mode: Option<PermissionMode>,
    pub allow: Option<Vec<String>>,
    pub ask: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialAgentsConfig {
    pub max_active: Option<u32>,
    pub max_depth: Option<u32>,
    pub allow_provider_override: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialConcurrencyConfig {
    pub llm_requests: Option<u32>,
    pub shell_jobs: Option<u32>,
    pub browser_jobs: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialBudgetDefaults {
    pub max_turns: Option<u32>,
    pub max_output_tokens: Option<u64>,
    pub max_wall_clock: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialBudgetConfig {
    pub default: Option<PartialBudgetDefaults>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PartialContextConfig {
    pub compaction_threshold: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialToolsConfig {
    pub spool_head_bytes: Option<u64>,
    pub spool_tail_bytes: Option<u64>,
    pub shell_timeout_secs: Option<u64>,
    pub env_passthrough: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialAuthConfig {
    pub store: Option<AuthStoreKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialRulesConfig {
    pub sources: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialUiConfig {
    pub keybind: Option<BTreeMap<String, String>>,
    pub theme: Option<Option<String>>,
}

/// One config layer, fully optional (docs/PLAN.md §12.2). `#[serde(deny_unknown_fields)]` catches
/// typos in a user's `config.toml` at the top level; nested `BTreeMap` values (e.g. `provider.*`)
/// are still open-ended by key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialConfig {
    pub default_provider: Option<ProviderId>,
    pub provider: Option<BTreeMap<String, PartialProviderDefaults>>,
    pub experimental: Option<PartialExperimentalFlags>,
    pub disabled_transports: Option<Vec<TransportId>>,
    pub permissions: Option<PartialPermissionsConfig>,
    pub agents: Option<PartialAgentsConfig>,
    pub concurrency: Option<PartialConcurrencyConfig>,
    pub budget: Option<PartialBudgetConfig>,
    pub context: Option<PartialContextConfig>,
    pub tools: Option<PartialToolsConfig>,
    pub auth: Option<PartialAuthConfig>,
    pub rules: Option<PartialRulesConfig>,
    pub ui: Option<PartialUiConfig>,
}

impl PartialConfig {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parses one layer's TOML text. Callers attach the source path for error context
    /// (`crate::loader`).
    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}
