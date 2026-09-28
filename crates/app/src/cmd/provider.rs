// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli provider {list,info}` — read-only introspection over the registered
//! `ProviderRegistry` (no credential, no network).

use xlightcli_protocol::ProviderId;

use crate::cli::ProviderCommand as ProviderArgs;
use crate::cmd::error::CliError;
use crate::wiring::AppContext;

pub fn dispatch(ctx: &AppContext, command: &ProviderArgs) -> Result<(), CliError> {
    match command {
        ProviderArgs::List => list(ctx),
        ProviderArgs::Info { provider } => info(ctx, provider),
    }
}

fn list(ctx: &AppContext) -> Result<(), CliError> {
    if ctx.providers.is_empty() {
        crate::output::info("(no providers registered)");
        return Ok(());
    }
    for provider in ctx.providers.iter() {
        let transports = provider
            .transports()
            .iter()
            .map(|t| format!("{}({:?})", t.id(), t.stability()))
            .collect::<Vec<_>>()
            .join(", ");
        crate::output::info(&format!(
            "{:<10} {:<24} transports: {transports}",
            provider.id().to_string(),
            provider.display_name(),
        ));
    }
    Ok(())
}

fn info(ctx: &AppContext, provider_arg: &str) -> Result<(), CliError> {
    let provider_id = ProviderId::new(provider_arg);
    let provider = ctx.providers.get(&provider_id).ok_or_else(|| {
        CliError::invalid_input(format!(
            "unknown provider {provider_arg:?} (registered: {})",
            ctx.providers
                .ids()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    crate::output::info(&format!("{} — {}", provider.id(), provider.display_name()));
    crate::output::info("Transports:");
    for transport in provider.transports() {
        let caps = transport.capabilities();
        crate::output::info(&format!(
            "  - {} ({:?}, auth={:?}, protocol=v{}) reasoning={} images={} tool_calls={} \
             parallel_tool_calls={} web_search={:?} mcp={:?} session_resume={:?} usage={:?} \
             quota={:?} context_window={:?}",
            transport.id(),
            transport.stability(),
            transport.required_auth(),
            transport.protocol_version().0,
            caps.reasoning,
            caps.images,
            caps.tool_calls,
            caps.parallel_tool_calls,
            caps.web_search,
            caps.mcp,
            caps.session_resume,
            caps.usage,
            caps.quota,
            caps.context_window,
        ));
    }

    let commands = provider.features().commands();
    if commands.is_empty() {
        crate::output::info("Commands: (none)");
    } else {
        crate::output::info("Commands:");
        for cmd in commands {
            let requires = cmd
                .requires_transport
                .as_ref()
                .map(|t| format!(", requires_transport={t}"))
                .unwrap_or_default();
            crate::output::info(&format!(
                "  - {} (alias /{}, mode={:?}{requires}) — {}",
                cmd.id, cmd.alias, cmd.mode, cmd.summary,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_provider::ProviderRegistry;
    use xlightcli_provider::testing::MockProvider;

    use super::*;
    use crate::wiring::AppContext;

    fn ctx_with_one_mock_provider() -> AppContext {
        let mut providers = ProviderRegistry::new();
        providers.register(std::sync::Arc::new(MockProvider::scripted(
            "mock",
            "mock-transport",
            Vec::new(),
        )));
        AppContext {
            auth: xlightcli_auth::AuthBroker::new(),
            providers,
        }
    }

    #[test]
    fn list_does_not_error_with_providers_registered() {
        let ctx = ctx_with_one_mock_provider();
        assert!(dispatch(&ctx, &ProviderArgs::List).is_ok());
    }

    #[test]
    fn list_does_not_error_when_empty() {
        let ctx = AppContext {
            auth: xlightcli_auth::AuthBroker::new(),
            providers: ProviderRegistry::new(),
        };
        assert!(dispatch(&ctx, &ProviderArgs::List).is_ok());
    }

    #[test]
    fn info_on_registered_provider_succeeds() {
        let ctx = ctx_with_one_mock_provider();
        assert!(
            dispatch(
                &ctx,
                &ProviderArgs::Info {
                    provider: "mock".to_string()
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn info_on_unknown_provider_is_invalid_input() {
        let ctx = ctx_with_one_mock_provider();
        let err = dispatch(
            &ctx,
            &ProviderArgs::Info {
                provider: "nope".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(err.exit_code_number(), 2);
    }
}
