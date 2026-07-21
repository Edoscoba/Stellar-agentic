//! End-to-end acceptance test: a real payment history flows through the
//! actual `PaymentChannel` Soroban contract, a real Groth16 proof is
//! generated for it off-chain, and the *actual on-chain*
//! `PaymentChannel::verify_solvency_proof` — using Soroban's native
//! BLS12-381 `pairing_check` host function, not a mock — accepts it. A
//! tampered proof is demonstrably rejected the same way.
//!
//! This is the strongest possible correctness check for the
//! arkworks<->Soroban byte-encoding bridge: if the encoding were wrong in
//! any way, a genuinely valid proof would fail this real on-chain pairing
//! check, not just an internal self-consistency check.

use ark_std::rand::rngs::StdRng;
use ark_std::rand::SeedableRng;
use payment_channel::{
    PaymentChannel, PaymentChannelClient, SolvencyProof, SolvencyVerifyingKey, SpendPeriod,
};
use solvency_proof::{proof_to_soroban_bytes, prove, setup, vk_to_soroban_bytes, HistoryEntry};
use soroban_sdk::{testutils::Address as _, token, Address, Bytes, BytesN, Env, Vec};

fn rng() -> StdRng {
    StdRng::seed_from_u64(1)
}

/// Registers PaymentChannel, wires up a token, opens a channel, and drives
/// `limit_per_period` real payments through it so the channel's own
/// on-chain `total_spent` is authentic (produced by the contract's normal
/// accounting, not asserted by the test).
struct Harness {
    env: Env,
    channel: PaymentChannelClient<'static>,
    channel_id: u64,
}

fn setup_channel(limit_per_period: i128, deposit: i128) -> Harness {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let agent = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token = token_contract.address();
    token::StellarAssetClient::new(&env, &token).mint(&owner, &deposit);

    let channel_id_pkg = env.register(PaymentChannel, ());
    let channel = PaymentChannelClient::new(&env, &channel_id_pkg);

    let channel_id = channel.open_channel(
        &owner,
        &agent,
        &token,
        &deposit,
        &limit_per_period,
        &SpendPeriod::Daily,
    );

    Harness {
        env,
        channel,
        channel_id,
    }
}

fn spend(h: &Harness, agent: &Address, recipient: &Address, amount: i128) {
    h.channel.pay(
        agent,
        &h.channel_id,
        recipient,
        &amount,
        &Bytes::new(&h.env),
    );
}

fn install_vk(
    h: &Harness,
    admin: &Address,
    vk: &ark_groth16::VerifyingKey<ark_bls12_381::Bls12_381>,
) {
    let sv = vk_to_soroban_bytes(vk);
    let onchain_vk = SolvencyVerifyingKey {
        alpha_g1: soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(
            &h.env,
            &sv.alpha_g1,
        )),
        beta_g2: soroban_sdk::crypto::bls12_381::G2Affine::from_bytes(BytesN::from_array(
            &h.env,
            &sv.beta_g2,
        )),
        gamma_g2: soroban_sdk::crypto::bls12_381::G2Affine::from_bytes(BytesN::from_array(
            &h.env,
            &sv.gamma_g2,
        )),
        delta_g2: soroban_sdk::crypto::bls12_381::G2Affine::from_bytes(BytesN::from_array(
            &h.env,
            &sv.delta_g2,
        )),
        gamma_abc_g1: Vec::from_array(
            &h.env,
            [
                soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(
                    &h.env,
                    &sv.gamma_abc_g1[0],
                )),
                soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(
                    &h.env,
                    &sv.gamma_abc_g1[1],
                )),
                soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(
                    &h.env,
                    &sv.gamma_abc_g1[2],
                )),
            ],
        ),
    };
    h.channel.set_solvency_vk(admin, &onchain_vk);
}

fn proof_contracttype(
    h: &Harness,
    proof: &ark_groth16::Proof<ark_bls12_381::Bls12_381>,
) -> SolvencyProof {
    let sp = proof_to_soroban_bytes(proof);
    SolvencyProof {
        a: soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(&h.env, &sp.a)),
        b: soroban_sdk::crypto::bls12_381::G2Affine::from_bytes(BytesN::from_array(&h.env, &sp.b)),
        c: soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(&h.env, &sp.c)),
    }
}

#[test]
fn valid_proof_is_accepted_by_the_real_on_chain_verifier() {
    let limit_per_period = 500_000i128;
    let h = setup_channel(limit_per_period, 10_000_000);

    let agent = h.channel.get_channel(&h.channel_id).agent;
    let recipient = Address::generate(&h.env);

    // Real payments, through the real contract. total_spent below is read
    // back from the contract afterward, not assumed.
    spend(&h, &agent, &recipient, 100_000);
    spend(&h, &agent, &recipient, 200_000);
    let channel = h.channel.get_channel(&h.channel_id);
    let total_spent = channel.total_spent as u128;
    assert_eq!(total_spent, 300_000);

    let (pk, vk) = setup(&mut rng()).unwrap();
    let admin = Address::generate(&h.env);
    install_vk(&h, &admin, &vk);

    // The channel owner's own private record of what was actually spent —
    // this is the data the proof keeps confidential.
    let history = [
        HistoryEntry {
            amount: 100_000,
            period_index: 0,
        },
        HistoryEntry {
            amount: 200_000,
            period_index: 0,
        },
    ];
    let proof = prove(
        &pk,
        &history,
        limit_per_period as u128,
        total_spent,
        &mut rng(),
    )
    .unwrap();
    let onchain_proof = proof_contracttype(&h, &proof);

    assert!(h
        .channel
        .verify_solvency_proof(&h.channel_id, &onchain_proof));
}

#[test]
fn proof_for_a_different_total_spent_is_rejected() {
    let limit_per_period = 500_000i128;
    let h = setup_channel(limit_per_period, 10_000_000);

    let agent = h.channel.get_channel(&h.channel_id).agent;
    let recipient = Address::generate(&h.env);
    spend(&h, &agent, &recipient, 300_000);
    let channel = h.channel.get_channel(&h.channel_id);
    let total_spent = channel.total_spent as u128;

    let (pk, vk) = setup(&mut rng()).unwrap();
    let admin = Address::generate(&h.env);
    install_vk(&h, &admin, &vk);

    // Proof for a fabricated history that sums to a different total than
    // what the channel actually recorded — the on-chain caller always
    // reads the channel's own total_spent, so this proof (even though it
    // is valid *for its own claimed total*) must fail here.
    let fabricated_history = [HistoryEntry {
        amount: 300_000,
        period_index: 0,
    }];
    let wrong_total = total_spent + 1;
    let proof = prove(
        &pk,
        &fabricated_history,
        limit_per_period as u128,
        wrong_total,
        &mut rng(),
    );
    // Our own prover refuses to even build this proof, since the fabricated
    // total doesn't match the history — construct it a different way: prove
    // correctly for `wrong_total` using a *consistent* fabricated history,
    // to isolate what we're actually testing (total mismatch vs. channel).
    assert!(
        proof.is_err(),
        "prove() should refuse an inconsistent (history, total) pair"
    );

    let consistent_history = [HistoryEntry {
        amount: wrong_total,
        period_index: 0,
    }];
    let proof = prove(
        &pk,
        &consistent_history,
        limit_per_period as u128,
        wrong_total,
        &mut rng(),
    )
    .unwrap();
    let onchain_proof = proof_contracttype(&h, &proof);

    // Valid proof, but for the wrong channel state — on-chain verification
    // reads the channel's real total_spent, which doesn't match.
    assert!(!h
        .channel
        .verify_solvency_proof(&h.channel_id, &onchain_proof));
}

#[test]
fn proof_with_a_component_swapped_from_another_proof_is_rejected() {
    // A well-formed-but-wrong proof: swap in the `c` component from a
    // *different* valid proof. Every point is still genuinely on the
    // curve (so this exercises the pairing-equation mismatch path, not
    // the malformed-input path below), but the combination no longer
    // satisfies Groth16's verification equation.
    let limit_per_period = 500_000i128;
    let h = setup_channel(limit_per_period, 10_000_000);

    let agent = h.channel.get_channel(&h.channel_id).agent;
    let recipient = Address::generate(&h.env);
    spend(&h, &agent, &recipient, 300_000);
    let total_spent = h.channel.get_channel(&h.channel_id).total_spent as u128;

    let (pk, vk) = setup(&mut rng()).unwrap();
    let admin = Address::generate(&h.env);
    install_vk(&h, &admin, &vk);

    let history = [HistoryEntry {
        amount: 300_000,
        period_index: 0,
    }];
    let proof = prove(
        &pk,
        &history,
        limit_per_period as u128,
        total_spent,
        &mut rng(),
    )
    .unwrap();
    let other_history = [HistoryEntry {
        amount: 111_000,
        period_index: 0,
    }];
    let other_proof = prove(
        &pk,
        &other_history,
        limit_per_period as u128,
        111_000,
        &mut rng(),
    )
    .unwrap();

    let sp = proof_to_soroban_bytes(&proof);
    let other_sp = proof_to_soroban_bytes(&other_proof);
    let frankensteined = SolvencyProof {
        a: soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(&h.env, &sp.a)),
        b: soroban_sdk::crypto::bls12_381::G2Affine::from_bytes(BytesN::from_array(&h.env, &sp.b)),
        c: soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(
            &h.env,
            &other_sp.c,
        )),
    };

    assert!(!h
        .channel
        .verify_solvency_proof(&h.channel_id, &frankensteined));
}

#[test]
#[should_panic(expected = "InvalidInput")]
fn proof_with_malformed_off_curve_bytes_traps() {
    // Genuinely garbage bytes (not just "the wrong proof", but bytes that
    // don't decode to any point on the curve at all) aren't something
    // `pairing_check` returns `false` for — Soroban's host validates its
    // curve-point inputs and traps the whole transaction on malformed
    // input, same as it would for e.g. malformed XDR. That's still a
    // secure rejection (the call never succeeds), just via panic instead
    // of a boolean, so callers should expect `verify_solvency_proof` can
    // trap on corrupted input, not only return `false`.
    let limit_per_period = 500_000i128;
    let h = setup_channel(limit_per_period, 10_000_000);

    let agent = h.channel.get_channel(&h.channel_id).agent;
    let recipient = Address::generate(&h.env);
    spend(&h, &agent, &recipient, 300_000);
    let total_spent = h.channel.get_channel(&h.channel_id).total_spent as u128;

    let (pk, vk) = setup(&mut rng()).unwrap();
    let admin = Address::generate(&h.env);
    install_vk(&h, &admin, &vk);

    let history = [HistoryEntry {
        amount: 300_000,
        period_index: 0,
    }];
    let proof = prove(
        &pk,
        &history,
        limit_per_period as u128,
        total_spent,
        &mut rng(),
    )
    .unwrap();
    let mut sp = proof_to_soroban_bytes(&proof);
    sp.a[10] ^= 0xFF; // no longer decodes to a point on the curve

    let corrupted = SolvencyProof {
        a: soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(&h.env, &sp.a)),
        b: soroban_sdk::crypto::bls12_381::G2Affine::from_bytes(BytesN::from_array(&h.env, &sp.b)),
        c: soroban_sdk::crypto::bls12_381::G1Affine::from_bytes(BytesN::from_array(&h.env, &sp.c)),
    };

    h.channel.verify_solvency_proof(&h.channel_id, &corrupted);
}

#[test]
fn payment_still_works_normally_alongside_solvency_proofs() {
    // Wiring up solvency proofs shouldn't change PaymentChannel's core
    // spend-limit behavior at all.
    let limit_per_period = 500_000i128;
    let h = setup_channel(limit_per_period, 10_000_000);
    let agent = h.channel.get_channel(&h.channel_id).agent;
    let recipient = Address::generate(&h.env);

    spend(&h, &agent, &recipient, 500_000);
    let channel = h.channel.get_channel(&h.channel_id);
    assert_eq!(channel.total_spent, 500_000);
}
