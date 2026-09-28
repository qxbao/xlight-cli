// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-tools` — `Tool` trait, `ToolRegistry`, built-in tools, `PermissionEngine`,
//! `ProcessLauncher` (the single spawn point, INV-1), `OutputSpool` (INV-7).
//!
//! **Status (Phase 1 Wave A — contracts):** `permission`, `launcher`, `spool`, `tool`,
//! `registry` are real, tested code. `builtin`'s tool *structs* are real (definitions, effects,
//! registration) but every `run` body is a documented stub (`ToolError::NotImplemented`) — see
//! the `builtin` module doc for what each one still needs.
//!
//! This crate intentionally does **not** depend on `xlightcli-auth` (INV-4): no code path here
//! can ever see a provider credential.

pub mod builtin;
pub mod launcher;
pub mod permission;
pub mod registry;
pub mod spool;
pub mod tool;

pub use launcher::{
    EnvPolicy, ProcessLauncher, SpawnError, SpawnPurpose, SpawnSpec, SpawnedProcess,
};
pub use permission::{
    AskPolicy, ExecutionMode, PermissionAction, PermissionDecision, PermissionEngine,
    PermissionGate, PermissionMode, PermissionRequest, PermissionRule, RuleEffect,
    StaticPermissionGate,
};
pub use registry::ToolRegistry;
pub use spool::{ArtifactRef, OutputSpool, SpoolLimits, SpooledOutput};
pub use tool::{
    Tool, ToolContext, ToolEffect, ToolError, ToolOutput, ToolSpoolConfig, WorkspacePath,
};
