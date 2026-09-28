// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-provider` — `Provider`/`TransportAdapter` traits and shared adapter infrastructure
//! (CODEBASE.md §2).
//!
//! Depends only on `xlightcli-protocol` and `xlightcli-auth` (CODEBASE.md §3); provider-specific
//! crates (`provider-codex`, `provider-claude`, `provider-agy`) additionally depend on
//! `xlightcli-config` for the experimental gate's runtime opt-in.

pub mod error;
pub mod gate;
pub mod http;
pub mod registry;
pub mod retry;
pub mod sse;
#[cfg(feature = "testing")]
pub mod testing;
pub mod traits;

pub use error::{BODY_EXCERPT_LIMIT, body_excerpt, map_status};
pub use gate::TransportGate;
pub use http::{HttpClientConfig, build_client};
pub use registry::ProviderRegistry;
pub use retry::Backoff;
pub use sse::{SseEvent, SseParser, parse as parse_sse};
pub use traits::{
    CommandContext, CommandDefinition, CommandError, CommandResult, ConfigImporter, EventStream,
    Provider, ProviderCommand, ProviderFeaturePack, RichText, TransportAdapter, TurnRequestPatch,
};
