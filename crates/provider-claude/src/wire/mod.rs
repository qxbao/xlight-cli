// SPDX-License-Identifier: GPL-3.0-only

//! `pub(crate)` Anthropic Messages wire types + translator, shared by `transport_api` and
//! `transport_subscription`. No wire type may appear in a `pub` signature (INV-3).

pub(crate) mod request;
pub(crate) mod response;
