#![deny(missing_docs)]

//! The server side of the `RemoteSigner` protocol.
//!
//! Phase 4: a tamper-evident record of every request and decision, and the
//! counters worth alerting on. The request path that ties everything together
//! lands next.

pub mod audit;
pub mod auth;
pub mod backend;
pub mod error;
pub mod inspect;
pub mod ledger;
pub mod metrics;
pub mod policy;
pub mod protocol;
pub mod registry;
pub mod stellar;
pub mod testing;

pub use error::{RefusalReason, Result, ServiceError, Violation};

/// This crate's version, for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
