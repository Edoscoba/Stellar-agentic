//! CLI for generating and checking PaymentChannel solvency proofs.
//!
//! ```text
//! solvency-prover setup --out-dir ./keys
//! solvency-prover prove --pk ./keys/pk.bin --history history.json \
//!     --limit 500000 --total 750000 --out proof.json
//! solvency-prover verify --vk ./keys/vk.bin --proof proof.json
//! ```
//!
//! `history.json` is a JSON array of `{"amount": <u128>, "period_index": <u64>}`
//! — the channel owner's own private record of what they actually spent.
//! `proof.json` and the `vk_soroban.json` written by `setup` are both
//! ready to feed into `PaymentChannel::verify_solvency_proof` /
//! `set_solvency_vk` (hex-encoded Soroban BLS12-381 bytes).

use ark_bls12_381::Bls12_381;
use ark_groth16::{Proof, ProvingKey, VerifyingKey};
use ark_std::rand::SeedableRng;
use serde::{Deserialize, Serialize};
use solvency_proof::soroban_encoding::{
    g1_from_soroban_bytes, g2_from_soroban_bytes, G1_SIZE, G2_SIZE,
};
use solvency_proof::{
    deserialize_canonical, proof_to_soroban_bytes, prove, serialize_canonical, setup,
    verify_native, vk_to_soroban_bytes, HistoryEntry,
};
use std::collections::HashMap;
use std::fs;
use std::process::ExitCode;

fn parse_flags(args: &[String]) -> HashMap<String, String> {
    let mut flags = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(key) = args[i].strip_prefix("--") {
            let value = args.get(i + 1).cloned().unwrap_or_default();
            flags.insert(key.to_string(), value);
            i += 2;
        } else {
            i += 1;
        }
    }
    flags
}

#[derive(Serialize, Deserialize)]
struct HistoryEntryJson {
    amount: u128,
    period_index: u64,
}

#[derive(Serialize)]
struct SorobanVkJson {
    alpha_g1: String,
    beta_g2: String,
    gamma_g2: String,
    delta_g2: String,
    gamma_abc_g1: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct SorobanProofJson {
    a: String,
    b: String,
    c: String,
    limit_per_period: u128,
    total_spent: u128,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(command) = args.get(1) else {
        eprintln!("usage: solvency-prover <setup|prove|verify> [--flags...]");
        return ExitCode::FAILURE;
    };
    let flags = parse_flags(&args[2..]);

    let result = match command.as_str() {
        "setup" => cmd_setup(&flags),
        "prove" => cmd_prove(&flags),
        "verify" => cmd_verify(&flags),
        other => Err(format!("unknown command '{other}'")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_setup(flags: &HashMap<String, String>) -> Result<(), String> {
    let out_dir = flags.get("out-dir").ok_or("missing --out-dir")?;
    fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;

    eprintln!(
        "WARNING: this is a toy, single-party Groth16 setup, not a real trusted-setup \
         ceremony. Do not use these keys for anything beyond local testing/demos. \
         See docs/zk-solvency-design.md."
    );

    let mut rng = ark_std::rand::rngs::StdRng::from_entropy();
    let (pk, vk) = setup(&mut rng).map_err(|e| format!("{e:?}"))?;

    fs::write(format!("{out_dir}/pk.bin"), serialize_canonical(&pk)).map_err(|e| e.to_string())?;
    fs::write(format!("{out_dir}/vk.bin"), serialize_canonical(&vk)).map_err(|e| e.to_string())?;

    let sv = vk_to_soroban_bytes(&vk);
    let vk_json = SorobanVkJson {
        alpha_g1: hex::encode(sv.alpha_g1),
        beta_g2: hex::encode(sv.beta_g2),
        gamma_g2: hex::encode(sv.gamma_g2),
        delta_g2: hex::encode(sv.delta_g2),
        gamma_abc_g1: sv.gamma_abc_g1.iter().map(hex::encode).collect(),
    };
    fs::write(
        format!("{out_dir}/vk_soroban.json"),
        serde_json::to_string_pretty(&vk_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    eprintln!("wrote {out_dir}/pk.bin, {out_dir}/vk.bin, {out_dir}/vk_soroban.json");
    Ok(())
}

fn cmd_prove(flags: &HashMap<String, String>) -> Result<(), String> {
    let pk_path = flags.get("pk").ok_or("missing --pk")?;
    let history_path = flags.get("history").ok_or("missing --history")?;
    let limit: u128 = flags
        .get("limit")
        .ok_or("missing --limit")?
        .parse()
        .map_err(|_| "invalid --limit")?;
    let total: u128 = flags
        .get("total")
        .ok_or("missing --total")?
        .parse()
        .map_err(|_| "invalid --total")?;
    let out_path = flags.get("out").ok_or("missing --out")?;

    let pk_bytes = fs::read(pk_path).map_err(|e| e.to_string())?;
    let pk: ProvingKey<Bls12_381> = deserialize_canonical(&pk_bytes).map_err(|e| e.to_string())?;

    let history_json = fs::read_to_string(history_path).map_err(|e| e.to_string())?;
    let entries: Vec<HistoryEntryJson> =
        serde_json::from_str(&history_json).map_err(|e| e.to_string())?;
    let history: Vec<HistoryEntry> = entries
        .into_iter()
        .map(|e| HistoryEntry {
            amount: e.amount,
            period_index: e.period_index,
        })
        .collect();

    let mut rng = ark_std::rand::rngs::StdRng::from_entropy();
    let proof = prove(&pk, &history, limit, total, &mut rng).map_err(|e| format!("{e:?}"))?;

    let sp = proof_to_soroban_bytes(&proof);
    let proof_json = SorobanProofJson {
        a: hex::encode(sp.a),
        b: hex::encode(sp.b),
        c: hex::encode(sp.c),
        limit_per_period: limit,
        total_spent: total,
    };
    fs::write(
        out_path,
        serde_json::to_string_pretty(&proof_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    eprintln!("wrote {out_path}");
    Ok(())
}

fn cmd_verify(flags: &HashMap<String, String>) -> Result<(), String> {
    let vk_path = flags.get("vk").ok_or("missing --vk")?;
    let proof_path = flags.get("proof").ok_or("missing --proof")?;

    let vk_bytes = fs::read(vk_path).map_err(|e| e.to_string())?;
    let vk: VerifyingKey<Bls12_381> =
        deserialize_canonical(&vk_bytes).map_err(|e| e.to_string())?;

    let proof_json = fs::read_to_string(proof_path).map_err(|e| e.to_string())?;
    let sp: SorobanProofJson = serde_json::from_str(&proof_json).map_err(|e| e.to_string())?;

    // Decode the same Soroban-hex bytes that would be submitted on-chain,
    // through the same bridge `verify_solvency_proof` relies on, so this
    // command is checking exactly what would be submitted — not a
    // separately-trusted copy of the proof.
    let a: [u8; G1_SIZE] = decode_fixed(&sp.a)?;
    let b: [u8; G2_SIZE] = decode_fixed(&sp.b)?;
    let c: [u8; G1_SIZE] = decode_fixed(&sp.c)?;
    let proof = Proof::<Bls12_381> {
        a: g1_from_soroban_bytes(&a),
        b: g2_from_soroban_bytes(&b),
        c: g1_from_soroban_bytes(&c),
    };

    let ok = verify_native(&vk, sp.limit_per_period, sp.total_spent, &proof)
        .map_err(|e| format!("{e:?}"))?;

    println!("{}", if ok { "VALID" } else { "INVALID" });
    if !ok {
        return Err("proof did not verify".to_string());
    }
    Ok(())
}

fn decode_fixed<const N: usize>(hex_str: &str) -> Result<[u8; N], String> {
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("expected {N} bytes, got {}", v.len()))
}
