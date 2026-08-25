#![deny(missing_docs)]

//! AI agent payment rails on Stellar — the Rust SDK.
//!
//! The third first-class implementation of this protocol, alongside
//! `@stellaragent/core` (TypeScript) and `stellaragent` (Python).
//!
//! This first phase lands the part with a hard correctness requirement: the
//! deterministic math. Bid scores and spend calculations must come out
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

pub mod error;
pub mod math;
pub mod types;

pub use error::{ErrorCode, Result, StellarAgentError};

/// This crate's version, for user agents and diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
