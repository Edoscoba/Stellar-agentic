//! Off-chain Groth16 prover/verifier for `PaymentChannel` solvency proofs.
//!
//! See `docs/zk-solvency-design.md` (repo root) for the full design writeup.
//! In short: a channel owner who has a private list of individual payments
//! can prove, without revealing that list, that it is a valid explanation
//! of the channel's own public `total_spent` figure — i.e. that some
//! ordering of those payments into spend-limit periods never exceeded
//! `limit_per_period`, and that they sum to exactly `total_spent`.

pub mod circuit;
pub mod soroban_encoding;

use ark_bls12_381::{Bls12_381, Fr};
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem, SynthesisError};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_snark::SNARK;
use ark_std::rand::{CryptoRng, RngCore};

use circuit::{PrivatePayment, SolvencyCircuit, MAX_PAYMENTS};
use soroban_encoding::{
    fr_to_soroban_bytes, g1_to_soroban_bytes, g2_to_soroban_bytes, FR_SIZE, G1_SIZE, G2_SIZE,
};

#[derive(Debug)]
pub enum Error {
    /// The private history has more payments than the circuit can hold in
    /// one proof (`MAX_PAYMENTS`). See the design doc for how to extend
    /// this (batching into multiple proofs, or a variable-length circuit).
    TooManyPayments {
        given: usize,
        max: usize,
    },
    /// A single payment exceeds the period limit on its own — no valid
    /// witness can exist, so there's no point asking the prover to try.
    PaymentExceedsLimit {
        amount: u128,
        limit: u128,
    },
    /// The given payment history's actual total doesn't match the claimed
    /// public `total_spent` — same reasoning as above.
    TotalMismatch {
        computed: u128,
        claimed: u128,
    },
    Synthesis(SynthesisError),
}

impl From<SynthesisError> for Error {
    fn from(e: SynthesisError) -> Self {
        Error::Synthesis(e)
    }
}

/// One payment in a channel owner's own private record of what they spent.
/// `period_index` is an opaque, caller-assigned, non-decreasing counter
/// identifying which spend-limit period the payment falls in (e.g. derived
/// from `(ledger - period_start_ledger) / ledgers_per_period` at proving
/// time) — it does not need to match on-chain ledger numbers exactly, only
/// to correctly group payments that shared a limit window.
#[derive(Clone, Copy, Debug)]
pub struct HistoryEntry {
    pub amount: u128,
    pub period_index: u64,
}

/// Builds the fixed-size witness array the circuit expects: real payments
/// first, padded with zero-amount no-ops (repeating the last period) up to
/// `MAX_PAYMENTS`. Also runs the same checks the circuit will enforce, so
/// callers get a clear error instead of an unprovable circuit.
fn build_witness(
    history: &[HistoryEntry],
    limit_per_period: u128,
) -> Result<[PrivatePayment; MAX_PAYMENTS], Error> {
    if history.len() > MAX_PAYMENTS {
        return Err(Error::TooManyPayments {
            given: history.len(),
            max: MAX_PAYMENTS,
        });
    }

    let mut payments = [PrivatePayment::zero(); MAX_PAYMENTS];
    let mut running_period_spend: u128 = 0;
    let mut current_period: u64 = 0;

    for (i, entry) in history.iter().enumerate() {
        if entry.amount > limit_per_period {
            return Err(Error::PaymentExceedsLimit {
                amount: entry.amount,
                limit: limit_per_period,
            });
        }
        if i == 0 || entry.period_index != current_period {
            running_period_spend = 0;
        }
        running_period_spend += entry.amount;
        if running_period_spend > limit_per_period {
            return Err(Error::PaymentExceedsLimit {
                amount: running_period_spend,
                limit: limit_per_period,
            });
        }
        current_period = entry.period_index;

        payments[i] = PrivatePayment {
            amount: Fr::from(entry.amount),
            period_id: Fr::from(current_period),
        };
    }

    // Pad remaining slots with zero-amount no-ops in the last real period
    // (or period 0 if there was no history at all) so the monotonic-period
    // constraint stays satisfied.
    for slot in payments.iter_mut().skip(history.len()) {
        slot.period_id = Fr::from(current_period);
    }

    Ok(payments)
}

fn history_total(history: &[HistoryEntry]) -> u128 {
    history.iter().map(|e| e.amount).sum()
}

/// Runs Groth16's circuit-specific setup for the solvency circuit.
///
/// **This is a toy, single-party setup — not a real trusted-setup
/// ceremony.** The `rng` here produces the Groth16 "toxic waste" (the
/// secret randomness used to build the proving/verifying keys); whoever
/// generates it could forge false proofs if they kept it. A real deployment
/// needs a multi-party computation ceremony (as Zcash/Filecoin/etc. run) so
/// no single party ever holds the full secret, or a switch to a
/// universal-setup or setup-free proving system. See the design doc's
/// "Trusted setup" section.
pub fn setup<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<(ProvingKey<Bls12_381>, VerifyingKey<Bls12_381>), Error> {
    let blank_circuit = SolvencyCircuit {
        payments: [PrivatePayment::zero(); MAX_PAYMENTS],
        limit_per_period: Fr::from(0u64),
        total_spent: Fr::from(0u64),
    };
    let (pk, vk) = Groth16::<Bls12_381>::circuit_specific_setup(blank_circuit, rng)?;
    Ok((pk, vk))
}

/// Generates a Groth16 proof that `history` is a valid, limit-respecting
/// explanation of `total_spent` under `limit_per_period`.
///
/// Fails fast (without invoking the prover) if `history` couldn't possibly
/// satisfy the circuit — e.g. if it doesn't actually sum to `total_spent`,
/// or if any prefix would have exceeded the limit. This mirrors exactly
/// what the circuit itself would enforce, so a well-formed history always
/// produces a verifiable proof.
pub fn prove<R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bls12_381>,
    history: &[HistoryEntry],
    limit_per_period: u128,
    total_spent: u128,
    rng: &mut R,
) -> Result<Proof<Bls12_381>, Error> {
    let computed_total = history_total(history);
    if computed_total != total_spent {
        return Err(Error::TotalMismatch {
            computed: computed_total,
            claimed: total_spent,
        });
    }

    let payments = build_witness(history, limit_per_period)?;
    let circuit = SolvencyCircuit {
        payments,
        limit_per_period: Fr::from(limit_per_period),
        total_spent: Fr::from(total_spent),
    };

    // Sanity-check locally before spending time on a real proof: a witness
    // that doesn't satisfy the circuit can never produce a proof that
    // verifies, so surface that as a clear synthesis error instead of a
    // proof the verifier will just reject.
    let cs = ConstraintSystem::<Fr>::new_ref();
    circuit.clone().generate_constraints(cs.clone())?;
    if !cs.is_satisfied()? {
        return Err(Error::Synthesis(SynthesisError::Unsatisfiable));
    }

    let proof = Groth16::<Bls12_381>::prove(pk, circuit, rng)?;
    Ok(proof)
}

/// Off-chain (pure arkworks) verification — useful for a quick local check
/// without spinning up a Soroban environment. The authoritative check for
/// this project is the on-chain one in `PaymentChannel::verify_solvency_proof`,
/// which uses this exact same (vk, proof) data, re-encoded for Soroban's
/// native BLS12-381 host functions (see `soroban_encoding`).
pub fn verify_native(
    vk: &VerifyingKey<Bls12_381>,
    limit_per_period: u128,
    total_spent: u128,
    proof: &Proof<Bls12_381>,
) -> Result<bool, Error> {
    let public_inputs = [Fr::from(limit_per_period), Fr::from(total_spent)];
    Ok(Groth16::<Bls12_381>::verify(vk, &public_inputs, proof)?)
}

/// A Groth16 proof, re-encoded into the exact byte layout Soroban's native
/// BLS12-381 host functions expect (see `soroban_encoding`).
pub struct SorobanProof {
    pub a: [u8; G1_SIZE],
    pub b: [u8; G2_SIZE],
    pub c: [u8; G1_SIZE],
}

pub fn proof_to_soroban_bytes(proof: &Proof<Bls12_381>) -> SorobanProof {
    SorobanProof {
        a: g1_to_soroban_bytes(&proof.a),
        b: g2_to_soroban_bytes(&proof.b),
        c: g1_to_soroban_bytes(&proof.c),
    }
}

/// A Groth16 verifying key, re-encoded into Soroban's native BLS12-381 byte
/// layout — this is what gets passed to `PaymentChannel::set_solvency_vk`.
pub struct SorobanVerifyingKey {
    pub alpha_g1: [u8; G1_SIZE],
    pub beta_g2: [u8; G2_SIZE],
    pub gamma_g2: [u8; G2_SIZE],
    pub delta_g2: [u8; G2_SIZE],
    pub gamma_abc_g1: Vec<[u8; G1_SIZE]>,
}

pub fn vk_to_soroban_bytes(vk: &VerifyingKey<Bls12_381>) -> SorobanVerifyingKey {
    SorobanVerifyingKey {
        alpha_g1: g1_to_soroban_bytes(&vk.alpha_g1),
        beta_g2: g2_to_soroban_bytes(&vk.beta_g2),
        gamma_g2: g2_to_soroban_bytes(&vk.gamma_g2),
        delta_g2: g2_to_soroban_bytes(&vk.delta_g2),
        gamma_abc_g1: vk.gamma_abc_g1.iter().map(g1_to_soroban_bytes).collect(),
    }
}

/// The two public inputs, as Soroban `Fr` bytes, in the order the circuit
/// declares them (`limit_per_period`, then `total_spent`).
pub fn public_inputs_to_soroban_bytes(
    limit_per_period: u128,
    total_spent: u128,
) -> [[u8; FR_SIZE]; 2] {
    [
        fr_to_soroban_bytes(&Fr::from(limit_per_period)),
        fr_to_soroban_bytes(&Fr::from(total_spent)),
    ]
}

/// Serializes a proving/verifying key to arkworks' own (non-Soroban)
/// canonical format, for saving to / loading from disk between CLI runs.
pub fn serialize_canonical<T: CanonicalSerialize>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value
        .serialize_compressed(&mut out)
        .expect("serialization to an in-memory buffer cannot fail");
    out
}

pub fn deserialize_canonical<T: CanonicalDeserialize>(
    bytes: &[u8],
) -> Result<T, ark_serialize::SerializationError> {
    T::deserialize_compressed(bytes)
}

#[cfg(test)]
mod test {
    use super::*;
    use ark_std::rand::rngs::StdRng;
    use ark_std::rand::SeedableRng;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    #[test]
    fn valid_history_proves_and_verifies_natively() {
        let (pk, vk) = setup(&mut rng()).unwrap();

        let history = [
            HistoryEntry {
                amount: 100_000,
                period_index: 0,
            },
            HistoryEntry {
                amount: 200_000,
                period_index: 0,
            },
            HistoryEntry {
                amount: 50_000,
                period_index: 1,
            },
            HistoryEntry {
                amount: 400_000,
                period_index: 1,
            },
        ];
        let limit_per_period = 500_000u128;
        let total_spent = 750_000u128;

        let proof = prove(&pk, &history, limit_per_period, total_spent, &mut rng()).unwrap();
        assert!(verify_native(&vk, limit_per_period, total_spent, &proof).unwrap());
    }

    #[test]
    fn empty_history_with_zero_total_verifies() {
        let (pk, vk) = setup(&mut rng()).unwrap();
        let proof = prove(&pk, &[], 500_000, 0, &mut rng()).unwrap();
        assert!(verify_native(&vk, 500_000, 0, &proof).unwrap());
    }

    #[test]
    fn history_exceeding_period_limit_is_rejected_before_proving() {
        let (pk, _vk) = setup(&mut rng()).unwrap();
        let history = [
            HistoryEntry {
                amount: 400_000,
                period_index: 0,
            },
            HistoryEntry {
                amount: 400_000,
                period_index: 0,
            }, // 800k > 500k limit
        ];
        let result = prove(&pk, &history, 500_000, 800_000, &mut rng());
        assert!(matches!(result, Err(Error::PaymentExceedsLimit { .. })));
    }

    #[test]
    fn history_not_summing_to_claimed_total_is_rejected_before_proving() {
        let (pk, _vk) = setup(&mut rng()).unwrap();
        let history = [HistoryEntry {
            amount: 100_000,
            period_index: 0,
        }];
        let result = prove(&pk, &history, 500_000, 999_999, &mut rng());
        assert!(matches!(result, Err(Error::TotalMismatch { .. })));
    }

    #[test]
    fn proof_for_wrong_public_inputs_fails_verification() {
        let (pk, vk) = setup(&mut rng()).unwrap();
        let history = [HistoryEntry {
            amount: 100_000,
            period_index: 0,
        }];
        let proof = prove(&pk, &history, 500_000, 100_000, &mut rng()).unwrap();

        // Same proof, but checked against a different claimed total —
        // a tampered / mismatched claim must not verify.
        assert!(!verify_native(&vk, 500_000, 999_999, &proof).unwrap());
    }

    #[test]
    fn history_spanning_max_payments_verifies() {
        let (pk, vk) = setup(&mut rng()).unwrap();
        let history: Vec<HistoryEntry> = (0..MAX_PAYMENTS as u64)
            .map(|i| HistoryEntry {
                amount: 1_000,
                period_index: i,
            })
            .collect();
        let total = 1_000 * MAX_PAYMENTS as u128;

        let proof = prove(&pk, &history, 1_000, total, &mut rng()).unwrap();
        assert!(verify_native(&vk, 1_000, total, &proof).unwrap());
    }

    #[test]
    fn history_longer_than_max_payments_is_rejected() {
        let (pk, _vk) = setup(&mut rng()).unwrap();
        let history: Vec<HistoryEntry> = (0..(MAX_PAYMENTS as u64 + 1))
            .map(|i| HistoryEntry {
                amount: 1,
                period_index: i,
            })
            .collect();
        let result = prove(&pk, &history, 1_000, history.len() as u128, &mut rng());
        assert!(matches!(result, Err(Error::TooManyPayments { .. })));
    }
}
