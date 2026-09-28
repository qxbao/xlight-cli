// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli dev probe <provider> [--transport T] [--model M] "<prompt>"` — Phase 0 smoke test
//! entry point (docs/PLAN.md §19): `credential -> send prompt -> stream response`.
//!
//! Split into `probe` (the full CLI path: provider/transport lookup, `AuthBroker::credential`,
//! model resolution) and `stream_and_render` (just "given a transport + credential + model,
//! stream and print"), so the latter can be exercised directly in tests against a
//! `provider::testing::MockTransport` without needing `AuthBroker` to have real persistence yet.

use std::sync::Arc;

use futures::StreamExt as _;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::{AuthError, CredentialHandle};
use xlightcli_protocol::{ModelId, ProviderId, Stability, TransportId, TurnRequest};
use xlightcli_provider::{Provider, TransportAdapter};

use crate::cmd::error::CliError;
use crate::wiring::AppContext;

pub async fn probe(
    ctx: &AppContext,
    provider_arg: &str,
    transport_arg: Option<&str>,
    model_arg: Option<&str>,
    prompt: &str,
    cancel: CancellationToken,
) -> Result<(), CliError> {
    let provider_id = ProviderId::new(provider_arg);
    let provider = ctx.providers.get(&provider_id).ok_or_else(|| {
        CliError::invalid_input(format!(
            "unknown provider {provider_arg:?} (registered: {})",
            joined_provider_ids(ctx)
        ))
    })?;

    let transport_id = match transport_arg {
        Some(t) => TransportId::new(t),
        None => default_transport_id(provider)?,
    };
    let transport = provider.transport(&transport_id).ok_or_else(|| {
        CliError::invalid_input(format!(
            "provider {provider_id} has no transport {transport_id} (available: {})",
            provider
                .transports()
                .iter()
                .map(|t| t.id().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    let cred = ctx
        .auth
        .credential(&provider_id, &transport_id)
        .await
        .map_err(|err| credential_error_to_cli(&provider_id, &transport_id, err))?;

    let model_id = match model_arg {
        Some(m) => ModelId::new(m),
        None => first_model_id(transport, &cred).await?,
    };

    stream_and_render(transport, model_id, cred, prompt, cancel).await
}

fn joined_provider_ids(ctx: &AppContext) -> String {
    ctx.providers
        .ids()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn default_transport_id(provider: &Arc<dyn Provider>) -> Result<TransportId, CliError> {
    provider
        .transports()
        .iter()
        .find(|t| t.stability() == Stability::Stable)
        .or_else(|| provider.transports().first())
        .map(|t| t.id())
        .ok_or_else(|| {
            CliError::invalid_input(format!(
                "provider {} has no transports registered",
                provider.id()
            ))
        })
}

async fn first_model_id(
    transport: &Arc<dyn TransportAdapter>,
    cred: &CredentialHandle,
) -> Result<ModelId, CliError> {
    let models = transport
        .list_models(cred)
        .await
        .map_err(|err| CliError::other(format!("failed to list models: {err}")))?;
    models.into_iter().next().map(|m| m.id).ok_or_else(|| {
        CliError::invalid_input("no --model given and the transport returned no models".to_string())
    })
}

fn credential_error_to_cli(
    provider: &ProviderId,
    transport: &TransportId,
    err: AuthError,
) -> CliError {
    match err {
        AuthError::NotLoggedIn { .. } => CliError::invalid_input(format!(
            "not logged in to {provider} ({transport}); run `xlightcli auth login {provider} \
             --transport {transport}` first"
        )),
        other => CliError::other(format!(
            "could not obtain credential for {provider}/{transport}: {other}"
        )),
    }
}

/// Streams one turn and prints every event via `crate::output::render_event`. `Ctrl-C` cancels
/// `cancel`, which this loop checks with `biased` priority (PATTERNS.md §3) so cancellation is
/// never starved by a fast-producing stream.
pub async fn stream_and_render(
    transport: &Arc<dyn TransportAdapter>,
    model: ModelId,
    cred: CredentialHandle,
    prompt: &str,
    cancel: CancellationToken,
) -> Result<(), CliError> {
    let req = TurnRequest::simple(model, prompt);
    let mut stream = transport
        .stream(req, cred, cancel.clone())
        .await
        .map_err(|err| CliError::other(format!("stream failed to start: {err}")))?;

    let mut produced_output = false;
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                return Err(fail(produced_output, "cancelled".to_string()));
            }
            next = stream.next() => match next {
                None => break,
                Some(Ok(event)) => {
                    produced_output |= crate::output::render_event(&event);
                }
                Some(Err(err)) => {
                    return Err(fail(produced_output, format!("stream error: {err}")));
                }
            },
        }
    }
    Ok(())
}

fn fail(produced_output: bool, msg: String) -> CliError {
    // Logged (not just returned) so operators can see stream failures in
    // `$XDG_STATE_HOME/xlightcli/logs/xlightcli.log`; `crate::logging`'s redacting writer scrubs
    // the formatted line regardless of what `msg` embeds. `CliError::other`/`partial` redact `msg`
    // again on the way out (INV-4 defense in depth) — `msg` can embed an upstream body excerpt
    // (`ProviderError::Upstream`/`Network`'s `Display`).
    tracing::error!(%msg, "dev probe: stream error");
    if produced_output {
        CliError::partial(msg)
    } else {
        CliError::other(msg)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::{AgentEvent, AuthKind, Message, Role, StopReason, Usage};
    use xlightcli_provider::testing::MockProvider;

    use super::*;

    fn test_account() -> xlightcli_auth::AccountInfo {
        xlightcli_auth::AccountInfo {
            provider: ProviderId::new("mock"),
            transport: TransportId::new("mock-transport"),
            account_id: "acc-1".into(),
            label: None,
            auth_kind: AuthKind::ApiKey,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn stream_and_render_succeeds_on_a_clean_completed_script() {
        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![
                Ok(AgentEvent::TurnStarted {
                    model: ModelId::new("mock-model"),
                }),
                Ok(AgentEvent::TextDelta {
                    index: 0,
                    text: "hi".into(),
                }),
                Ok(AgentEvent::Completed {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![],
                    },
                    stop: StopReason::EndTurn,
                    usage: Usage::default(),
                }),
            ],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), "unused-token");

        let result = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stream_error_before_any_output_is_other_not_partial() {
        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![Err(xlightcli_protocol::ProviderError::Network(
                "boom".into(),
            ))],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), "unused-token");

        let err = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.exit_code_number(), 1);
    }

    #[tokio::test]
    async fn stream_error_after_output_is_partial() {
        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![
                Ok(AgentEvent::TextDelta {
                    index: 0,
                    text: "partial".into(),
                }),
                Err(xlightcli_protocol::ProviderError::Network("boom".into())),
            ],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), "unused-token");

        let err = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.exit_code_number(), 3);
    }

    /// Secret sentinel test (docs/PLAN.md §16, PATTERNS.md §13): a `CredentialHandle` carrying a
    /// sentinel secret must never leak into stdout/stderr/log even when the stream errors out.
    /// `CredentialHandle` never exposes the secret to callers (INV-4) and the mock transport never
    /// touches it either, so this mostly documents/guards the invariant at the `dev probe` call
    /// boundary; it also exercises `crate::logging`'s redacting writer end to end.
    #[tokio::test]
    async fn credential_secret_never_appears_in_probe_output_or_log() {
        const SENTINEL: &str = "XLC-SENTINEL-SECRET-abcdefgh1234";
        let temp = tempfile::tempdir().unwrap();
        // Scoped (not global) subscriber: `[workspace.lints] unsafe_code = "forbid"` rules out
        // `std::env::set_var` (edition 2024 makes it `unsafe`), and racing every other test in
        // this binary for the process-global `try_init` slot would make this flaky. See
        // `logging::scoped_for_test`.
        let _log_guard = crate::logging::scoped_for_test(temp.path());

        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![
                Ok(AgentEvent::TextDelta {
                    index: 0,
                    text: "hello".into(),
                }),
                Err(xlightcli_protocol::ProviderError::Upstream {
                    status: 401,
                    body_excerpt: format!("Bearer {SENTINEL}"),
                }),
            ],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), SENTINEL);

        let err = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let rendered = err.to_string();
        assert!(
            !rendered.contains(SENTINEL),
            "sentinel leaked into CliError message: {rendered}"
        );

        let log_path = temp.path().join("xlightcli.log");
        let contents = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|e| panic!("expected a log line at {}: {e}", log_path.display()));
        assert!(
            !contents.is_empty(),
            "expected the stream error to be logged"
        );
        assert!(
            !contents.contains(SENTINEL),
            "sentinel leaked into log file {}: {contents}",
            log_path.display()
        );
    }
}
