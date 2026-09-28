// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-auth` — `AuthBroker`, `CredentialHandle`, OAuth toolkit (CODEBASE.md §2,
//! docs/PLAN.md §6).
//!
//! Raw credentials never leave this crate (INV-4): everything outside sees only a
//! `CredentialHandle`. Secrets are wrapped in `secrecy::SecretString`; nothing outside
//! `crate::store` may turn a secret into serialized bytes (PATTERNS.md §4).
//!
//! **Status (Phase 0 / Wave 1):** public contract is final; `broker::AuthBroker::credential` /
//! `login` / `import` persistence, `store::SecretStore` backends, `refresh::RefreshCoordinator`
//! and most of `oauth`'s IO are explicit `AuthError::NotImplemented` stubs for Wave 2. Trivial
//! parts (PKCE, authorization URL building, `CredentialHandle::authorize`/`Debug`, discovery
//! filtering, log redaction) are fully implemented and tested.

pub mod adapter;
pub mod broker;
pub mod credential;
pub mod discovery;
pub mod error;
pub mod handle;
pub mod login_ui;
pub mod oauth;
pub mod redact;
pub mod refresh;
pub mod store;

pub use adapter::{AuthAdapter, AuthMethod, DiscoveredCredential};
pub use broker::AuthBroker;
pub use credential::{AccountInfo, CredentialSecret, CredentialSet};
pub use error::AuthError;
pub use handle::CredentialHandle;
pub use login_ui::LoginUi;
pub use refresh::RefreshCoordinator;
pub use store::{FileStore, KeyringStore, SecretStore, StoreKind, UnimplementedStore};
