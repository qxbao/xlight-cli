// SPDX-License-Identifier: GPL-3.0-only

//! Test-only mocks, behind cargo feature `testing`.
//!
//! `MockProvider`/`MockTransport` replay a scripted `AgentEvent` sequence with no network, so
//! `runtime` (and anyone else) can exercise the agent loop deterministically (PATTERNS.md §13).
//! `parse_sse_fixture` feeds a `.sse` fixture file through `crate::sse::parse` for adapter wire
//! tests.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::{
    AuthAdapter, AuthMethod, CredentialHandle, CredentialSet, DiscoveredCredential, LoginUi,
};
use xlightcli_protocol::{
    AgentEvent, AuthKind, CapabilityMode, ModelInfo, ProtocolVersion, ProviderCapabilities,
    ProviderError, ProviderId, QuotaSnapshot, ReasoningEffort, Stability, TransportId, TurnRequest,
};

use crate::sse::{self, SseEvent};
use crate::traits::{
    CommandContext, CommandDefinition, CommandError, CommandResult, EventStream, Provider,
    ProviderCommand, ProviderFeaturePack, TransportAdapter,
};

fn no_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        reasoning: false,
        images: false,
        tool_calls: false,
        parallel_tool_calls: false,
        web_search: CapabilityMode::Unsupported,
        mcp: CapabilityMode::Core,
        session_resume: CapabilityMode::Core,
        usage: CapabilityMode::Unsupported,
        quota: CapabilityMode::Unsupported,
        context_window: None,
    }
}

/// Transport that replays a fixed, pre-scripted sequence of stream items regardless of the
/// `TurnRequest` it receives.
#[derive(Debug)]
pub struct MockTransport {
    id: TransportId,
    capabilities: ProviderCapabilities,
    script: Vec<Result<AgentEvent, ProviderError>>,
}

impl MockTransport {
    pub fn new(id: impl Into<TransportId>, script: Vec<Result<AgentEvent, ProviderError>>) -> Self {
        Self {
            id: id.into(),
            capabilities: no_capabilities(),
            script,
        }
    }
}

#[async_trait]
impl TransportAdapter for MockTransport {
    fn id(&self) -> TransportId {
        self.id.clone()
    }

    fn stability(&self) -> Stability {
        Stability::Stable
    }

    fn required_auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn protocol_version(&self) -> ProtocolVersion {
        ProtocolVersion(1)
    }

    async fn list_models(&self, _cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(Vec::new())
    }

    async fn quota(
        &self,
        _cred: &CredentialHandle,
    ) -> Result<Option<QuotaSnapshot>, ProviderError> {
        Ok(None)
    }

    fn apply_effort(&self, _req: &mut TurnRequest, _effort: ReasoningEffort) {}

    async fn stream(
        &self,
        _req: TurnRequest,
        _cred: CredentialHandle,
        _cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let items = self.script.clone();
        Ok(Box::pin(async_stream::stream! {
            for item in items {
                yield item;
            }
        }))
    }
}

#[derive(Debug)]
struct MockAuthAdapter;

#[async_trait]
impl AuthAdapter for MockAuthAdapter {
    fn methods(&self) -> &[AuthMethod] {
        &[AuthMethod::ApiKey]
    }

    async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
        Vec::new()
    }

    async fn import(
        &self,
        _found: &DiscoveredCredential,
    ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
        Err(xlightcli_auth::AuthError::NotImplemented(
            "MockAuthAdapter has no real backing store",
        ))
    }

    async fn login(
        &self,
        _method: AuthMethod,
        _ui: &dyn LoginUi,
    ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
        Err(xlightcli_auth::AuthError::NotImplemented(
            "MockAuthAdapter has no real backing store",
        ))
    }

    async fn refresh(
        &self,
        current: &CredentialSet,
    ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
        Ok(current.clone())
    }

    async fn revoke(&self, _current: &CredentialSet) -> Result<(), xlightcli_auth::AuthError> {
        Ok(())
    }
}

#[derive(Debug)]
struct MockFeaturePack;

#[async_trait]
impl ProviderFeaturePack for MockFeaturePack {
    fn commands(&self) -> Vec<CommandDefinition> {
        Vec::new()
    }

    async fn execute(
        &self,
        _cmd: ProviderCommand,
        _ctx: CommandContext,
    ) -> Result<CommandResult, CommandError> {
        Ok(CommandResult::Unavailable {
            reason: "MockProvider has no commands".into(),
        })
    }
}

/// A `Provider` with one `MockTransport`, for `runtime`/agent-loop tests (no network, no auth
/// broker required — `CredentialHandle::for_tests` supplies the credential).
pub struct MockProvider {
    id: ProviderId,
    auth: MockAuthAdapter,
    features: MockFeaturePack,
    transports: Vec<Arc<dyn TransportAdapter>>,
}

impl std::fmt::Debug for MockProvider {
    /// Manual impl: `dyn TransportAdapter` has no `Debug` bound, so only transport ids are shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockProvider")
            .field("id", &self.id)
            .field(
                "transports",
                &self.transports.iter().map(|t| t.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl MockProvider {
    /// Builds a provider whose single transport `transport_id` replays `script` for every call to
    /// `stream()`, ignoring the request it's given.
    pub fn scripted(
        provider_id: impl Into<ProviderId>,
        transport_id: impl Into<TransportId>,
        script: Vec<Result<AgentEvent, ProviderError>>,
    ) -> Self {
        Self {
            id: provider_id.into(),
            auth: MockAuthAdapter,
            features: MockFeaturePack,
            transports: vec![Arc::new(MockTransport::new(transport_id, script))],
        }
    }
}

impl Provider for MockProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn display_name(&self) -> &str {
        "Mock Provider"
    }

    fn auth(&self) -> &dyn AuthAdapter {
        &self.auth
    }

    fn transports(&self) -> &[Arc<dyn TransportAdapter>] {
        &self.transports
    }

    fn features(&self) -> &dyn ProviderFeaturePack {
        &self.features
    }
}

/// Reads `path`, splits it into two chunks at its midpoint, and feeds it through
/// `crate::sse::parse` — exercising the "chunk boundary splits mid-something" path even for
/// fixtures that would otherwise be small enough to arrive in one read.
pub async fn parse_sse_fixture(path: &Path) -> Result<Vec<SseEvent>, ProviderError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| ProviderError::Network(format!("reading fixture {}: {e}", path.display())))?;
    let mid = bytes.len() / 2;
    let (first, second) = bytes.split_at(mid);
    let chunks: Vec<Result<Bytes, ProviderError>> = vec![
        Ok(Bytes::copy_from_slice(first)),
        Ok(Bytes::copy_from_slice(second)),
    ];
    let stream = futures::stream::iter(chunks);
    sse::parse(stream)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_protocol::{ModelId, StopReason, Usage};

    use super::*;

    fn sample_account() -> xlightcli_auth::AccountInfo {
        xlightcli_auth::AccountInfo {
            provider: ProviderId::new("mock"),
            transport: TransportId::new("mock-transport"),
            account_id: "acc".into(),
            label: None,
            auth_kind: AuthKind::ApiKey,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn mock_transport_replays_scripted_events_in_order() {
        let script = vec![
            Ok(AgentEvent::TurnStarted {
                model: ModelId::new("mock-model"),
            }),
            Ok(AgentEvent::TextDelta {
                index: 0,
                text: "hi".into(),
            }),
            Ok(AgentEvent::Completed {
                message: xlightcli_protocol::Message {
                    role: xlightcli_protocol::Role::Assistant,
                    content: vec![],
                },
                stop: StopReason::EndTurn,
                usage: Usage::default(),
            }),
        ];
        let provider = MockProvider::scripted("mock", "mock-transport", script.clone());
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(sample_account(), "unused-token");

        let stream = transport
            .stream(
                TurnRequest::simple(ModelId::new("mock-model"), "hi"),
                cred,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), script.len());
    }

    #[tokio::test]
    async fn parse_sse_fixture_reads_and_parses_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.sse");
        tokio::fs::write(&path, b"data: hello\n\ndata: world\n\n")
            .await
            .unwrap();
        let events = parse_sse_fixture(&path).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[1].data, "world");
    }
}
