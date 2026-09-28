// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-protocol` — canonical, IO-free types shared by every crate in the workspace
//! (CODEBASE.md §2).
//!
//! This crate has **no internal dependencies** and performs no IO. Provider wire types are
//! `pub(crate)` inside each `provider-*` adapter crate and are translated to/from the types
//! defined here (INV-3): the agent runtime, tools and TUI only ever see `TurnRequest` /
//! `AgentEvent` / `Message`.

pub mod capability;
pub mod error;
pub mod event;
pub mod ids;
pub mod import;
pub mod message;
pub mod tool;
pub mod turn;

pub use capability::{
    AuthKind, CapabilityMode, ModelInfo, ProtocolVersion, ProviderCapabilities, Stability,
};
pub use error::{AuthFailure, ProviderError};
pub use event::{AgentEvent, QuotaSnapshot, RateLimitInfo, StopReason, Usage};
pub use ids::{
    AgentId, CommandId, ModelId, ProviderId, SessionId, ToolCallId, TransportId, WorkspaceId,
};
pub use import::ConfigFragment;
pub use message::{ContentBlock, ImageSource, Message, OpaqueBlob, Role, ToolResultPart};
pub use tool::ToolDefinition;
pub use turn::{ProviderOptions, ReasoningConfig, ReasoningEffort, SystemPrompt, TurnRequest};
