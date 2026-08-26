//! A conformance suite for the StellarAgent remote signing protocol.
//!
//! # Two suites, not one
//!
//! The epic's Phase 2 and Phase 5 both say "conformance suite" and they are
//! different things. Conflating them is how one of them ends up not existing.
//!
//! - **Backend conformance** lives in the service crate
//!   (`stellaragent_signer::backend::conformance`) and checks that a *key
//!   backend* signs correctly. In-process, no HTTP.
//! - **Protocol conformance** is this crate, and it is black-box over HTTP so
//!   it can verify **any** implementation in any language.
//!
//! # This crate has two halves too
//!
//! [`server`] points at a running service and checks it implements
//! `docs/signing.md`: endpoint shapes, status codes, the `{ error }` refusal
//! body, and the authentication behaviour.
//!
//! [`client`] is the mirror. It stands up a reference server that asserts the
//! **client's** requests are well-formed and returns canned responses. That is
//! what the TypeScript `RemoteSigner` and the Rust one get run against — Phase
//! 5's "verify the TS RemoteSigner and the future Python one" is about clients,
//! and pointing a server-side suite at them would prove nothing.
//!
//! # A conformance failure names the clause it violates
//!
//! Every check carries the sentence from `docs/signing.md` it is enforcing, so
//! an implementer reading a failure knows what to change without reading this
//! source.

pub mod client;
pub mod server;

/// One conformance check that did not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// A short, stable name — `public-key/shape`, `sign-transaction/401`.
    pub check: String,
    /// What the protocol requires.
    pub requirement: String,
    /// What the implementation did instead.
    pub actual: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}\n    required: {}\n    actual:   {}",
            self.check, self.requirement, self.actual
        )
    }
}

/// The outcome of a conformance run.
#[derive(Debug, Default)]
pub struct Report {
    /// Checks that held.
    pub passed: Vec<String>,
    /// Checks that did not.
    pub failed: Vec<Failure>,
    /// Checks that could not run — usually because a prerequisite failed.
    pub skipped: Vec<String>,
}

impl Report {
    /// Record a passing check.
    pub fn pass(&mut self, check: impl Into<String>) {
        self.passed.push(check.into());
    }

    /// Record a failing check.
    pub fn fail(
        &mut self,
        check: impl Into<String>,
        requirement: impl Into<String>,
        actual: impl Into<String>,
    ) {
        self.failed.push(Failure {
            check: check.into(),
            requirement: requirement.into(),
            actual: actual.into(),
        });
    }

    /// Record a check that could not run.
    pub fn skip(&mut self, check: impl Into<String>) {
        self.skipped.push(check.into());
    }

    /// Assert `condition`, recording either way.
    pub fn check(
        &mut self,
        name: impl Into<String>,
        requirement: impl Into<String>,
        condition: bool,
        actual: impl Into<String>,
    ) {
        let name = name.into();
        if condition {
            self.pass(name);
        } else {
            self.fail(name, requirement, actual);
        }
    }

    /// Whether everything that ran held.
    pub fn is_conformant(&self) -> bool {
        self.failed.is_empty()
    }

    /// Merge another report into this one.
    pub fn absorb(&mut self, other: Report) {
        self.passed.extend(other.passed);
        self.failed.extend(other.failed);
        self.skipped.extend(other.skipped);
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for check in &self.passed {
            writeln!(f, "  ok    {check}")?;
        }
        for check in &self.skipped {
            writeln!(f, "  skip  {check}")?;
        }
        for failure in &self.failed {
            writeln!(f, "  FAIL  {failure}")?;
        }
        writeln!(
            f,
            "\n{} passed, {} failed, {} skipped",
            self.passed.len(),
            self.failed.len(),
            self.skipped.len()
        )
    }
}
