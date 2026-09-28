// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic layer merge (PATTERNS.md §11, docs/PLAN.md §12.2): tables deep-merge field by
//! field, scalars are overwritten by the later layer, arrays are **replaced** (never
//! concatenated). [`resolve`] then fills in every remaining `None` with the hardcoded default from
//! `crate::schema`.

use std::collections::BTreeMap;

use crate::partial::{
    PartialAgentsConfig, PartialAuthConfig, PartialBudgetConfig, PartialBudgetDefaults,
    PartialConcurrencyConfig, PartialConfig, PartialContextConfig, PartialExperimentalFlags,
    PartialPermissionsConfig, PartialProviderDefaults, PartialRulesConfig, PartialToolsConfig,
    PartialUiConfig,
};
use crate::schema::{
    AgentsConfig, AuthConfig, BudgetConfig, BudgetDefaults, ConcurrencyConfig, Config,
    ContextConfig, ExperimentalFlags, PermissionsConfig, ProviderDefaults, RulesConfig,
    ToolsConfig, UiConfig,
};

/// `over` wins on scalars; `Some`/`Some` on a nested table recurses; a missing side just takes
/// the other side as-is (including `None`).
fn merge_opt<T>(base: Option<T>, over: Option<T>, f: impl FnOnce(T, T) -> T) -> Option<T> {
    match (base, over) {
        (Some(b), Some(o)) => Some(f(b, o)),
        (Some(b), None) => Some(b),
        (None, over) => over,
    }
}

/// Array fields are **replaced**, not concatenated (PATTERNS.md §11): the later layer's `Some`
/// simply wins outright, with no recursion into element content.
fn merge_replace<T>(base: Option<T>, over: Option<T>) -> Option<T> {
    over.or(base)
}

fn merge_provider_map(
    base: Option<BTreeMap<String, PartialProviderDefaults>>,
    over: Option<BTreeMap<String, PartialProviderDefaults>>,
) -> Option<BTreeMap<String, PartialProviderDefaults>> {
    match (base, over) {
        (None, None) => None,
        (Some(b), None) => Some(b),
        (None, Some(o)) => Some(o),
        (Some(mut merged), Some(over)) => {
            for (id, over_defaults) in over {
                match merged.get_mut(&id) {
                    Some(existing) => {
                        if over_defaults.transport.is_some() {
                            existing.transport = over_defaults.transport;
                        }
                        if over_defaults.default_model.is_some() {
                            existing.default_model = over_defaults.default_model;
                        }
                    }
                    None => {
                        merged.insert(id, over_defaults);
                    }
                }
            }
            Some(merged)
        }
    }
}

fn merge_experimental(
    base: PartialExperimentalFlags,
    over: PartialExperimentalFlags,
) -> PartialExperimentalFlags {
    PartialExperimentalFlags {
        claude_subscription: over.claude_subscription.or(base.claude_subscription),
        antigravity_subscription: over
            .antigravity_subscription
            .or(base.antigravity_subscription),
    }
}

fn merge_permissions(
    base: PartialPermissionsConfig,
    over: PartialPermissionsConfig,
) -> PartialPermissionsConfig {
    PartialPermissionsConfig {
        mode: over.mode.or(base.mode),
        allow: merge_replace(base.allow, over.allow),
        ask: merge_replace(base.ask, over.ask),
        deny: merge_replace(base.deny, over.deny),
    }
}

fn merge_agents(base: PartialAgentsConfig, over: PartialAgentsConfig) -> PartialAgentsConfig {
    PartialAgentsConfig {
        max_active: over.max_active.or(base.max_active),
        max_depth: over.max_depth.or(base.max_depth),
        allow_provider_override: over
            .allow_provider_override
            .or(base.allow_provider_override),
    }
}

fn merge_concurrency(
    base: PartialConcurrencyConfig,
    over: PartialConcurrencyConfig,
) -> PartialConcurrencyConfig {
    PartialConcurrencyConfig {
        llm_requests: over.llm_requests.or(base.llm_requests),
        shell_jobs: over.shell_jobs.or(base.shell_jobs),
        browser_jobs: over.browser_jobs.or(base.browser_jobs),
    }
}

fn merge_budget_defaults(
    base: PartialBudgetDefaults,
    over: PartialBudgetDefaults,
) -> PartialBudgetDefaults {
    PartialBudgetDefaults {
        max_turns: over.max_turns.or(base.max_turns),
        max_output_tokens: over.max_output_tokens.or(base.max_output_tokens),
        max_wall_clock: over.max_wall_clock.or(base.max_wall_clock),
    }
}

fn merge_budget(base: PartialBudgetConfig, over: PartialBudgetConfig) -> PartialBudgetConfig {
    PartialBudgetConfig {
        default: merge_opt(base.default, over.default, merge_budget_defaults),
    }
}

fn merge_context(base: PartialContextConfig, over: PartialContextConfig) -> PartialContextConfig {
    PartialContextConfig {
        compaction_threshold: over.compaction_threshold.or(base.compaction_threshold),
    }
}

fn merge_tools(base: PartialToolsConfig, over: PartialToolsConfig) -> PartialToolsConfig {
    PartialToolsConfig {
        spool_head_bytes: over.spool_head_bytes.or(base.spool_head_bytes),
        spool_tail_bytes: over.spool_tail_bytes.or(base.spool_tail_bytes),
        shell_timeout_secs: over.shell_timeout_secs.or(base.shell_timeout_secs),
        env_passthrough: merge_replace(base.env_passthrough, over.env_passthrough),
    }
}

fn merge_auth(base: PartialAuthConfig, over: PartialAuthConfig) -> PartialAuthConfig {
    PartialAuthConfig {
        store: over.store.or(base.store),
    }
}

fn merge_rules(base: PartialRulesConfig, over: PartialRulesConfig) -> PartialRulesConfig {
    PartialRulesConfig {
        sources: merge_replace(base.sources, over.sources),
    }
}

fn merge_ui(base: PartialUiConfig, over: PartialUiConfig) -> PartialUiConfig {
    PartialUiConfig {
        // `keybind` is itself a table (action name -> chord): deep-merge per key rather than
        // replace-the-whole-map, so a project layer can add one keybind without repeating every
        // built-in default.
        keybind: match (base.keybind, over.keybind) {
            (None, None) => None,
            (Some(b), None) => Some(b),
            (None, Some(o)) => Some(o),
            (Some(mut b), Some(o)) => {
                b.extend(o);
                Some(b)
            }
        },
        theme: over.theme.or(base.theme),
    }
}

/// Merges `over` on top of `base` (`over` is the *later*, higher-priority layer). Pure, no IO.
pub fn merge(base: PartialConfig, over: PartialConfig) -> PartialConfig {
    PartialConfig {
        default_provider: over.default_provider.or(base.default_provider),
        provider: merge_provider_map(base.provider, over.provider),
        experimental: merge_opt(base.experimental, over.experimental, merge_experimental),
        disabled_transports: merge_replace(base.disabled_transports, over.disabled_transports),
        permissions: merge_opt(base.permissions, over.permissions, merge_permissions),
        agents: merge_opt(base.agents, over.agents, merge_agents),
        concurrency: merge_opt(base.concurrency, over.concurrency, merge_concurrency),
        budget: merge_opt(base.budget, over.budget, merge_budget),
        context: merge_opt(base.context, over.context, merge_context),
        tools: merge_opt(base.tools, over.tools, merge_tools),
        auth: merge_opt(base.auth, over.auth, merge_auth),
        rules: merge_opt(base.rules, over.rules, merge_rules),
        ui: merge_opt(base.ui, over.ui, merge_ui),
    }
}

/// Fills in every remaining `None` with the hardcoded default from `crate::schema`, producing the
/// fully-resolved [`Config`].
pub fn resolve(partial: PartialConfig) -> Config {
    let provider = partial
        .provider
        .unwrap_or_default()
        .into_iter()
        .map(|(id, p)| {
            (
                id,
                ProviderDefaults {
                    transport: p.transport,
                    default_model: p.default_model,
                },
            )
        })
        .collect();

    let experimental = partial
        .experimental
        .map(|e| ExperimentalFlags {
            claude_subscription: e.claude_subscription.unwrap_or_default(),
            antigravity_subscription: e.antigravity_subscription.unwrap_or_default(),
        })
        .unwrap_or_default();

    let permissions = partial
        .permissions
        .map(|p| PermissionsConfig {
            mode: p.mode.unwrap_or_default(),
            allow: p.allow.unwrap_or_default(),
            ask: p.ask.unwrap_or_default(),
            deny: p.deny.unwrap_or_default(),
        })
        .unwrap_or_default();

    let agents = partial
        .agents
        .map(|a| {
            let defaults = AgentsConfig::default();
            AgentsConfig {
                max_active: a.max_active.unwrap_or(defaults.max_active),
                max_depth: a.max_depth.unwrap_or(defaults.max_depth),
                allow_provider_override: a
                    .allow_provider_override
                    .unwrap_or(defaults.allow_provider_override),
            }
        })
        .unwrap_or_default();

    let concurrency = partial
        .concurrency
        .map(|c| {
            let defaults = ConcurrencyConfig::default();
            ConcurrencyConfig {
                llm_requests: c.llm_requests.unwrap_or(defaults.llm_requests),
                shell_jobs: c.shell_jobs.unwrap_or(defaults.shell_jobs),
                browser_jobs: c.browser_jobs.unwrap_or(defaults.browser_jobs),
            }
        })
        .unwrap_or_default();

    let budget = BudgetConfig {
        default: partial
            .budget
            .and_then(|b| b.default)
            .map(|d| {
                let defaults = BudgetDefaults::default();
                BudgetDefaults {
                    max_turns: d.max_turns.unwrap_or(defaults.max_turns),
                    max_output_tokens: d.max_output_tokens.unwrap_or(defaults.max_output_tokens),
                    max_wall_clock: d.max_wall_clock.unwrap_or(defaults.max_wall_clock),
                }
            })
            .unwrap_or_default(),
    };

    let context = partial
        .context
        .map(|c| {
            let defaults = ContextConfig::default();
            ContextConfig {
                compaction_threshold: c
                    .compaction_threshold
                    .unwrap_or(defaults.compaction_threshold),
            }
        })
        .unwrap_or_default();

    let tools = partial
        .tools
        .map(|t| {
            let defaults = ToolsConfig::default();
            ToolsConfig {
                spool_head_bytes: t.spool_head_bytes.unwrap_or(defaults.spool_head_bytes),
                spool_tail_bytes: t.spool_tail_bytes.unwrap_or(defaults.spool_tail_bytes),
                shell_timeout_secs: t.shell_timeout_secs.unwrap_or(defaults.shell_timeout_secs),
                env_passthrough: t.env_passthrough.unwrap_or_default(),
            }
        })
        .unwrap_or_default();

    let auth = partial
        .auth
        .map(|a| AuthConfig {
            store: a.store.unwrap_or_default(),
        })
        .unwrap_or_default();

    let rules = partial
        .rules
        .map(|r| RulesConfig {
            sources: r.sources.unwrap_or_else(|| RulesConfig::default().sources),
        })
        .unwrap_or_default();

    let ui = partial
        .ui
        .map(|u| UiConfig {
            keybind: u.keybind.unwrap_or_default(),
            theme: u.theme.flatten(),
        })
        .unwrap_or_default();

    Config {
        default_provider: partial.default_provider,
        provider,
        experimental,
        disabled_transports: partial.disabled_transports.unwrap_or_default(),
        permissions,
        agents,
        concurrency,
        budget,
        context,
        tools,
        auth,
        rules,
        ui,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use xlightcli_protocol::{ModelId, ProviderId, TransportId};

    use super::*;
    use crate::partial::{PartialPermissionsConfig, PartialToolsConfig};

    #[test]
    fn scalar_override_wins() {
        let base = PartialConfig {
            tools: Some(PartialToolsConfig {
                shell_timeout_secs: Some(60),
                ..Default::default()
            }),
            ..Default::default()
        };
        let over = PartialConfig {
            tools: Some(PartialToolsConfig {
                shell_timeout_secs: Some(300),
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = merge(base, over);
        assert_eq!(merged.tools.unwrap().shell_timeout_secs, Some(300));
    }

    #[test]
    fn array_field_is_replaced_not_concatenated() {
        let base = PartialConfig {
            permissions: Some(PartialPermissionsConfig {
                allow: Some(vec!["read_file(*)".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let over = PartialConfig {
            permissions: Some(PartialPermissionsConfig {
                allow: Some(vec!["git_diff(*)".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = merge(base, over);
        assert_eq!(
            merged.permissions.unwrap().allow,
            Some(vec!["git_diff(*)".to_string()])
        );
    }

    #[test]
    fn missing_layer_keeps_base_untouched() {
        let base = PartialConfig {
            default_provider: Some(ProviderId::new("codex")),
            ..Default::default()
        };
        let merged = merge(base.clone(), PartialConfig::empty());
        assert_eq!(merged, base);
    }

    #[test]
    fn resolve_fills_hardcoded_defaults() {
        let resolved = resolve(PartialConfig::empty());
        assert_eq!(
            resolved.permissions.mode,
            crate::schema::PermissionMode::Ask
        );
        assert_eq!(resolved.tools.spool_head_bytes, 8 * 1024);
        assert_eq!(resolved.agents.max_active, 8);
        assert_eq!(resolved.budget.default.max_turns, 50);
    }

    #[test]
    fn provider_table_deep_merges_by_key() {
        let base = PartialConfig {
            provider: Some(BTreeMap::from([(
                "codex".to_string(),
                PartialProviderDefaults {
                    transport: Some(TransportId::new("chatgpt")),
                    default_model: None,
                },
            )])),
            ..Default::default()
        };
        let over = PartialConfig {
            provider: Some(BTreeMap::from([(
                "codex".to_string(),
                PartialProviderDefaults {
                    transport: None,
                    default_model: Some(ModelId::new("gpt-5")),
                },
            )])),
            ..Default::default()
        };
        let merged = merge(base, over);
        let codex = merged.provider.unwrap().remove("codex").unwrap();
        assert_eq!(codex.transport, Some(TransportId::new("chatgpt")));
        assert_eq!(codex.default_model, Some(ModelId::new("gpt-5")));
    }
}
