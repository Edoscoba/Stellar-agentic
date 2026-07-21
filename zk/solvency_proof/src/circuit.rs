//! The solvency circuit.
//!
//! Proves: "there exists a sequence of (at most `MAX_PAYMENTS`) private
//! payments, grouped into non-decreasing periods, such that the running
//! spend within every period never exceeded `limit_per_period`, and the
//! payments sum to exactly `total_spent`" — without revealing the
//! individual payment amounts or how many periods they span.
//!
//! `limit_per_period` and `total_spent` are the circuit's only public
//! inputs. Both are read directly off `PaymentChannel`'s own on-chain state
//! (`Channel::limit_per_period`, `Channel::total_spent`) by the verifier —
//! they are not something the prover gets to choose, so a proof is only
//! useful for the specific channel whose live state matches those two
//! values.
//!
//! See `../../../docs/zk-solvency-design.md` for the full design writeup,
//! including why per-payment amounts are bounded the way they are.

use ark_bls12_381::Fr;
use ark_ff::Zero;
use ark_r1cs_std::{
    alloc::AllocVar,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    select::CondSelectGadget,
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use core::cmp::Ordering;

/// Fixed circuit capacity. A real channel's history is padded with
/// zero-amount, repeated-period "no-op" slots up to this length, or split
/// across multiple proofs if it has more than this many payments. Kept
/// small so setup/proving stay fast for the prototype; see the design doc
/// for how this generalizes.
pub const MAX_PAYMENTS: usize = 8;

/// One private payment: how much was spent, and which spend-limit period
/// (an opaque, prover-assigned, non-decreasing index — not a ledger number)
/// it falls into.
#[derive(Clone, Copy, Debug)]
pub struct PrivatePayment {
    pub amount: Fr,
    pub period_id: Fr,
}

impl PrivatePayment {
    pub fn zero() -> Self {
        PrivatePayment {
            amount: Fr::zero(),
            period_id: Fr::zero(),
        }
    }
}

/// The solvency circuit. `payments` holds `MAX_PAYMENTS` private witness
/// slots (unused slots are zero-amount padding). `limit_per_period` and
/// `total_spent` are the public inputs.
#[derive(Clone)]
pub struct SolvencyCircuit {
    pub payments: [PrivatePayment; MAX_PAYMENTS],
    pub limit_per_period: Fr,
    pub total_spent: Fr,
}

impl ConstraintSynthesizer<Fr> for SolvencyCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let limit_per_period =
            FpVar::new_input(ark_relations::ns!(cs, "limit_per_period"), || {
                Ok(self.limit_per_period)
            })?;
        let total_spent_public = FpVar::new_input(ark_relations::ns!(cs, "total_spent"), || {
            Ok(self.total_spent)
        })?;

        let mut total = FpVar::<Fr>::zero();
        let mut running_period_spend = FpVar::<Fr>::zero();
        let mut current_period_marker = FpVar::<Fr>::zero();

        for (i, payment) in self.payments.iter().enumerate() {
            let amount =
                FpVar::new_witness(ark_relations::ns!(cs, "amount"), || Ok(payment.amount))?;
            let period_id =
                FpVar::new_witness(
                    ark_relations::ns!(cs, "period_id"),
                    || Ok(payment.period_id),
                )?;

            // A single payment can never exceed the whole period's limit.
            // This also pins `amount` into the field's safe (non-wraparound)
            // comparison range — see the design doc's soundness discussion.
            amount.enforce_cmp(&limit_per_period, Ordering::Less, true)?;

            if i > 0 {
                // Periods are visited in non-decreasing order.
                period_id.enforce_cmp(&current_period_marker, Ordering::Greater, true)?;
            }

            let same_period = period_id.is_eq(&current_period_marker)?;
            let period_base =
                FpVar::conditionally_select(&same_period, &running_period_spend, &FpVar::zero())?;
            let new_running_spend = &period_base + &amount;

            // The running spend within this period never exceeds the limit.
            new_running_spend.enforce_cmp(&limit_per_period, Ordering::Less, true)?;

            running_period_spend = new_running_spend;
            current_period_marker = period_id;
            total = &total + &amount;
        }

        total.enforce_equal(&total_spent_public)?;

        Ok(())
    }
}
