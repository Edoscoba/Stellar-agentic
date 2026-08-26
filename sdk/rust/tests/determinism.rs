//! Cross-language determinism suite — Rust half.
//!
//! `fixtures/determinism.json` is generated from the **TypeScript**
//! implementation (`pnpm fixtures:generate`) and is consumed by three suites:
//! this one, `packages/core/src/math/__tests__/determinism-fixtures.test.ts`,
//! and `python/tests/test_determinism.py`. If all three pass, the three
//! implementations produce byte-identical strings for every case in the file.
//!
//! The TypeScript module exists to stop x86 and ARM disagreeing about a bid
//! score. A Rust port that quietly rounded differently would reintroduce
//! exactly that divergence for a mixed TS/Python/Rust agent ecosystem — only
//! now the implementations would disagree on *every* machine rather than some
//! of them. Hence: string equality, never numeric closeness. An
//! `assert!((a - b).abs() < f64::EPSILON` here would defeat the entire point
//! of the module under test.
//!
//! Two cases in the file are as important as the value cases: `throws: true`
//! entries assert that all three implementations *reject* the same inputs
//! (agreeing on values while disagreeing on which inputs are legal is still a
//! divergence), and the `invalidWeights` set does the same for bid scoring.

use std::path::PathBuf;

use num_bigint::BigInt;
use serde_json::Value;

use stellaragent::math::{
    bid as bid_mod, fixed_point as fp, AgentBid, BidWeights, FixedPointError,
};

// ─── Fixture loading ─────────────────────────────────────────────────────────

/// The fixtures live at the repo root so all three suites read the identical
/// file. `cargo test` runs with the crate root as the manifest dir, hence the
/// two levels up out of `sdk/rust`.
fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/determinism.json")
}

fn fixtures() -> Value {
    let path = fixtures_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "Shared fixtures missing at {}: {error}\n\
             Generate them from the TypeScript implementation: pnpm fixtures:generate",
            path.display()
        )
    });
    serde_json::from_str(&raw).expect("fixtures are valid JSON")
}

#[test]
fn fixture_file_is_populated() {
    // A silently empty fixture file would make every test below vacuous —
    // they would all iterate zero cases and report success.
    let f = fixtures();
    assert_eq!(f["version"], 1);
    assert!(f["fixedPoint"].as_array().unwrap().len() > 300);
    assert!(f["bid"]["scoreBid"].as_array().unwrap().len() > 100);
    assert!(f["bid"]["rankBids"].as_array().unwrap().len() > 10);
    assert!(f["bid"]["spendLimit"].as_array().unwrap().len() > 10);
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// A fixture argument, which the generator emits as either a JSON string or a
/// JSON number depending on whether it is a value or a decimal-place count.
fn as_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn as_places(value: &Value) -> u32 {
    u32::try_from(value.as_u64().expect("a decimal-place count is a number"))
        .expect("decimal places fit in u32")
}

/// Serialise a Rust result the way the generator serialised the TypeScript one.
///
/// `decimal` is `toFixed(18, ROUND_DOWN)`; `int` is a plain integer string;
/// `bool` is JavaScript's `String(true)`, which is `"true"` and not Rust's
/// `Debug` spelling. Getting this wrong would make the suite compare the wrong
/// two things and pass for the wrong reason.
enum Outcome {
    Value(String),
    Rejected(FixedPointError),
}

/// Evaluate one fixed-point case, dispatched by the TypeScript function name.
///
/// The mapping is explicit rather than derived from the name, exactly as the
/// TypeScript and Python suites do it, so a renamed or missing function fails
/// loudly here instead of silently skipping its cases.
fn eval_fixed_point(name: &str, args: &[Value], kind: &str) -> Outcome {
    let decimal = |result: Result<stellaragent::math::Decimal, FixedPointError>| match result {
        Ok(value) => Outcome::Value(value.to_fixed(18)),
        Err(error) => Outcome::Rejected(error),
    };
    let boolean = |result: Result<bool, FixedPointError>| match result {
        Ok(value) => Outcome::Value(if value { "true" } else { "false" }.to_string()),
        Err(error) => Outcome::Rejected(error),
    };
    let text = |result: Result<String, FixedPointError>| match result {
        Ok(value) => Outcome::Value(value),
        Err(error) => Outcome::Rejected(error),
    };

    let a = || as_str(&args[0]);
    let b = || as_str(&args[1]);

    let outcome = match name {
        "bn" => decimal(fp::bn(a())),
        "add" => decimal(fp::add(a(), b())),
        "sub" => decimal(fp::sub(a(), b())),
        "mul" => decimal(fp::mul(a(), b())),
        "div" => decimal(fp::div(a(), b())),
        "pct" => {
            let places = args.get(2).map_or(4, as_places);
            decimal(fp::pct(a(), b(), places))
        }
        "clamp" => decimal(fp::clamp(a(), b(), as_str(&args[2]))),
        "sumStrings" => decimal(fp::sum_strings(args.iter().map(as_str))),
        "toStroops" => match fp::to_stroops(a()) {
            Ok(value) => Outcome::Value(value.to_string()),
            Err(error) => Outcome::Rejected(error),
        },
        "fromStroops" => {
            let stroops: BigInt = a().parse().expect("stroop fixtures are integers");
            let places = args.get(1).map_or(7, as_places);
            text(fp::from_stroops(&stroops, places))
        }
        "fmt" => text(fp::fmt(a(), args.get(1).map_or(2, as_places))),
        "toStr" => text(fp::to_str(a(), args.get(1).map_or(7, as_places))),
        "gt" => boolean(fp::gt(a(), b())),
        "gte" => boolean(fp::gte(a(), b())),
        "lt" => boolean(fp::lt(a(), b())),
        "lte" => boolean(fp::lte(a(), b())),
        "eq" => boolean(fp::eq(a(), b())),
        "isZero" => boolean(fp::is_zero(a())),
        "isPositive" => boolean(fp::is_positive(a())),
        other => panic!(
            "fixture references `{other}`, which the Rust dispatch table does not cover. \
             Add it here rather than letting its cases go unasserted."
        ),
    };

    // The `kind` field is the generator's own record of how it serialised the
    // TypeScript result. Asserting the dispatch agrees catches a case whose
    // kind was edited without updating the arm that handles it.
    if let Outcome::Value(rendered) = &outcome {
        match kind {
            "decimal" => assert_eq!(
                rendered.matches('.').count(),
                1,
                "{name} rendered {rendered}"
            ),
            "bool" => assert!(matches!(rendered.as_str(), "true" | "false")),
            _ => {}
        }
    }
    outcome
}

fn weights(fixtures: &Value, name: &str) -> BidWeights {
    let raw = &fixtures["weightSets"][name];
    BidWeights {
        price: raw["price"].as_str().unwrap().to_string(),
        reputation: raw["reputation"].as_str().unwrap().to_string(),
        latency: raw["latency"].as_str().unwrap().to_string(),
        reliability: raw["reliability"].as_str().unwrap().to_string(),
    }
}

fn agent_bid(raw: &Value) -> AgentBid {
    AgentBid {
        agent_address: raw["agentAddress"].as_str().unwrap().to_string(),
        price: raw["price"].as_str().unwrap().to_string(),
        reputation: raw["reputation"].as_str().unwrap().to_string(),
        estimated_latency_seconds: raw["estimatedLatencySeconds"].as_str().unwrap().to_string(),
        success_rate: raw["successRate"].as_str().unwrap().to_string(),
    }
}

// ─── fixed-point parity ──────────────────────────────────────────────────────

#[test]
fn fixed_point_matches_typescript() {
    let f = fixtures();
    let cases = f["fixedPoint"].as_array().unwrap();
    let mut asserted = 0;

    for case in cases {
        if case.get("throws").is_some() {
            continue;
        }
        let id = case["id"].as_str().unwrap();
        let name = case["fn"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        let args = case["args"].as_array().unwrap();
        let expected = case["expect"].as_str().unwrap();

        match eval_fixed_point(name, args, kind) {
            Outcome::Value(actual) => assert_eq!(
                actual, expected,
                "{id}\n  TypeScript: {expected}\n  Rust:       {actual}\n  \
                 The two implementations have diverged — this breaks the determinism \
                 guarantee for any mixed TS/Python/Rust agent ecosystem."
            ),
            Outcome::Rejected(error) => {
                panic!("{id} was rejected by Rust but produced {expected} in TypeScript: {error}")
            }
        }
        asserted += 1;
    }

    assert!(asserted > 300, "only {asserted} value cases were asserted");
}

#[test]
fn fixed_point_rejects_what_typescript_rejects() {
    // Agreeing on values while disagreeing on which inputs are legal is still
    // a divergence: one SDK would ship a payment the other refuses to build.
    let f = fixtures();
    let mut asserted = 0;

    for case in f["fixedPoint"].as_array().unwrap() {
        if case.get("throws").is_none() {
            continue;
        }
        let id = case["id"].as_str().unwrap();
        let name = case["fn"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        let args = case["args"].as_array().unwrap();

        match eval_fixed_point(name, args, kind) {
            Outcome::Rejected(_) => asserted += 1,
            Outcome::Value(actual) => {
                panic!("{id} throws in TypeScript but Rust returned {actual}")
            }
        }
    }

    assert!(asserted > 0, "no throwing cases were exercised");
}

// ─── scoreBid parity ─────────────────────────────────────────────────────────

#[test]
fn score_bid_matches_typescript() {
    let f = fixtures();
    let cases = f["bid"]["scoreBid"].as_array().unwrap();

    for case in cases {
        let id = case["id"].as_str().unwrap();
        let scored = bid_mod::score_bid(
            &agent_bid(&case["bid"]),
            case["maxBid"].as_str().unwrap(),
            case["maxLatency"].as_str().unwrap(),
            &weights(&f, case["weights"].as_str().unwrap()),
        )
        .unwrap_or_else(|error| panic!("{id} failed to score: {error}"));

        let expected = &case["expect"];
        assert_eq!(
            scored.score,
            expected["score"].as_str().unwrap(),
            "{id} composite score"
        );

        let breakdown = &expected["breakdown"];
        assert_eq!(
            scored.breakdown.price_score,
            breakdown["priceScore"].as_str().unwrap(),
            "{id} priceScore"
        );
        assert_eq!(
            scored.breakdown.reputation_score,
            breakdown["reputationScore"].as_str().unwrap(),
            "{id} reputationScore"
        );
        assert_eq!(
            scored.breakdown.latency_score,
            breakdown["latencyScore"].as_str().unwrap(),
            "{id} latencyScore"
        );
        assert_eq!(
            scored.breakdown.reliability_score,
            breakdown["reliabilityScore"].as_str().unwrap(),
            "{id} reliabilityScore"
        );
    }

    assert!(cases.len() > 100);
}

// ─── rankBids parity ─────────────────────────────────────────────────────────

#[test]
fn rank_bids_matches_typescript() {
    let f = fixtures();

    for case in f["bid"]["rankBids"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let bids: Vec<AgentBid> = case["bids"]
            .as_array()
            .unwrap()
            .iter()
            .map(agent_bid)
            .collect();
        let ranked = bid_mod::rank_bids(&bids, &weights(&f, case["weights"].as_str().unwrap()))
            .unwrap_or_else(|error| panic!("{id} failed to rank: {error}"));

        let actual: Vec<(String, String)> = ranked
            .iter()
            .map(|r| (r.agent_address.clone(), r.score.clone()))
            .collect();
        let expected: Vec<(String, String)> = case["expect"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["agentAddress"].as_str().unwrap().to_string(),
                    e["score"].as_str().unwrap().to_string(),
                )
            })
            .collect();

        assert_eq!(
            actual, expected,
            "{id}\n  Ranking order or scores diverged — two agents scoring the same \
             pool would pick different winners."
        );
    }
}

#[test]
fn ranking_is_order_independent() {
    // Reversing the pool must not change the outcome, in any of the three
    // languages. Without the lexicographic tie-break this would fail on any
    // fixture containing equal scores.
    let f = fixtures();

    for case in f["bid"]["rankBids"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let bids: Vec<AgentBid> = case["bids"]
            .as_array()
            .unwrap()
            .iter()
            .map(agent_bid)
            .collect();
        let weights = weights(&f, case["weights"].as_str().unwrap());

        let forward = bid_mod::rank_bids(&bids, &weights).unwrap();
        let mut reversed = bids.clone();
        reversed.reverse();
        let backward = bid_mod::rank_bids(&reversed, &weights).unwrap();

        assert_eq!(forward, backward, "{id} is sensitive to input order");
    }
}

#[test]
fn select_best_bid_agrees_with_the_ranking() {
    let f = fixtures();

    for case in f["bid"]["rankBids"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let bids: Vec<AgentBid> = case["bids"]
            .as_array()
            .unwrap()
            .iter()
            .map(agent_bid)
            .collect();
        let weights = weights(&f, case["weights"].as_str().unwrap());
        let best = bid_mod::select_best_bid(&bids, &weights).unwrap();
        let expected = case["expect"].as_array().unwrap();

        match expected.first() {
            None => assert!(best.is_none(), "{id} should have no winner"),
            Some(head) => {
                let best = best.unwrap_or_else(|| panic!("{id} should have a winner"));
                assert_eq!(best.agent_address, head["agentAddress"].as_str().unwrap());
                assert_eq!(best.score, head["score"].as_str().unwrap());
            }
        }
    }
}

// ─── Spend limits parity ─────────────────────────────────────────────────────

#[test]
fn spend_limits_match_typescript() {
    let f = fixtures();

    for case in f["bid"]["spendLimit"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let spent = case["spent"].as_str().unwrap();
        let limit = case["limit"].as_str().unwrap();
        let amount = case["amount"].as_str().unwrap();

        let within = bid_mod::is_within_spend_limit(spent, limit, amount).unwrap();
        let remaining = bid_mod::remaining_budget(spent, limit).unwrap();

        assert_eq!(
            within,
            case["expect"]["withinLimit"].as_bool().unwrap(),
            "{id} isWithinSpendLimit — a disagreement here means one \
             implementation would allow a payment the other blocks"
        );
        assert_eq!(
            remaining,
            case["expect"]["remaining"].as_str().unwrap(),
            "{id} remainingBudget"
        );
    }
}

// ─── Invalid weights parity ──────────────────────────────────────────────────

#[test]
fn invalid_weights_are_rejected() {
    let f = fixtures();
    let sample = AgentBid {
        agent_address: "GTEST".into(),
        price: "1".into(),
        reputation: "50".into(),
        estimated_latency_seconds: "10".into(),
        success_rate: "0.5".into(),
    };

    for case in f["bid"]["invalidWeights"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let raw = &case["weights"];
        let weights = BidWeights {
            price: raw["price"].as_str().unwrap().to_string(),
            reputation: raw["reputation"].as_str().unwrap().to_string(),
            latency: raw["latency"].as_str().unwrap().to_string(),
            reliability: raw["reliability"].as_str().unwrap().to_string(),
        };

        let error = bid_mod::score_bid(&sample, "10", "10", &weights)
            .expect_err(&format!("{id} should be rejected"));
        assert!(
            matches!(error, FixedPointError::InvalidWeights { .. }),
            "{id} produced {error} rather than a weight-sum rejection"
        );
        assert!(error.to_string().contains("weights must sum to 1.0"));
    }
}
