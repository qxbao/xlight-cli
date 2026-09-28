// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-config` — XDG paths and the experimental-flag gate (CODEBASE.md §2).
//!
//! **Status (Phase 0): minimal.** Full layered config (`layer`, `merge`, `schema`, `trust` —
//! docs/PLAN.md §12) lands in Phase 1. This crate depends only on `xlightcli-protocol`
//! (dependency rules, CODEBASE.md §3).

pub mod flags;
pub mod paths;

pub use flags::{Config, ExperimentalFlags};
