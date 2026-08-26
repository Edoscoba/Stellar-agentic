#![deny(missing_docs)]

//! The server side of the `RemoteSigner` protocol.
//!
//! Phase 3: decoding what we are asked to sign, and deciding whether to.
//! Later phases add the audit log and the request path that ties it together.

pub mod auth;
pub mod backend;
pub mod error;
pub mod inspect;
pub mod ledger;
pub mod policy;
pub mod protocol;
pub mod registry;
pub mod stellar;
pub mod testing;

pub use error::{RefusalReason, Result, ServiceError, Violation};

/// This crate's version, for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
