#![deny(missing_docs)]

//! The server side of the `RemoteSigner` protocol.
//!
//! Phases 1–2: the wire protocol, the identity model behind it, and the
//! backends a key can live in. Later phases add inspection and policy, the
//! audit log, and the request path that ties them together.

pub mod auth;
pub mod backend;
pub mod error;
pub mod protocol;
pub mod registry;
pub mod stellar;
pub mod testing;

pub use error::{RefusalReason, Result, ServiceError, Violation};

/// This crate's version, for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
