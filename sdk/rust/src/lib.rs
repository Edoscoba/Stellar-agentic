#![deny(missing_docs)]

//! AI agent payment rails on Stellar — the Rust SDK.
//!
//! The third first-class implementation of this protocol, alongside
//! `@stellaragent/core` (TypeScript) and `stellaragent` (Python).
//!
//! [`math`] is the part with a hard correctness requirement: bid scores and spend calculations must come out
//! **byte-identical** in all three languages, or two agents scoring the same
//! pool of bids will disagree about the winner.
//!
//! ```
//! use stellaragent::math::{rank_bids, AgentBid, BidWeights};
//!
//! let bids = vec![AgentBid {
//!     agent_address: "GWORKER".into(),
//!     price: "0.05".into(),
//!     reputation: "88".into(),
//!     estimated_latency_seconds: "12".into(),
//!     success_rate: "0.97".into(),
//! }];
//! let ranked = rank_bids(&bids, &BidWeights::default())?;
//! assert_eq!(ranked[0].score, "46.2500");
//! # Ok::<(), stellaragent::math::FixedPointError>(())
//! ```

pub mod contracts;
pub mod error;
pub mod math;
pub mod rpc;
pub mod scval;
pub mod signer;
pub mod types;

pub use error::{ErrorCode, Result, StellarAgentError};

/// The Stellar XDR types this SDK builds and decodes.
///
/// Re-exported so callers can construct an `ScVal` or inspect a
/// `TransactionEnvelope` without adding `stellar-xdr` to their own manifest
/// and risking a version skew against the one this crate compiled against.
pub use stellar_xdr::curr as xdr;

/// This crate's version, for user agents and diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
