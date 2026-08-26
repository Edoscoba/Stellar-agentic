//! The SDK's error taxonomy, mapped onto Rust types.
//!
//! Rust port of `packages/core/src/errors.ts`. The TypeScript SDK carries a
//! `StellarAgentErrorCode` string union alongside a message; here the code
//! *is* the type, so a caller matches on [`ErrorCode`] rather than comparing
//! strings, and the compiler points out the arm they forgot when a new code is
//! added.
//!
//! # Why the code is separate from the message
//!
//! Soroban contracts fail by panicking with a string. The only machine-readable
//! signal that reaches the client is that string, so
//! [`StellarAgentError::from_contract_message`] maps known panic texts onto
//! codes — see `contracts/payment_channel/src/lib.rs` and
//! `contracts/rate_limiter/src/lib.rs` for the panic sites. A caller can then
//! retry on [`ErrorCode::TransactionTimeout`], surface a funding prompt on
//! [`ErrorCode::SpendLimitExceeded`], and treat anything else as fatal —
//! without pattern-matching on prose that will be reworded eventually.

use std::fmt;

/// Stable machine-readable codes for `StellarAgent` failures.
///
/// Identical in name and meaning to the TypeScript `StellarAgentErrorCode`
/// union, so a log line from one SDK is greppable against the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// A caller-supplied argument was invalid before anything was sent.
    InvalidArgument,
    /// An operation needed a payment channel and none was open.
    NoActiveChannel,
    /// The channel's per-period spend limit would be exceeded.
    SpendLimitExceeded,
    /// No channel exists with the given ID.
    ChannelNotFound,
    /// The channel exists but has been closed.
    ChannelClosed,
    /// No escrow job exists with the given ID.
    JobNotFound,
    /// The job is not in a state that allows this transition.
    JobNotOpen,
    /// The job's deadline ledger has passed.
    JobExpired,
    /// The signer is not authorised for this operation.
    NotAuthorized,
    /// No rate limit is configured for this agent.
    RateLimitNotFound,
    /// The contract panicked for a reason with no more specific code.
    ContractError,
    /// Simulation failed before anything was submitted.
    SimulationFailed,
    /// The RPC server rejected the submission outright.
    SubmissionFailed,
    /// The transaction was included but failed on-chain.
    TransactionFailed,
    /// The transaction did not reach a terminal status in time.
    TransactionTimeout,
    /// The RPC or Horizon endpoint could not be reached, or replied unusably.
    NetworkError,
}

impl ErrorCode {
    /// The `SCREAMING_SNAKE_CASE` spelling used by the TypeScript SDK.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::NoActiveChannel => "NO_ACTIVE_CHANNEL",
            Self::SpendLimitExceeded => "SPEND_LIMIT_EXCEEDED",
            Self::ChannelNotFound => "CHANNEL_NOT_FOUND",
            Self::ChannelClosed => "CHANNEL_CLOSED",
            Self::JobNotFound => "JOB_NOT_FOUND",
            Self::JobNotOpen => "JOB_NOT_OPEN",
            Self::JobExpired => "JOB_EXPIRED",
            Self::NotAuthorized => "NOT_AUTHORIZED",
            Self::RateLimitNotFound => "RATE_LIMIT_NOT_FOUND",
            Self::ContractError => "CONTRACT_ERROR",
            Self::SimulationFailed => "SIMULATION_FAILED",
            Self::SubmissionFailed => "SUBMISSION_FAILED",
            Self::TransactionFailed => "TRANSACTION_FAILED",
            Self::TransactionTimeout => "TRANSACTION_TIMEOUT",
            Self::NetworkError => "NETWORK_ERROR",
        }
    }

    /// Whether retrying the same call unchanged could plausibly succeed.
    ///
    /// A timeout or a transport failure says nothing about whether the request
    /// was valid; a spend-limit rejection says the request will keep failing
    /// until the window rolls over. Callers with a retry loop should consult
    /// this rather than retrying everything.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::NetworkError | Self::TransactionTimeout)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The error type every fallible SDK operation returns.
#[derive(Debug)]
pub struct StellarAgentError {
    code: ErrorCode,
    message: String,
    transaction_hash: Option<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl StellarAgentError {
    /// Build an error with a code and a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            transaction_hash: None,
            source: None,
        }
    }

    /// Attach the hash of the transaction this failure belongs to.
    ///
    /// Set for every failure that occurred *after* submission, so an operator
    /// can look the transaction up rather than reconstruct which one it was.
    #[must_use]
    pub fn with_transaction_hash(mut self, hash: impl Into<String>) -> Self {
        self.transaction_hash = Some(hash.into());
        self
    }

    /// Attach the underlying cause.
    #[must_use]
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// The machine-readable code.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The human-readable message, without the code prefix `Display` adds.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The transaction this failure belongs to, if it had been submitted.
    pub fn transaction_hash(&self) -> Option<&str> {
        self.transaction_hash.as_deref()
    }

    /// Shorthand for `self.code().is_retryable()`.
    pub fn is_retryable(&self) -> bool {
        self.code.is_retryable()
    }

    /// Classify a contract panic message onto a specific [`ErrorCode`].
    ///
    /// Soroban surfaces a contract failure as prose, so this is pattern
    /// matching on text — unavoidable, and deliberately concentrated in one
    /// function rather than smeared across every call site. `fallback` is used
    /// when nothing matches, which is the common case for a contract the SDK
    /// does not know.
    ///
    /// The patterns are the same ones `packages/core/src/index.ts` uses, so
    /// all three SDKs classify the same panic identically.
    ///
    /// ```
    /// use stellaragent::{ErrorCode, StellarAgentError};
    ///
    /// let error = StellarAgentError::from_contract_message(
    ///     ErrorCode::TransactionFailed,
    ///     "HostError: Error(Contract, #0) spend limit exceeded",
    /// );
    /// assert_eq!(error.code(), ErrorCode::SpendLimitExceeded);
    /// ```
    pub fn from_contract_message(fallback: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        let code = classify_contract_message(&message).unwrap_or(fallback);
        Self::new(code, message)
    }
}

impl fmt::Display for StellarAgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(hash) = &self.transaction_hash {
            write!(f, " (transaction {hash})")?;
        }
        Ok(())
    }
}

impl std::error::Error for StellarAgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl From<crate::math::FixedPointError> for StellarAgentError {
    /// Bad numbers are bad arguments: a value that will not parse never
    /// reaches the network, so it is an [`ErrorCode::InvalidArgument`] rather
    /// than a contract or transport failure.
    fn from(error: crate::math::FixedPointError) -> Self {
        Self::new(ErrorCode::InvalidArgument, error.to_string()).with_source(error)
    }
}

impl From<crate::types::UnknownNetwork> for StellarAgentError {
    fn from(error: crate::types::UnknownNetwork) -> Self {
        Self::new(ErrorCode::InvalidArgument, error.to_string()).with_source(error)
    }
}

/// The convenience alias every public fallible function uses.
pub type Result<T> = std::result::Result<T, StellarAgentError>;

/// Lowercase-substring classification of a contract panic message.
///
/// Ordered most-specific first: `"job is not open"` must be tested before the
/// looser authorisation patterns, or a message containing both would be
/// misfiled.
fn classify_contract_message(message: &str) -> Option<ErrorCode> {
    let haystack = message.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|n| haystack.contains(n));

    if contains_any(&["spend limit exceeded"]) {
        return Some(ErrorCode::SpendLimitExceeded);
    }
    if contains_any(&["channel not found"]) {
        return Some(ErrorCode::ChannelNotFound);
    }
    if contains_any(&["channel is closed"]) {
        return Some(ErrorCode::ChannelClosed);
    }
    if contains_any(&["job not found"]) {
        return Some(ErrorCode::JobNotFound);
    }
    if contains_any(&["job is not open"]) {
        return Some(ErrorCode::JobNotOpen);
    }
    if contains_any(&["job has expired"]) {
        return Some(ErrorCode::JobExpired);
    }
    if contains_any(&[
        "not authorized",
        "not the authorized",
        "not assigned",
        "not the assigned",
    ]) {
        return Some(ErrorCode::NotAuthorized);
    }
    if contains_any(&["no rate limit", "limit not found"]) {
        return Some(ErrorCode::RateLimitNotFound);
    }
    if contains_any(&["deadline must"])
        || (contains_any(&["amount", "deposit", "limit"]) && contains_any(&["positive", "invalid"]))
    {
        return Some(ErrorCode::InvalidArgument);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_contract_panics_map_to_specific_codes() {
        let cases = [
            ("spend limit exceeded", ErrorCode::SpendLimitExceeded),
            ("channel not found", ErrorCode::ChannelNotFound),
            ("channel is closed", ErrorCode::ChannelClosed),
            ("job not found", ErrorCode::JobNotFound),
            ("job is not open", ErrorCode::JobNotOpen),
            ("job has expired", ErrorCode::JobExpired),
            ("caller is not authorized", ErrorCode::NotAuthorized),
            ("no rate limit for agent", ErrorCode::RateLimitNotFound),
            ("amount must be positive", ErrorCode::InvalidArgument),
            ("deposit must be positive", ErrorCode::InvalidArgument),
            ("deadline must be in the future", ErrorCode::InvalidArgument),
        ];
        for (message, expected) in cases {
            let error = StellarAgentError::from_contract_message(ErrorCode::ContractError, message);
            assert_eq!(error.code(), expected, "classifying {message:?}");
        }
    }

    #[test]
    fn classification_is_case_insensitive_and_tolerates_host_error_wrapping() {
        let error = StellarAgentError::from_contract_message(
            ErrorCode::TransactionFailed,
            "HostError: Error(Contract, #0), Escrow: JOB HAS EXPIRED",
        );
        assert_eq!(error.code(), ErrorCode::JobExpired);
    }

    #[test]
    fn an_unrecognised_panic_keeps_the_callers_fallback() {
        let error = StellarAgentError::from_contract_message(
            ErrorCode::SimulationFailed,
            "something entirely new went wrong",
        );
        assert_eq!(error.code(), ErrorCode::SimulationFailed);
    }

    #[test]
    fn only_transport_and_timeout_failures_are_retryable() {
        assert!(ErrorCode::NetworkError.is_retryable());
        assert!(ErrorCode::TransactionTimeout.is_retryable());
        assert!(!ErrorCode::SpendLimitExceeded.is_retryable());
        assert!(!ErrorCode::InvalidArgument.is_retryable());
    }

    #[test]
    fn display_leads_with_the_code_and_names_the_transaction() {
        let error = StellarAgentError::new(ErrorCode::TransactionFailed, "pay reverted")
            .with_transaction_hash("abc123");
        assert_eq!(
            error.to_string(),
            "[TRANSACTION_FAILED] pay reverted (transaction abc123)"
        );
    }

    #[test]
    fn a_bad_decimal_becomes_an_invalid_argument() {
        let error: StellarAgentError = crate::math::bn("not-a-number").unwrap_err().into();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(std::error::Error::source(&error).is_some());
    }
}
