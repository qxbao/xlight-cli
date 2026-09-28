// SPDX-License-Identifier: GPL-3.0-only

//! `TransportGate` — experimental opt-in + kill switch (D-002, docs/PLAN.md §15).
//!
//! Deliberately does not depend on `xlightcli-config` (CODEBASE.md §3: `provider` only depends on
//! `protocol` + `auth`). Callers (in `provider-*`, which do depend on `config`) read
//! `ExperimentalFlags`/`Config::disabled_transports` and pass the resulting booleans in here.

use std::sync::atomic::{AtomicBool, Ordering};

use xlightcli_protocol::{ProviderError, Stability, TransportId};

/// Gate for one transport: combines its static `Stability` with a runtime kill switch that can be
/// flipped on by the transport itself (docs/PLAN.md §15: "repeated adapter failures ⇒ self-disable").
#[derive(Debug)]
pub struct TransportGate {
    transport: TransportId,
    stability: Stability,
    disabled: AtomicBool,
}

impl TransportGate {
    pub fn new(transport: TransportId, stability: Stability) -> Self {
        Self {
            transport,
            stability,
            disabled: AtomicBool::new(false),
        }
    }

    /// Kill switch: disables the transport for the rest of the process/session regardless of
    /// opt-in state (e.g. after repeated adapter failures, or `xlightcli provider disable`).
    pub fn disable(&self) {
        self.disabled.store(true, Ordering::Relaxed);
    }

    pub fn enable(&self) {
        self.disabled.store(false, Ordering::Relaxed);
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Relaxed)
    }

    pub fn stability(&self) -> Stability {
        self.stability
    }

    /// Checks whether a call is allowed right now. `experimental_opt_in` is the caller's already
    /// resolved `[experimental]` config flag for this transport; ignored for `Stability::Stable`
    /// transports.
    pub fn ensure_enabled(&self, experimental_opt_in: bool) -> Result<(), ProviderError> {
        if self.is_disabled() {
            return Err(ProviderError::TransportDisabled {
                reason: format!("{} is disabled (kill switch)", self.transport),
            });
        }
        if self.stability == Stability::Experimental && !experimental_opt_in {
            return Err(ProviderError::TransportDisabled {
                reason: format!(
                    "{} is experimental; enable it via [experimental] in the global config first",
                    self.transport
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn gate(stability: Stability) -> TransportGate {
        TransportGate::new(TransportId::new("claude-subscription"), stability)
    }

    #[test]
    fn stable_transport_does_not_need_opt_in() {
        let gate = gate(Stability::Stable);
        assert!(gate.ensure_enabled(false).is_ok());
    }

    #[test]
    fn experimental_transport_requires_opt_in() {
        let gate = gate(Stability::Experimental);
        assert!(matches!(
            gate.ensure_enabled(false),
            Err(ProviderError::TransportDisabled { .. })
        ));
        assert!(gate.ensure_enabled(true).is_ok());
    }

    #[test]
    fn kill_switch_overrides_opt_in() {
        let gate = gate(Stability::Experimental);
        gate.disable();
        assert!(matches!(
            gate.ensure_enabled(true),
            Err(ProviderError::TransportDisabled { .. })
        ));
        gate.enable();
        assert!(gate.ensure_enabled(true).is_ok());
    }
}
