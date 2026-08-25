#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! AI agent payment rails on Stellar — the Rust SDK.
//!
//! The third first-class implementation of this protocol, alongside
//! [`@stellaragent/core`] (TypeScript) and `stellaragent` (Python). Agent
//! infrastructure written in Rust — and anything already embedding the Soroban
//! tooling — can talk to the contracts directly instead of shelling out to the
//! TypeScript SDK or re-implementing the protocol.
//!
//! [`@stellaragent/core`]: https://github.com/Edoscoba/Stellar-agentic/tree/main/packages/core
//!
//! # The determinism guarantee
//!
//! Bid scores and spend calculations must come out **byte-identical** in all
//! three languages. That is not a nicety: two agents scoring the same pool of
//! bids have to agree on the winner, and an agent pre-checking a spend limit
//! has to agree with the contract that will enforce it.
//!
//! `fixtures/determinism.json` at the repo root is generated from the
//! TypeScript implementation and asserted against by all three test suites in
//! one required CI job. [`math`] is the Rust half of that;
//! `tests/determinism.rs` is the assertion.
//!
//! A third implementation is also the real test of whether those fixtures
//! actually pin the semantics — two implementations can agree by sharing an
//! assumption neither of them ever wrote down.
//!
//! # A worked example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use stellaragent::contracts::ContractKey;
//! use stellaragent::signer::{RemoteSigner, RemoteSignerOptions};
//! use stellaragent::types::{Network, OpenChannelParams, PayForApiParams, SpendPeriod};
//! use stellaragent::StellarAgent;
//!
//! # async fn example() -> Result<(), stellaragent::StellarAgentError> {
//! // The secret never enters this process — the signing service holds it.
//! let signer = RemoteSigner::new(
//!     RemoteSignerOptions::new("https://signer.internal")
//!         .token(std::env::var("SIGNER_TOKEN").unwrap()),
//! )?;
//!
//! let agent = StellarAgent::builder()
//!     .network(Network::Testnet)
//!     .signer(Arc::new(signer))
//!     .contract(ContractKey::PaymentChannel, "C...")
//!     .asset_contract("USDC", "C...")
//!     .build()
//!     .await?;
//!
//! // Fund a channel and cap what the agent can spend per hour.
//! agent
//!     .open_channel(&OpenChannelParams {
//!         deposit: "10".into(),
//!         limit_per_period: "1".into(),
//!         period: SpendPeriod::Hourly,
//!         token: Some("USDC".into()),
//!     })
//!     .await?;
//!
//! // Pay per API call. The on-chain limit is enforced whatever this process does.
//! agent
//!     .pay_for_api(&PayForApiParams {
//!         endpoint: "https://api.example.com/inference".into(),
//!         amount: "0.001".into(),
//!         asset: Some("USDC".into()),
//!         ..Default::default()
//!     })
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Choosing a bid, without trusting the counterparty's arithmetic
//!
//! ```
//! use stellaragent::math::{rank_bids, AgentBid, BidWeights};
//!
//! let bids = vec![
//!     AgentBid {
//!         agent_address: "GFAST".into(),
//!         price: "0.05".into(),
//!         reputation: "80".into(),
//!         estimated_latency_seconds: "2".into(),
//!         success_rate: "0.95".into(),
//!     },
//!     AgentBid {
//!         agent_address: "GCHEAP".into(),
//!         price: "0.01".into(),
//!         reputation: "60".into(),
//!         estimated_latency_seconds: "30".into(),
//!         success_rate: "0.99".into(),
//!     },
//! ];
//!
//! let ranked = rank_bids(&bids, &BidWeights::default())?;
//! // The same ordering the TypeScript and Python SDKs produce, digit for digit.
//! assert_eq!(ranked[0].agent_address, "GFAST");
//! # Ok::<(), stellaragent::math::FixedPointError>(())
//! ```
//!
//! # Module map
//!
//! | Module | What lives there |
//! |---|---|
//! | [`math`] | Deterministic fixed-point arithmetic, bid scoring, spend prediction, ledger-time estimates |
//! | [`client`] | [`StellarAgent`]: build, simulate, sign, submit, poll, and the operations built on it |
//! | [`rpc`] | The Soroban RPC transport |
//! | [`scval`] | `ScVal` encoding and decoding for every contract type |
//! | [`signer`] | The [`Signer`](signer::Signer) trait, an in-memory keypair, and a remote signing service |
//! | [`contracts`] | Contract-address resolution and its deployed-or-not check |
//! | [`types`] | The shared data types, with the TypeScript SDK's JSON shape |
//! | [`error`] | The error taxonomy |
//!
//! # Floating point
//!
//! There is deliberately no `impl IntoDecimal for f64`: a float reaching a
//! monetary or score calculation reintroduces exactly the cross-platform
//! divergence [`math`] exists to prevent. Pass decimal strings. The one place
//! `f64` appears is [`math::ledger_time`], which produces wall-clock
//! *estimates* for display and never feeds a payment decision.

pub mod client;
pub mod contracts;
pub mod error;
pub mod math;
pub mod rpc;
pub mod scval;
pub mod signer;
pub mod types;

pub use client::{StellarAgent, StellarAgentBuilder};
pub use error::{ErrorCode, Result, StellarAgentError};

/// The Stellar XDR types this SDK builds and decodes.
///
/// Re-exported so callers can construct an `ScVal` or inspect a
/// `TransactionEnvelope` without adding `stellar-xdr` to their own manifest
/// and risking a version skew against the one this crate compiled against.
pub use stellar_xdr::curr as xdr;

/// This crate's version, for user agents and diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
