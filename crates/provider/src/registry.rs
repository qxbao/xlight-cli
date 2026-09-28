// SPDX-License-Identifier: GPL-3.0-only

//! `ProviderRegistry` — holds every `Provider` the running binary knows about. Only `app` ever
//! populates one (CODEBASE.md §3: `app` is the only crate that knows concrete providers).

use std::collections::BTreeMap;
use std::sync::Arc;

use xlightcli_protocol::ProviderId;

use crate::traits::Provider;

#[derive(Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<ProviderId, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.id(), provider);
    }

    pub fn get(&self, id: &ProviderId) -> Option<&Arc<dyn Provider>> {
        self.providers.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &ProviderId> {
        self.providers.keys()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Provider>> {
        self.providers.values()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use async_trait::async_trait;
    use xlightcli_auth::{AuthAdapter, AuthMethod, CredentialSet, DiscoveredCredential, LoginUi};

    use super::*;
    use crate::traits::{
        CommandContext, CommandDefinition, CommandError, CommandResult, ProviderCommand,
        ProviderFeaturePack,
    };

    struct NoopAuth;

    #[async_trait]
    impl AuthAdapter for NoopAuth {
        fn methods(&self) -> &[AuthMethod] {
            &[]
        }
        async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
            Vec::new()
        }
        async fn import(
            &self,
            _found: &DiscoveredCredential,
        ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
            Err(xlightcli_auth::AuthError::NotImplemented("noop"))
        }
        async fn login(
            &self,
            _method: AuthMethod,
            _ui: &dyn LoginUi,
        ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
            Err(xlightcli_auth::AuthError::NotImplemented("noop"))
        }
        async fn refresh(
            &self,
            _current: &CredentialSet,
        ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
            Err(xlightcli_auth::AuthError::NotImplemented("noop"))
        }
        async fn revoke(&self, _current: &CredentialSet) -> Result<(), xlightcli_auth::AuthError> {
            Ok(())
        }
    }

    struct NoopFeatures;

    #[async_trait]
    impl ProviderFeaturePack for NoopFeatures {
        fn commands(&self) -> Vec<CommandDefinition> {
            Vec::new()
        }
        async fn execute(
            &self,
            _cmd: ProviderCommand,
            _ctx: CommandContext,
        ) -> Result<CommandResult, CommandError> {
            Ok(CommandResult::Unavailable {
                reason: "test stub".into(),
            })
        }
    }

    struct StubProvider {
        id: ProviderId,
        auth: NoopAuth,
        features: NoopFeatures,
        transports: Vec<Arc<dyn crate::traits::TransportAdapter>>,
    }

    impl Provider for StubProvider {
        fn id(&self) -> ProviderId {
            self.id.clone()
        }
        fn display_name(&self) -> &str {
            "Stub"
        }
        fn auth(&self) -> &dyn AuthAdapter {
            &self.auth
        }
        fn transports(&self) -> &[Arc<dyn crate::traits::TransportAdapter>] {
            &self.transports
        }
        fn features(&self) -> &dyn ProviderFeaturePack {
            &self.features
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut registry = ProviderRegistry::new();
        assert!(registry.is_empty());
        registry.register(Arc::new(StubProvider {
            id: ProviderId::new("codex"),
            auth: NoopAuth,
            features: NoopFeatures,
            transports: Vec::new(),
        }));
        assert_eq!(registry.len(), 1);
        assert!(registry.get(&ProviderId::new("codex")).is_some());
        assert!(registry.get(&ProviderId::new("claude")).is_none());
    }
}
