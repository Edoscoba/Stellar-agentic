//! Estimating the current ledger, without depending on an RPC server.
//!
//! # The problem
//!
//! Capping `validUntilLedgerSeq` is the one control on how long a leaked
//! auth-entry signature stays replayable on-chain (see
//! `docs/signer-service-design.md`, gap #2). A cap is *relative* — "at most N
//! ledgers past now" — so it needs to know what "now" is in ledger terms.
//!
//! The obvious source is a Soroban RPC server. Phase 3 of the epic says
//! "simulate and inspect the transaction before signing", which reads as
//! exactly that. This service deliberately does **not** take it:
//!
//! - it would make signing unavailable whenever RPC is unavailable, and this
//!   service is already a hard dependency for every payment;
//! - it would require the signer to hold an opinion about *which* RPC to
//!   trust, and a compromised or lagging RPC could then widen the replay
//!   window by under-reporting the ledger;
//! - inspection already achieves the goal the phase is after — knowing what we
//!   are signing — and simulation adds a network round trip to learn something
//!   the envelope already states.
//!
//! This is called out as a deliberate deviation in the design doc, not an
//! oversight.
//!
//! # What this does instead: a ratchet
//!
//! Callers already know the current ledger — the SDK computes
//! `validUntilLedgerSeq` as `simulation.latestLedger + 100`. That number is
//! untrusted on its own, but it is not useless: the service tracks the highest
//! value it has ever accepted and refuses to advance by more than
//! `max_advance` in one step.
//!
//! So the estimate self-calibrates from honest traffic, and a caller trying to
//! buy a long-lived signature has to walk the ratchet forward in bounded
//! increments — each one a separate request, subject to the rate limit, and
//! each one in the audit log.
//!
//! # What it does not do
//!
//! A patient attacker holding a valid token can still ratchet forward over
//! many requests. This bounds the blast radius of a *single* stolen request;
//! it is not a substitute for revoking the token. An operator who wants a hard
//! external bound should supply their own [`LedgerClock`] backed by a source
//! they trust — the trait exists for that.

use std::sync::Mutex;

/// Something that can say what ledger it is.
pub trait LedgerClock: Send + Sync + std::fmt::Debug {
    /// The best current estimate.
    fn current_ledger(&self) -> u32;

    /// Fold in a ledger value observed in a request.
    ///
    /// Returns `Err` with the permitted ceiling when the observation would
    /// advance the estimate too far in one step.
    fn observe(&self, claimed: u32) -> Result<(), u32>;
}

/// The default clock: self-calibrating, bounded, no network.
#[derive(Debug)]
pub struct RatchetingClock {
    state: Mutex<u32>,
    max_advance: u32,
}

impl RatchetingClock {
    /// Start at `initial`, permitting advances of at most `max_advance`.
    ///
    /// `initial` is normally `0` — the first honest request calibrates it —
    /// but an operator restarting a service can seed it from the last value in
    /// the audit log so the ratchet does not reset.
    pub fn new(initial: u32, max_advance: u32) -> Self {
        Self {
            state: Mutex::new(initial),
            max_advance: max_advance.max(1),
        }
    }

    /// How far the estimate may advance in one observation.
    pub fn max_advance(&self) -> u32 {
        self.max_advance
    }
}

impl Default for RatchetingClock {
    fn default() -> Self {
        // ~1,000 ledgers is a little over an hour at five-second ledgers:
        // comfortably more than a restarted fleet needs to re-sync, and far
        // less than a window anyone would want a leaked signature valid for.
        Self::new(0, 1_000)
    }
}

impl LedgerClock for RatchetingClock {
    fn current_ledger(&self) -> u32 {
        *self.state.lock().expect("ledger clock")
    }

    fn observe(&self, claimed: u32) -> Result<(), u32> {
        let mut current = self.state.lock().expect("ledger clock");

        // An uncalibrated clock takes the first observation as truth. There is
        // nothing better available, and refusing every request until some
        // other source appears would make the service unusable on a cold
        // start.
        if *current == 0 {
            *current = claimed;
            return Ok(());
        }

        // A value at or below the estimate is not suspicious — a lagging
        // caller, or two requests racing. It simply does not advance anything.
        if claimed <= *current {
            return Ok(());
        }

        let ceiling = current.saturating_add(self.max_advance);
        if claimed > ceiling {
            return Err(ceiling);
        }

        *current = claimed;
        Ok(())
    }
}

/// A clock pinned to one value. For tests, and for an operator who wants the
/// auth-validity rule evaluated against a value they control.
#[derive(Debug)]
pub struct FixedClock(pub u32);

impl LedgerClock for FixedClock {
    fn current_ledger(&self) -> u32 {
        self.0
    }

    fn observe(&self, _: u32) -> Result<(), u32> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cold_clock_calibrates_from_the_first_observation() {
        // Refusing everything until some other source appears would make the
        // service unusable on a cold start.
        let clock = RatchetingClock::new(0, 1_000);
        assert_eq!(clock.current_ledger(), 0);
        clock.observe(500_000).unwrap();
        assert_eq!(clock.current_ledger(), 500_000);
    }

    #[test]
    fn the_estimate_advances_with_honest_traffic() {
        let clock = RatchetingClock::new(1_000, 100);
        clock.observe(1_050).unwrap();
        assert_eq!(clock.current_ledger(), 1_050);
        clock.observe(1_100).unwrap();
        assert_eq!(clock.current_ledger(), 1_100);
    }

    #[test]
    fn a_far_future_claim_is_refused_and_names_the_ceiling() {
        // The whole point: one stolen request cannot buy an indefinitely
        // long-lived signature.
        let clock = RatchetingClock::new(1_000, 100);
        assert_eq!(clock.observe(9_999_999), Err(1_100));
        // ...and the refusal does not move the estimate.
        assert_eq!(clock.current_ledger(), 1_000);
    }

    #[test]
    fn a_stale_claim_is_accepted_but_does_not_move_the_clock_backwards() {
        // A lagging caller is normal; letting it rewind the ratchet would undo
        // the calibration.
        let clock = RatchetingClock::new(1_000, 100);
        clock.observe(400).unwrap();
        assert_eq!(clock.current_ledger(), 1_000);
    }

    #[test]
    fn the_ratchet_is_bounded_per_step_not_in_total() {
        // Stated honestly: a patient caller can walk it forward. Each step is
        // a separate request, rate-limited and audited.
        let clock = RatchetingClock::new(1_000, 100);
        for _ in 0..10 {
            let next = clock.current_ledger() + 100;
            clock.observe(next).unwrap();
        }
        assert_eq!(clock.current_ledger(), 2_000);
    }

    #[test]
    fn a_zero_max_advance_is_clamped_so_the_clock_can_still_move() {
        // A misconfigured `0` would freeze the estimate forever and refuse
        // every auth-entry request after the first.
        let clock = RatchetingClock::new(1_000, 0);
        assert_eq!(clock.max_advance(), 1);
        clock.observe(1_001).unwrap();
        assert_eq!(clock.current_ledger(), 1_001);
    }

    #[test]
    fn advancing_near_u32_max_saturates_rather_than_wrapping() {
        let clock = RatchetingClock::new(u32::MAX - 10, 1_000);
        assert!(clock.observe(u32::MAX).is_ok());
        assert_eq!(clock.current_ledger(), u32::MAX);
    }

    #[test]
    fn a_fixed_clock_never_moves() {
        let clock = FixedClock(42);
        clock.observe(999_999).unwrap();
        assert_eq!(clock.current_ledger(), 42);
    }
}
