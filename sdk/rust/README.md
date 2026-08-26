# StellarAgent — Rust SDK

> **AI Agent Payment Rails on Stellar.**
> The Rust counterpart of [`@stellaragent/core`](../../packages/core) and
> [`stellaragent`](../../python) (Python).

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange)](https://www.rust-lang.org)

---

## Why a third SDK

Agent infrastructure written in Rust — and anything already embedding the
Soroban tooling in this repo — currently has to shell out to the TypeScript SDK
or re-implement the protocol. This crate removes that.

It also earns its keep for a reason that has nothing to do with Rust: the
determinism guarantee is this project's headline claim, and it was previously
proven across exactly **two** implementations. Two implementations can agree by
sharing an assumption neither of them ever wrote down. A third is the real test
of whether `fixtures/determinism.json` actually pins the semantics.

It does. Every one of the 388 fixed-point cases, 210 bid scores, 24 rankings
and 16 spend-limit checks in that file matched byte-for-byte, and the
`Determinism` CI job now covers three languages.

---

## Install

```toml
[dependencies]
stellaragent = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

From a checkout:

```bash
cd sdk/rust
cargo test
```

---

## Quick Start

The same worked example as the [TypeScript](../../README.md#quick-start) and
[Python](../../python/README.md#quick-start) SDKs.

```rust
use stellaragent::contracts::ContractKey;
use stellaragent::types::{
    Network, OpenChannelParams, PayForApiParams, RequestWorkParams, SpendPeriod,
};
use stellaragent::StellarAgent;

#[tokio::main]
async fn main() -> Result<(), stellaragent::StellarAgentError> {
    let agent = StellarAgent::builder()
        .network(Network::Testnet)
        .secret_key(std::env::var("AGENT_SECRET").unwrap())
        .contract(ContractKey::PaymentChannel, "C...")
        .asset_contract("USDC", "C...")
        .build()
        .await?;

    // Fund a channel and cap the agent at 10 USDC per hour, enforced on-chain.
    agent
        .open_channel(&OpenChannelParams {
            deposit: "100".into(),
            limit_per_period: "10".into(),
            period: SpendPeriod::Hourly,
            token: Some("USDC".into()),
        })
        .await?;

    // Pay for an API call.
    agent
        .pay_for_api(&PayForApiParams {
            endpoint: "https://api.example.com/inference".into(),
            amount: "0.001".into(),
            asset: Some("USDC".into()),
            ..Default::default()
        })
        .await?;

    // Agent-to-agent escrow job.
    let job_id = agent
        .request_work(&RequestWorkParams {
            worker_agent: "G...AGENT_ADDRESS".into(),
            task: "Summarize this document".into(),
            escrow_amount: "0.05".into(),
            asset: Some("USDC".into()),
            ..Default::default()
        })
        .await?;
    println!("escrow job {job_id} created");

    Ok(())
}
```

---

## Keys stay out of your process

`secret_key` is fine for testnet. For anything holding real funds, hand the
agent a [`Signer`] instead — the secret then lives behind a network boundary
and this process holds only a URL and a token:

```rust
use std::sync::Arc;

use stellaragent::signer::{RemoteSigner, RemoteSignerOptions};
use stellaragent::{types::Network, StellarAgent};

#[tokio::main]
async fn main() -> Result<(), stellaragent::StellarAgentError> {
    let signer = RemoteSigner::new(
        RemoteSignerOptions::new("https://signer.internal")
            .token(std::env::var("SIGNER_TOKEN").unwrap())
            // Refuse to continue if the service signs for a different account.
            .expect_public_key("GAGENT..."),
    )?;

    let agent = StellarAgent::builder()
        .network(Network::Mainnet)
        .signer(Arc::new(signer))
        .build()
        .await?;

    println!("agent {} ready", agent.address());
    Ok(())
}
```

`Signer` is a trait — implement it over a KMS client, an HSM, or a SEP-43
wallet. The interface is base64 XDR in, signed base64 XDR out, which is the
narrowest boundary that never requires key material to cross it. See
[`signer`](src/signer.rs) for why a *remote service* rather than a hardware
wallet is the right shape for an unattended agent.

[`Signer`]: src/signer.rs

---

## Deterministic math

The reason this crate exists in the form it does. Bid scores and spend
calculations must come out byte-identical in all three languages, or two agents
scoring the same pool of bids will disagree about the winner.

```rust
use stellaragent::math::{rank_bids, AgentBid, BidWeights};

let bids = vec![
    AgentBid {
        agent_address: "GFAST".into(),
        price: "0.05".into(),
        reputation: "80".into(),
        estimated_latency_seconds: "2".into(),
        success_rate: "0.95".into(),
    },
    AgentBid {
        agent_address: "GCHEAP".into(),
        price: "0.01".into(),
        reputation: "60".into(),
        estimated_latency_seconds: "30".into(),
        success_rate: "0.99".into(),
    },
];

let ranked = rank_bids(&bids, &BidWeights::default()).unwrap();
assert_eq!(ranked[0].agent_address, "GFAST");
```

Predict whether a payment would be blocked, without an RPC round trip or a fee:

```rust
use stellaragent::math::{predict_payment_outcome, BlockReason, ChannelSpendState, PredictPaymentParams};
use stellaragent::types::SpendPeriod;

let channel = ChannelSpendState {
    active: true,
    limit_per_period: "10000".into(),
    spent_this_period: "9900".into(),
    period_start_ledger: 1000,
    period: SpendPeriod::Hourly,
};

let prediction = predict_payment_outcome(PredictPaymentParams {
    channel_state: Some(&channel),
    rate_limit_state: None,
    amount: "200",
    current_ledger: 1500,
})
.unwrap();
assert_eq!(prediction.reasons, vec![BlockReason::ChannelSpendLimit]);
```

This replicates the contracts' own reset-then-check window semantics, so it
does not predict a block the chain will not enforce once a period has rolled
over.

### Floating point is a compile error, not a convention

There is no `impl IntoDecimal for f64`. A float reaching a monetary or score
calculation reintroduces exactly the cross-platform divergence this module
exists to prevent — `0.1f64` is not `0.1` — so the type system refuses it
rather than a comment asking nicely. Pass decimal strings.

The single exception is `math::ledger_time`, which returns `f64` because what
it produces is an *estimate* for human display ("resets in ~4 minutes") and
never feeds a payment decision.

---

## Module map

| Module | What lives there |
| --- | --- |
| `math` | Fixed-point arithmetic, bid scoring, spend prediction, ledger-time estimates |
| `client` | `StellarAgent`: build, simulate, sign, submit, poll, and the operations on top |
| `rpc` | The Soroban RPC transport |
| `scval` | `ScVal` encoding and decoding for every contract type |
| `signer` | The `Signer` trait, an in-memory keypair, and a remote signing service |
| `contracts` | Contract-address resolution and its deployed-or-not check |
| `types` | Shared data types, carrying the TypeScript SDK's JSON shape |
| `error` | The error taxonomy |

---

## Configuration

Contract addresses resolve in this order, highest first — the same precedence
and the same variable names as the other two SDKs, so one deployment's `.env`
block configures all three:

1. `StellarAgent::builder().contract(ContractKey::PaymentChannel, "C...")`
2. `STELLARAGENT_TESTNET_PAYMENT_CHANNEL` (network-scoped)
3. `STELLARAGENT_PAYMENT_CHANNEL` (unscoped)
4. The per-network unconfigured sentinel

If the result is not a set of real deployed contract IDs, `build()` fails
immediately with a message naming every missing contract and its environment
variable — rather than letting an opaque RPC error surface later from the
middle of a payment. Pass `.allow_unconfigured_contracts(true)` when you only
need contract-free calls such as `balance()`.

---

## Testing

```bash
cargo test                  # unit, doc, determinism, and mocked-RPC suites
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

`tests/determinism.rs` reads `fixtures/determinism.json` from the repo root —
the identical file the TypeScript and Python suites read — and asserts string
equality, never numeric closeness. "Close enough" is precisely what makes x86
and ARM disagree about a bid score.

`tests/mock_rpc.rs` runs the whole invocation pipeline over real HTTP against a
local stub, so it exercises the envelope this SDK actually produces rather than
mocking out the client's internals.

### Against a local network

```bash
# Start a standalone network and deploy the contracts, then:
export STELLARAGENT_LOCAL_SECRET=S...
export STELLARAGENT_LOCAL_PAYMENT_CHANNEL=C...   # and the other four

cargo test --features integration -- --ignored --test-threads=1
```

`--test-threads=1` is not optional: every test there submits from the same
account, and two transactions built concurrently would claim the same sequence
number.

---

## Relationship to the other SDKs

| | TypeScript | Python | Rust |
| --- | --- | --- | --- |
| Deterministic math | ✅ source of truth | ✅ | ✅ |
| Fixture parity in CI | ✅ | ✅ | ✅ |
| Soroban invocation | ✅ | pending | ✅ |
| Signer abstraction | ✅ | pending | ✅ |

`fixtures/determinism.json` is generated from the TypeScript implementation by
`pnpm fixtures:generate`. If you change `packages/core/src/math`, regenerate it
and re-run all three suites — `pnpm fixtures:check` fails the build when the
committed fixtures are stale.

---

## License

MIT — see [LICENSE](LICENSE).
