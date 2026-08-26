#![deny(missing_docs)]

//! The server side of the `RemoteSigner` protocol.
//!
//! `docs/signing.md` argues that a long-lived agent process should not hold a
//! raw secret, and the SDKs ship `RemoteSigner` plus `agent.holdsSecretKey` so
//! that claim is checkable. This is the service to point them at.
//!
//! # One path
//!
//! ```text
//! authenticate → resolve key + policy → decode → evaluate policy
//!                                                      │
//!                                 deny ────────────────┤
//!                                                      │ allow
//!                                      backend.sign(32-byte payload)
//!                                                      │
//!                                                 audit + respond
//! ```
//!
//! The audit write happens on **both** branches, before the response is sent. A
//! refusal that is not recorded is indistinguishable from a request that never
//! arrived, so a sink failure fails the request.
//!
//! # The properties worth knowing
//!
//! - **Key material never leaves.** [`backend::SigningBackend`] has no method
//!   that can return a private key and no escape hatch, so this is enforced by
//!   the shape of the trait rather than by a rule someone has to remember.
//! - **Nothing is signed unread.** [`inspect`] decodes every envelope and
//!   matches every contract call against a known signature; anything it cannot
//!   fully understand is refused. A policy engine that shrugs at an
//!   unrecognised operation is worse than none, because it produces an audit
//!   trail that looks like diligence.
//! - **Deny by default.** An empty allowlist in [`policy`] denies everything. A
//!   policy file with a missing section must fail closed.
//! - **Every violation is reported**, not just the first — an operator fixing a
//!   policy wants the whole list.
//! - **Refused requests cost no budget.** [`policy::RateLimitState::check`]
//!   peeks; only a signature commits. Otherwise a stolen token could exhaust
//!   the legitimate agent's allowance with requests that were never going to be
//!   signed.
//!
//! # Module map
//!
//! | Module | What lives there |
//! |---|---|
//! | [`protocol`] | the wire types, exactly as the spec defines them |
//! | [`inspect`] | decoding an envelope into something policy can reason about |
//! | [`policy`] | the rules and the evaluation model |
//! | [`backend`] | the signing backends and their conformance harness |
//! | [`audit`] | the hash-chained append-only log |
//! | [`auth`] | token hashing, rotation, revocation |
//! | [`registry`] | identity → key and policy, one-way |
//! | [`ledger`] | the ratcheting current-ledger estimate |
//! | [`sign`] | the one orchestration path |
//! | [`http`] | routing and the error rendering |
//! | [`config`] | configuration and its startup checks |
//! | [`stellar`] | what exactly gets signed |
//! | [`testing`] | envelope fixtures for tests and conformance |
//!
//! # Threat model
//!
//! What this protects against, and — as carefully — what it does not, is in
//! `docs/signer-deployment.md`. The short version of the second half: this
//! service becomes the crown jewels, a caller acting *within* policy is
//! invisible to every rule, and the audit chain is tamper-evident rather than
//! tamper-proof until its head hash is anchored somewhere the attacker cannot
//! reach.

pub mod audit;
pub mod auth;
pub mod backend;
pub mod config;
pub mod error;
pub mod http;
pub mod inspect;
pub mod ledger;
pub mod metrics;
pub mod policy;
pub mod protocol;
pub mod registry;
pub mod sign;
pub mod stellar;
pub mod testing;

pub use error::{RefusalReason, Result, ServiceError, Violation};
pub use sign::SignerService;

/// This crate's version, for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
