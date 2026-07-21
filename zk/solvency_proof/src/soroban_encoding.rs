//! Encodes arkworks BLS12-381 points/scalars into the exact byte layout
//! Soroban's native `crypto().bls12_381()` host functions expect, and back.
//!
//! Soroban and arkworks agree on the curve (BLS12-381) but not on wire
//! format: arkworks' `CanonicalSerialize` uses little-endian, Montgomery-form
//! bytes by default, while Soroban follows the ZCash/IETF convention —
//! **uncompressed**, **big-endian** coordinates, with three flag bits packed
//! into the top of the first byte (see `soroban_sdk::crypto::bls12_381`'s
//! doc comments for the exact layout). Feeding arkworks' own serialization
//! straight into a Soroban host function would silently produce a different
//! (or invalid) point, so this module hand-rolls the conversion instead of
//! relying on either side's default (de)serializer.
//!
//! Correctness is checked in tests against the known-answer G1 generator
//! vector published in `soroban_sdk`'s own rustdoc, plus full pairing-check
//! round trips through a real Soroban contract (see `tests/`).

use ark_bls12_381::{Fq, Fr, G1Affine, G2Affine};
use ark_ff::{BigInteger, PrimeField};

pub const FP_SIZE: usize = 48;
pub const FP2_SIZE: usize = FP_SIZE * 2;
pub const G1_SIZE: usize = FP_SIZE * 2;
pub const G2_SIZE: usize = FP2_SIZE * 2;
pub const FR_SIZE: usize = 32;

const INFINITY_FLAG: u8 = 0x40;

fn fq_to_be_bytes(f: &Fq) -> [u8; FP_SIZE] {
    let bytes = f.into_bigint().to_bytes_be();
    let mut out = [0u8; FP_SIZE];
    out.copy_from_slice(&bytes);
    out
}

fn fq_from_be_bytes(bytes: &[u8]) -> Fq {
    Fq::from_be_bytes_mod_order(bytes)
}

/// Serializes a G1 point as `be(X) || be(Y)`, 96 bytes, matching
/// `soroban_sdk::crypto::bls12_381::G1Affine`'s documented encoding.
pub fn g1_to_soroban_bytes(p: &G1Affine) -> [u8; G1_SIZE] {
    let mut out = [0u8; G1_SIZE];
    if p.infinity {
        out[0] = INFINITY_FLAG;
        return out;
    }
    out[0..FP_SIZE].copy_from_slice(&fq_to_be_bytes(&p.x));
    out[FP_SIZE..G1_SIZE].copy_from_slice(&fq_to_be_bytes(&p.y));
    out
}

/// Inverse of [`g1_to_soroban_bytes`]. Used only for round-trip tests — the
/// prover never needs to decode points it just produced.
pub fn g1_from_soroban_bytes(bytes: &[u8; G1_SIZE]) -> G1Affine {
    if bytes[0] & INFINITY_FLAG != 0 {
        return G1Affine::identity();
    }
    let x = fq_from_be_bytes(&bytes[0..FP_SIZE]);
    let y = fq_from_be_bytes(&bytes[FP_SIZE..G1_SIZE]);
    G1Affine::new(x, y)
}

/// Serializes a G2 point as `be(X_c1) || be(X_c0) || be(Y_c1) || be(Y_c0)`,
/// 192 bytes — note the c1-before-c0 order, matching Soroban's documented
/// encoding (the opposite of arkworks' internal `c0, c1` field order).
pub fn g2_to_soroban_bytes(p: &G2Affine) -> [u8; G2_SIZE] {
    let mut out = [0u8; G2_SIZE];
    if p.infinity {
        out[0] = INFINITY_FLAG;
        return out;
    }
    out[0..FP_SIZE].copy_from_slice(&fq_to_be_bytes(&p.x.c1));
    out[FP_SIZE..FP2_SIZE].copy_from_slice(&fq_to_be_bytes(&p.x.c0));
    out[FP2_SIZE..FP2_SIZE + FP_SIZE].copy_from_slice(&fq_to_be_bytes(&p.y.c1));
    out[FP2_SIZE + FP_SIZE..G2_SIZE].copy_from_slice(&fq_to_be_bytes(&p.y.c0));
    out
}

/// Inverse of [`g2_to_soroban_bytes`]. Used only for round-trip tests.
pub fn g2_from_soroban_bytes(bytes: &[u8; G2_SIZE]) -> G2Affine {
    use ark_ff::Fp2;
    if bytes[0] & INFINITY_FLAG != 0 {
        return G2Affine::identity();
    }
    let x_c1 = fq_from_be_bytes(&bytes[0..FP_SIZE]);
    let x_c0 = fq_from_be_bytes(&bytes[FP_SIZE..FP2_SIZE]);
    let y_c1 = fq_from_be_bytes(&bytes[FP2_SIZE..FP2_SIZE + FP_SIZE]);
    let y_c0 = fq_from_be_bytes(&bytes[FP2_SIZE + FP_SIZE..G2_SIZE]);
    G2Affine::new(Fp2::new(x_c0, x_c1), Fp2::new(y_c0, y_c1))
}

/// Serializes a scalar field element as 32 big-endian bytes, matching
/// `soroban_sdk::crypto::bls12_381::Fr` (which wraps a `U256`).
pub fn fr_to_soroban_bytes(f: &Fr) -> [u8; FR_SIZE] {
    let bytes = f.into_bigint().to_bytes_be();
    let mut out = [0u8; FR_SIZE];
    out.copy_from_slice(&bytes);
    out
}

#[cfg(test)]
mod test {
    use super::*;
    use ark_ec::AffineRepr;

    // Known-answer test lifted directly from `soroban_sdk::crypto::bls12_381`'s
    // own rustdoc example for the G1 generator ("one"). If our hand-rolled
    // encoder disagrees with this, it disagrees with Soroban.
    const SOROBAN_G1_GENERATOR_HEX: &str = "17f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb08b3f481e3aaa0f1a09e30ed741d8ae4fcf5e095d5d00af600db18cb2c04b3edd03cc744a2888ae40caa232946c5e7e1";

    #[test]
    fn g1_generator_matches_soroban_known_answer() {
        let expected = hex::decode(SOROBAN_G1_GENERATOR_HEX).unwrap();
        let ours = g1_to_soroban_bytes(&G1Affine::generator());
        assert_eq!(ours.to_vec(), expected);
    }

    #[test]
    fn g1_round_trips_through_soroban_encoding() {
        let p = G1Affine::generator();
        let bytes = g1_to_soroban_bytes(&p);
        assert_eq!(g1_from_soroban_bytes(&bytes), p);
    }

    #[test]
    fn g2_round_trips_through_soroban_encoding() {
        let p = G2Affine::generator();
        let bytes = g2_to_soroban_bytes(&p);
        assert_eq!(g2_from_soroban_bytes(&bytes), p);
    }

    #[test]
    fn g1_infinity_encodes_to_flag_only() {
        let bytes = g1_to_soroban_bytes(&G1Affine::identity());
        assert_eq!(bytes[0], INFINITY_FLAG);
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }
}
