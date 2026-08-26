//! Understanding what we are being asked to sign.
//!
//! `docs/signing.md` argues that the service returns `signedXdr` rather than a
//! signature over an opaque digest precisely so it *can* inspect what it signs.
//! This module is where that argument is cashed.
//!
//! # Refuse what you cannot read
//!
//! The governing rule: **anything this module cannot fully decode is refused,
//! not signed.** A policy engine that shrugs at an unrecognised operation and
//! signs it anyway is strictly worse than no policy engine, because it produces
//! an audit trail that looks like diligence. The failure mode of refusing is a
//! payment that does not happen and an operator who has to add a function spec;
//! the failure mode of the alternative is a signature over something nobody
//! looked at.
//!
//! # Argument roles, not argument positions
//!
//! Extracting "every `i128` in the argument list" and calling them amounts
//! would be simple and wrong: `open_channel`'s `limit_per_period` and
//! `set_limits`' three ceilings are `i128` values that are not spends, and
//! capping them would refuse legitimate configuration calls while telling the
//! operator their spend limit was exceeded.
//!
//! So each known contract function has a spec naming what each argument *is*
//! ([`ArgRole`]), and policy reasons about roles. An unknown function has no
//! spec, which is exactly why it is refused.

use stellar_xdr::curr::{
    HostFunction, InvokeHostFunctionOp, Limits, Operation, OperationBody, ReadXdr, ScAddress,
    ScBytes, ScString, ScSymbol, ScVal, SorobanAuthorizationEntry, SorobanAuthorizedFunction,
    SorobanCredentials, Transaction, TransactionEnvelope,
};

use crate::error::{RefusalReason, Result, ServiceError};

/// What a contract argument means, for policy purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgRole {
    /// The address authorising the call — normally the agent itself.
    Signer,
    /// Who receives value. Subject to the recipient allowlist.
    Recipient,
    /// Value moved. Subject to the amount cap.
    Amount,
    /// A ceiling being *configured*, not spent. Explicitly **not** capped.
    Limit,
    /// A token contract address.
    Asset,
    /// An identifier — channel id, job id.
    Id,
    /// Free text: an endpoint, a task description, a result.
    Text,
    /// A ledger number, count, or enum variant.
    Scalar,
}

/// What one known contract function looks like.
struct FunctionSpec {
    function: &'static str,
    roles: &'static [ArgRole],
}

/// The contract calls this service understands.
///
/// Derived from `packages/core/src/index.ts` and `sdk/rust/src/client.rs` —
/// the two clients that actually build these invocations. Adding a contract
/// means adding a row here, deliberately: a new row is a reviewable change to
/// what the signer will authorise.
const KNOWN_FUNCTIONS: &[FunctionSpec] = &[
    // ── payment_channel ──────────────────────────────────────────────────
    FunctionSpec {
        function: "pay",
        roles: &[
            ArgRole::Signer,
            ArgRole::Id,
            ArgRole::Recipient,
            ArgRole::Amount,
            ArgRole::Text,
        ],
    },
    FunctionSpec {
        function: "pay_with_conversion",
        roles: &[
            ArgRole::Signer,
            ArgRole::Id,
            ArgRole::Recipient,
            ArgRole::Amount,
            ArgRole::Asset,
            // `min_received` is a slippage floor the caller sets, not value
            // leaving the channel. Capping it would refuse a *safer* trade.
            ArgRole::Limit,
            ArgRole::Text,
        ],
    },
    FunctionSpec {
        function: "open_channel",
        roles: &[
            ArgRole::Signer,
            ArgRole::Signer,
            ArgRole::Asset,
            // A deposit does move value, and out of the owner's account.
            ArgRole::Amount,
            ArgRole::Limit,
            ArgRole::Scalar,
        ],
    },
    FunctionSpec {
        function: "close_channel",
        roles: &[ArgRole::Signer, ArgRole::Id],
    },
    FunctionSpec {
        function: "remaining_this_period",
        roles: &[ArgRole::Id],
    },
    FunctionSpec {
        function: "get_channel",
        roles: &[ArgRole::Id],
    },
    // ── escrow ───────────────────────────────────────────────────────────
    FunctionSpec {
        function: "create_job",
        roles: &[
            ArgRole::Signer,
            ArgRole::Asset,
            ArgRole::Amount,
            ArgRole::Text,
            ArgRole::Scalar,
            ArgRole::Recipient,
        ],
    },
    FunctionSpec {
        function: "accept_job",
        roles: &[ArgRole::Signer, ArgRole::Id],
    },
    FunctionSpec {
        function: "submit_result",
        roles: &[ArgRole::Signer, ArgRole::Id, ArgRole::Text],
    },
    FunctionSpec {
        function: "release",
        roles: &[ArgRole::Signer, ArgRole::Id],
    },
    FunctionSpec {
        function: "get_job",
        roles: &[ArgRole::Id],
    },
    // ── rate_limiter ─────────────────────────────────────────────────────
    FunctionSpec {
        function: "set_limits",
        roles: &[
            ArgRole::Signer,
            ArgRole::Signer,
            // All three are ceilings being configured, not value moved.
            ArgRole::Limit,
            ArgRole::Limit,
            ArgRole::Limit,
            ArgRole::Scalar,
        ],
    },
    FunctionSpec {
        function: "check",
        roles: &[ArgRole::Signer, ArgRole::Amount],
    },
    FunctionSpec {
        function: "get_limits",
        roles: &[ArgRole::Signer],
    },
    // ── agent_wallet_factory ─────────────────────────────────────────────
    FunctionSpec {
        function: "create_agent",
        roles: &[ArgRole::Signer, ArgRole::Signer, ArgRole::Text],
    },
    FunctionSpec {
        function: "get_agent",
        roles: &[ArgRole::Id],
    },
];

/// How to treat a contract call with no entry in the service's function table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnknownCallPolicy {
    /// Refuse to sign it. The default, and the right answer for production.
    #[default]
    Refuse,
    /// Decode conservatively: treat **every** `i128` as an amount and **every**
    /// address as a recipient.
    ///
    /// Deliberately the restrictive direction — an unknown call becomes subject
    /// to *more* rules, never fewer. A `set_limits`-shaped call under this
    /// policy would be refused by the amount cap, which is an annoyance;
    /// the opposite mistake is a signature over an unbounded transfer.
    Conservative,
}

/// One decoded contract argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedArg {
    /// What this argument means.
    pub role: ArgRole,
    /// Its value, rendered for the audit log.
    pub value: ArgValue,
}

/// A decoded argument value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgValue {
    /// A Stellar address (`G…` or `C…`).
    Address(String),
    /// An integer, in stroops where it is monetary.
    Int(i128),
    /// Text.
    Text(String),
    /// Anything else we decoded but do not model — an enum variant, a void.
    Other(String),
}

impl std::fmt::Display for ArgValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Address(value) => f.write_str(value),
            Self::Int(value) => write!(f, "{value}"),
            Self::Text(value) => write!(f, "{value:?}"),
            Self::Other(value) => f.write_str(value),
        }
    }
}

/// One decoded contract invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedCall {
    /// The contract being invoked (`C…`).
    pub contract: String,
    /// The function name.
    pub function: String,
    /// The decoded arguments, in order.
    pub args: Vec<InspectedArg>,
    /// `true` when this was decoded by [`UnknownCallPolicy::Conservative`]
    /// rather than from a known spec. Surfaced in the audit record so an
    /// operator can see which calls are running without a reviewed spec.
    pub conservative: bool,
}

impl InspectedCall {
    /// Every value in an [`ArgRole::Amount`] position.
    pub fn amounts(&self) -> Vec<i128> {
        self.args
            .iter()
            .filter(|arg| arg.role == ArgRole::Amount)
            .filter_map(|arg| match &arg.value {
                ArgValue::Int(value) => Some(*value),
                _ => None,
            })
            .collect()
    }

    /// Every address in an [`ArgRole::Recipient`] position.
    pub fn recipients(&self) -> Vec<&str> {
        self.args
            .iter()
            .filter(|arg| arg.role == ArgRole::Recipient)
            .filter_map(|arg| match &arg.value {
                ArgValue::Address(value) => Some(value.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A decoded transaction, ready for policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedTransaction {
    /// The account the transaction is submitted from.
    pub source_account: String,
    /// Its sequence number.
    pub sequence: i64,
    /// The fee, in stroops.
    pub fee: u32,
    /// The upper time bound, if the transaction has one.
    ///
    /// `None` means the envelope never expires, which
    /// [`crate::policy`] refuses: an unbounded envelope stays submittable
    /// forever, so one that never made it into a ledger could be replayed much
    /// later.
    pub max_time: Option<u64>,
    /// The contract calls it makes.
    pub calls: Vec<InspectedCall>,
}

/// A decoded authorisation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedAuthEntry {
    /// The address whose authorisation is being given.
    pub address: String,
    /// The entry's nonce.
    pub nonce: i64,
    /// The invocation being authorised, when it is a contract call.
    pub call: Option<InspectedCall>,
}

/// Decode a base64 transaction envelope.
///
/// # Errors
///
/// - [`RefusalReason::MalformedEnvelope`] for anything that is not a v1
///   envelope carrying no signatures.
/// - [`RefusalReason::Uninspectable`] for an operation or contract call this
///   service does not model.
pub fn inspect_transaction(
    envelope_xdr: &str,
    unknown: UnknownCallPolicy,
) -> Result<(Transaction, InspectedTransaction)> {
    let envelope =
        TransactionEnvelope::from_xdr_base64(envelope_xdr, Limits::none()).map_err(|error| {
            ServiceError::new(
                RefusalReason::MalformedEnvelope,
                "the transaction is not valid TransactionEnvelope XDR",
            )
            .with_internal(error.to_string())
        })?;

    let inner = match envelope {
        TransactionEnvelope::Tx(inner) => inner,
        // A v0 envelope is a pre-Protocol-13 format nothing in this system
        // builds, and a fee-bump wraps someone else's transaction — signing
        // one would mean paying for a transaction we never inspected.
        TransactionEnvelope::TxV0(_) => {
            return Err(ServiceError::new(
                RefusalReason::MalformedEnvelope,
                "v0 transaction envelopes are not supported; send a v1 envelope",
            ))
        }
        TransactionEnvelope::TxFeeBump(_) => {
            return Err(ServiceError::new(
                RefusalReason::MalformedEnvelope,
                "fee-bump transactions are not supported: signing one would pay for an inner \
                 transaction this service has not inspected",
            ))
        }
    };

    // An envelope arriving with signatures means either a multisig flow this
    // service does not implement, or an attempt to smuggle an extra signer
    // past inspection. Either way it is not something to co-sign.
    if !inner.signatures.is_empty() {
        return Err(ServiceError::new(
            RefusalReason::MalformedEnvelope,
            format!(
                "the envelope already carries {} signature(s); this service does not co-sign",
                inner.signatures.len()
            ),
        ));
    }

    let transaction = inner.tx;
    let max_time = match &transaction.cond {
        stellar_xdr::curr::Preconditions::Time(bounds) => match bounds.max_time.0 {
            0 => None,
            value => Some(value),
        },
        stellar_xdr::curr::Preconditions::V2(v2) => {
            v2.time_bounds
                .as_ref()
                .and_then(|bounds| match bounds.max_time.0 {
                    0 => None,
                    value => Some(value),
                })
        }
        stellar_xdr::curr::Preconditions::None => None,
    };

    let mut calls = Vec::new();
    for operation in transaction.operations.iter() {
        calls.push(inspect_operation(operation, unknown)?);
    }

    let inspected = InspectedTransaction {
        source_account: transaction.source_account.to_string(),
        sequence: transaction.seq_num.0,
        fee: transaction.fee,
        max_time,
        calls,
    };

    Ok((transaction, inspected))
}

fn inspect_operation(operation: &Operation, unknown: UnknownCallPolicy) -> Result<InspectedCall> {
    // A per-operation source account would let one operation in a transaction
    // act as a different account than the one we checked.
    if operation.source_account.is_some() {
        return Err(ServiceError::new(
            RefusalReason::Uninspectable,
            "an operation overrides the transaction's source account, which this service will \
             not sign",
        ));
    }

    match &operation.body {
        OperationBody::InvokeHostFunction(InvokeHostFunctionOp { host_function, .. }) => {
            match host_function {
                HostFunction::InvokeContract(args) => inspect_invocation(
                    &args.contract_address,
                    &args.function_name,
                    args.args.as_slice(),
                    unknown,
                ),
                // Uploading or creating a contract is a deployment action, not
                // an agent payment. An agent key should not be doing it, and if
                // it must, that is a deliberate config change rather than
                // something to allow by default.
                other => Err(ServiceError::new(
                    RefusalReason::Uninspectable,
                    format!(
                        "host function {} is not something this service signs; only contract \
                         invocations are supported",
                        host_function_name(other)
                    ),
                )),
            }
        }
        other => Err(ServiceError::new(
            RefusalReason::Uninspectable,
            format!(
                "operation type {} is not modelled by this service, so it will not be signed",
                operation_name(other)
            ),
        )),
    }
}

fn inspect_invocation(
    contract: &ScAddress,
    function: &ScSymbol,
    args: &[ScVal],
    unknown: UnknownCallPolicy,
) -> Result<InspectedCall> {
    let function_name = String::from_utf8(function.0.as_slice().to_vec()).map_err(|_| {
        ServiceError::new(
            RefusalReason::MalformedEnvelope,
            "the contract function name is not valid UTF-8",
        )
    })?;

    let spec = KNOWN_FUNCTIONS
        .iter()
        .find(|spec| spec.function == function_name);

    let (roles, conservative): (Vec<ArgRole>, bool) = match spec {
        // A known function whose arity has changed is *not* a match. Reusing a
        // stale spec would mean labelling arguments by a signature the contract
        // no longer has — the amount cap could end up applied to a channel id.
        Some(spec) if spec.roles.len() == args.len() => (spec.roles.to_vec(), false),
        Some(spec) => {
            return Err(ServiceError::new(
                RefusalReason::Uninspectable,
                format!(
                    "`{function_name}` was called with {} arguments but this service's spec has \
                     {}; the contract and the signer disagree about its signature",
                    args.len(),
                    spec.roles.len()
                ),
            ))
        }
        None => match unknown {
            UnknownCallPolicy::Refuse => {
                return Err(ServiceError::new(
                    RefusalReason::Uninspectable,
                    format!(
                        "`{function_name}` is not a contract function this service knows how to \
                         inspect, so it will not be signed"
                    ),
                ))
            }
            UnknownCallPolicy::Conservative => (args.iter().map(conservative_role).collect(), true),
        },
    };

    let mut decoded = Vec::with_capacity(args.len());
    for (value, role) in args.iter().zip(roles) {
        decoded.push(InspectedArg {
            role,
            value: decode_value(value)?,
        });
    }

    Ok(InspectedCall {
        contract: contract.to_string(),
        function: function_name,
        args: decoded,
        conservative,
    })
}

/// The restrictive reading of an argument in an unknown call.
fn conservative_role(value: &ScVal) -> ArgRole {
    match value {
        ScVal::I128(_) | ScVal::U128(_) | ScVal::I64(_) | ScVal::U64(_) => ArgRole::Amount,
        ScVal::Address(_) => ArgRole::Recipient,
        _ => ArgRole::Scalar,
    }
}

fn decode_value(value: &ScVal) -> Result<ArgValue> {
    Ok(match value {
        ScVal::Address(address) => ArgValue::Address(address.to_string()),
        ScVal::I128(parts) => ArgValue::Int((i128::from(parts.hi) << 64) | i128::from(parts.lo)),
        ScVal::U128(parts) => {
            let value = (u128::from(parts.hi) << 64) | u128::from(parts.lo);
            ArgValue::Int(i128::try_from(value).map_err(|_| {
                ServiceError::new(
                    RefusalReason::Uninspectable,
                    "a u128 argument is too large to reason about as an amount",
                )
            })?)
        }
        ScVal::U64(value) => ArgValue::Int(i128::from(*value)),
        ScVal::I64(value) => ArgValue::Int(i128::from(*value)),
        ScVal::U32(value) => ArgValue::Int(i128::from(*value)),
        ScVal::I32(value) => ArgValue::Int(i128::from(*value)),
        ScVal::Bool(value) => ArgValue::Other(value.to_string()),
        ScVal::Void => ArgValue::Other("void".into()),
        ScVal::String(ScString(text)) => ArgValue::Text(lossy_text(text.as_slice())),
        ScVal::Symbol(ScSymbol(text)) => ArgValue::Text(lossy_text(text.as_slice())),
        ScVal::Bytes(ScBytes(bytes)) => ArgValue::Text(lossy_text(bytes.as_slice())),
        ScVal::Vec(_) => ArgValue::Other("vec".into()),
        ScVal::Map(_) => ArgValue::Other("map".into()),
        other => ArgValue::Other(other.name().to_string()),
    })
}

/// Render bytes for the audit log without letting arbitrary input in.
///
/// Truncated, and non-printable bytes replaced: an endpoint field is caller-
/// controlled, and a log line is somewhere it should not be able to inject
/// control characters or unbounded length.
fn lossy_text(bytes: &[u8]) -> String {
    const MAX: usize = 256;
    let truncated = &bytes[..bytes.len().min(MAX)];
    let mut text: String = String::from_utf8_lossy(truncated)
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect();
    if bytes.len() > MAX {
        text.push('…');
    }
    text
}

/// Decode a base64 authorisation entry.
pub fn inspect_auth_entry(
    entry_xdr: &str,
    unknown: UnknownCallPolicy,
) -> Result<(SorobanAuthorizationEntry, InspectedAuthEntry)> {
    let entry =
        SorobanAuthorizationEntry::from_xdr_base64(entry_xdr, Limits::none()).map_err(|error| {
            ServiceError::new(
                RefusalReason::MalformedEnvelope,
                "the authorization entry is not valid SorobanAuthorizationEntry XDR",
            )
            .with_internal(error.to_string())
        })?;

    let SorobanCredentials::Address(credentials) = &entry.credentials else {
        return Err(ServiceError::new(
            RefusalReason::MalformedEnvelope,
            "this authorization entry uses source-account credentials and needs no separate \
             signature; it is covered by the transaction signature",
        ));
    };

    let call = match &entry.root_invocation.function {
        SorobanAuthorizedFunction::ContractFn(args) => Some(inspect_invocation(
            &args.contract_address,
            &args.function_name,
            args.args.as_slice(),
            unknown,
        )?),
        // Authorising a contract *creation* from an agent key is a deployment
        // action; refuse rather than model it.
        SorobanAuthorizedFunction::CreateContractHostFn(_)
        | SorobanAuthorizedFunction::CreateContractV2HostFn(_) => {
            return Err(ServiceError::new(
                RefusalReason::Uninspectable,
                "this authorization entry authorises contract creation, which this service does \
                 not sign",
            ))
        }
    };

    let inspected = InspectedAuthEntry {
        address: credentials.address.to_string(),
        nonce: credentials.nonce,
        call,
    };

    Ok((entry, inspected))
}

fn host_function_name(function: &HostFunction) -> &'static str {
    match function {
        HostFunction::InvokeContract(_) => "InvokeContract",
        HostFunction::CreateContract(_) => "CreateContract",
        HostFunction::UploadContractWasm(_) => "UploadContractWasm",
        HostFunction::CreateContractV2(_) => "CreateContractV2",
    }
}

fn operation_name(body: &OperationBody) -> &'static str {
    match body {
        OperationBody::CreateAccount(_) => "CreateAccount",
        OperationBody::Payment(_) => "Payment",
        OperationBody::PathPaymentStrictReceive(_) => "PathPaymentStrictReceive",
        OperationBody::ManageSellOffer(_) => "ManageSellOffer",
        OperationBody::CreatePassiveSellOffer(_) => "CreatePassiveSellOffer",
        OperationBody::SetOptions(_) => "SetOptions",
        OperationBody::ChangeTrust(_) => "ChangeTrust",
        OperationBody::AllowTrust(_) => "AllowTrust",
        OperationBody::AccountMerge(_) => "AccountMerge",
        OperationBody::Inflation => "Inflation",
        OperationBody::ManageData(_) => "ManageData",
        OperationBody::BumpSequence(_) => "BumpSequence",
        OperationBody::ManageBuyOffer(_) => "ManageBuyOffer",
        OperationBody::PathPaymentStrictSend(_) => "PathPaymentStrictSend",
        OperationBody::CreateClaimableBalance(_) => "CreateClaimableBalance",
        OperationBody::ClaimClaimableBalance(_) => "ClaimClaimableBalance",
        OperationBody::BeginSponsoringFutureReserves(_) => "BeginSponsoringFutureReserves",
        OperationBody::EndSponsoringFutureReserves => "EndSponsoringFutureReserves",
        OperationBody::RevokeSponsorship(_) => "RevokeSponsorship",
        OperationBody::Clawback(_) => "Clawback",
        OperationBody::ClawbackClaimableBalance(_) => "ClawbackClaimableBalance",
        OperationBody::SetTrustLineFlags(_) => "SetTrustLineFlags",
        OperationBody::LiquidityPoolDeposit(_) => "LiquidityPoolDeposit",
        OperationBody::LiquidityPoolWithdraw(_) => "LiquidityPoolWithdraw",
        OperationBody::InvokeHostFunction(_) => "InvokeHostFunction",
        OperationBody::ExtendFootprintTtl(_) => "ExtendFootprintTtl",
        OperationBody::RestoreFootprint(_) => "RestoreFootprint",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn a_payment_decodes_into_roles_not_positions() {
        let xdr = testing::payment_envelope(testing::PaymentSpec::default());
        let (_, inspected) = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap();

        assert_eq!(inspected.calls.len(), 1);
        let call = &inspected.calls[0];
        assert_eq!(call.function, "pay");
        assert!(!call.conservative);
        assert_eq!(call.amounts(), vec![10_000_000]);
        assert_eq!(call.recipients(), vec![testing::RECIPIENT.as_str()]);
    }

    #[test]
    fn a_configured_ceiling_is_not_treated_as_a_spend() {
        // The reason roles exist. `set_limits` carries three i128 ceilings; a
        // naive "every i128 is an amount" reading would refuse every attempt
        // to configure a rate limit and blame the spend cap for it.
        let xdr = testing::set_limits_envelope(50_000_000_000);
        let (_, inspected) = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap();
        assert!(
            inspected.calls[0].amounts().is_empty(),
            "ceilings must not be read as amounts: {:?}",
            inspected.calls[0]
        );
    }

    #[test]
    fn an_unknown_function_is_refused_by_default() {
        let xdr = testing::unknown_call_envelope("drain_everything", 999);
        let error = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::Uninspectable);
        assert!(
            error.message().contains("drain_everything"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn an_unknown_function_read_conservatively_becomes_more_restricted_not_less() {
        // Every integer becomes an amount, so the cap applies to all of them.
        let xdr = testing::unknown_call_envelope("drain_everything", 999);
        let (_, inspected) = inspect_transaction(&xdr, UnknownCallPolicy::Conservative).unwrap();
        let call = &inspected.calls[0];
        assert!(call.conservative);
        assert_eq!(call.amounts(), vec![999]);
        assert_eq!(call.recipients(), vec![testing::RECIPIENT.as_str()]);
    }

    #[test]
    fn a_known_function_called_with_the_wrong_arity_is_refused() {
        // A contract that grew an argument must not be labelled with a stale
        // spec — the amount cap could end up applied to a channel id.
        let xdr = testing::wrong_arity_pay_envelope();
        let error = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::Uninspectable);
        assert!(error.message().contains("disagree"), "{}", error.message());
    }

    #[test]
    fn an_envelope_that_already_has_signatures_is_refused() {
        let xdr = testing::pre_signed_envelope();
        let error = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::MalformedEnvelope);
        assert!(error.message().contains("co-sign"), "{}", error.message());
    }

    #[test]
    fn fee_bump_and_v0_envelopes_are_refused() {
        let error = inspect_transaction(&testing::fee_bump_envelope(), UnknownCallPolicy::Refuse)
            .unwrap_err();
        assert_eq!(error.reason(), RefusalReason::MalformedEnvelope);
        assert!(error.message().contains("fee-bump"), "{}", error.message());

        let error =
            inspect_transaction(&testing::v0_envelope(), UnknownCallPolicy::Refuse).unwrap_err();
        assert!(error.message().contains("v0"), "{}", error.message());
    }

    #[test]
    fn a_classic_operation_is_refused_rather_than_signed_blind() {
        // A raw Payment moves value with no contract involved, so none of the
        // contract-aware rules would see it.
        let xdr = testing::classic_payment_envelope();
        let error = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::Uninspectable);
        assert!(error.message().contains("Payment"), "{}", error.message());
    }

    #[test]
    fn an_operation_that_overrides_the_source_account_is_refused() {
        // Otherwise one operation could act as an account we never checked.
        let xdr = testing::operation_source_override_envelope();
        let error = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::Uninspectable);
        assert!(
            error.message().contains("source account"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn contract_upload_is_not_something_an_agent_key_signs() {
        let xdr = testing::upload_wasm_envelope();
        let error = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::Uninspectable);
        assert!(
            error.message().contains("UploadContractWasm"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn an_unbounded_envelope_is_decoded_with_no_max_time() {
        // Policy refuses this; inspection just reports it.
        let xdr = testing::payment_envelope(testing::PaymentSpec {
            max_time: 0,
            ..Default::default()
        });
        let (_, inspected) = inspect_transaction(&xdr, UnknownCallPolicy::Refuse).unwrap();
        assert_eq!(inspected.max_time, None);
    }

    #[test]
    fn malformed_xdr_is_an_error_not_a_panic() {
        for input in ["", "not base64", "AAAA", "////"] {
            let error = inspect_transaction(input, UnknownCallPolicy::Refuse).unwrap_err();
            assert_eq!(error.reason(), RefusalReason::MalformedEnvelope);
        }
    }

    #[test]
    fn caller_controlled_text_cannot_inject_control_characters_into_a_log() {
        let nasty = b"line1\nline2\r\x00\x1b[31m";
        let rendered = lossy_text(nasty);
        assert!(!rendered.contains('\n'), "{rendered:?}");
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
    }

    #[test]
    fn caller_controlled_text_is_truncated() {
        let long = vec![b'a'; 10_000];
        let rendered = lossy_text(&long);
        assert!(rendered.len() <= 256 + 4, "{}", rendered.len());
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn an_auth_entry_decodes_its_address_nonce_and_call() {
        let xdr = testing::auth_entry_xdr(7, 25_000_000);
        let (_, inspected) = inspect_auth_entry(&xdr, UnknownCallPolicy::Refuse).unwrap();
        assert_eq!(inspected.nonce, 7);
        assert_eq!(inspected.address, *testing::AGENT);
        let call = inspected.call.unwrap();
        assert_eq!(call.function, "pay");
        assert_eq!(call.amounts(), vec![25_000_000]);
    }

    #[test]
    fn a_source_account_auth_entry_is_refused() {
        let xdr = testing::source_account_auth_entry_xdr();
        let error = inspect_auth_entry(&xdr, UnknownCallPolicy::Refuse).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::MalformedEnvelope);
    }
}
