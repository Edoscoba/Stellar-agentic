//! Envelope builders for tests and the conformance suite.
//!
//! Compiled unconditionally rather than behind `#[cfg(test)]`: the integration
//! and adversarial suites in `tests/` need these, and so does anyone standing
//! the service up against a fixture. Nothing here is used by the request path.
//!
//! Every address is derived deterministically from a fixed seed, so a failing
//! assertion names the same address on every machine and in every CI run.

use std::sync::LazyLock;

use ed25519_dalek::{Signer, SigningKey};
use stellar_xdr::curr::{
    AccountId, Asset, ContractId, DecoratedSignature, Hash, HostFunction, Int128Parts,
    InvokeContractArgs, InvokeHostFunctionOp, Limits, Memo, MuxedAccount, Operation, OperationBody,
    PaymentOp, Preconditions, ScAddress, ScBytes, ScSymbol, ScVal, SequenceNumber, Signature,
    SignatureHint, SorobanAddressCredentials, SorobanAuthorizationEntry, SorobanAuthorizedFunction,
    SorobanAuthorizedInvocation, SorobanCredentials, TimeBounds, TimePoint, Transaction,
    TransactionEnvelope, TransactionExt, TransactionV0, TransactionV0Envelope, TransactionV0Ext,
    TransactionV1Envelope, Uint256, WriteXdr,
};

/// The network these fixtures are built for.
pub const NETWORK_PASSPHRASE: &str = "Test SDF Network ; September 2015";

/// The seed the agent's key is derived from.
pub const AGENT_SEED: [u8; 32] = [1u8; 32];

fn address_for(seed: [u8; 32]) -> String {
    let key = SigningKey::from_bytes(&seed);
    stellar_strkey::ed25519::PublicKey(key.verifying_key().to_bytes()).to_string()
}

/// The agent this service signs for in fixtures.
pub static AGENT: LazyLock<String> = LazyLock::new(|| address_for(AGENT_SEED));

/// A recipient address, on the allowlist in fixture policies.
pub static RECIPIENT: LazyLock<String> = LazyLock::new(|| address_for([2u8; 32]));

/// A second recipient, deliberately *not* on fixture allowlists.
pub static STRANGER: LazyLock<String> = LazyLock::new(|| address_for([3u8; 32]));

/// A contract address for fixtures.
pub static CONTRACT: LazyLock<String> =
    LazyLock::new(|| stellar_strkey::Contract([9u8; 32]).to_string());

fn account(address: &str) -> MuxedAccount {
    address.parse().expect("a valid account address")
}

fn contract_address() -> ScAddress {
    ScAddress::Contract(ContractId(Hash([9u8; 32])))
}

fn symbol(name: &str) -> ScSymbol {
    ScSymbol(name.try_into().expect("a symbol under 32 characters"))
}

fn i128_val(value: i128) -> ScVal {
    ScVal::I128(Int128Parts {
        hi: (value >> 64) as i64,
        lo: value as u64,
    })
}

fn address_val(address: &str) -> ScVal {
    ScVal::Address(address.parse().expect("a valid address"))
}

fn bytes_val(text: &str) -> ScVal {
    ScVal::Bytes(ScBytes(
        text.as_bytes().to_vec().try_into().expect("short enough"),
    ))
}

fn envelope(transaction: Transaction) -> String {
    TransactionEnvelope::Tx(TransactionV1Envelope {
        tx: transaction,
        signatures: Default::default(),
    })
    .to_xdr_base64(Limits::none())
    .expect("encodes")
}

/// The knobs a fixture payment exposes.
#[derive(Debug, Clone)]
pub struct PaymentSpec {
    /// Amount in stroops.
    pub amount: i128,
    /// Who is paid.
    pub recipient: String,
    /// Transaction source account.
    pub source: String,
    /// Upper time bound; `0` means unbounded.
    pub max_time: u64,
    /// The endpoint recorded with the payment.
    pub endpoint: String,
}

impl Default for PaymentSpec {
    fn default() -> Self {
        Self {
            amount: 10_000_000,
            recipient: RECIPIENT.clone(),
            source: AGENT.clone(),
            max_time: 2_000_000_000,
            endpoint: "https://api.example.com/inference".into(),
        }
    }
}

fn transaction_with(source: &str, max_time: u64, operations: Vec<Operation>) -> Transaction {
    Transaction {
        source_account: account(source),
        fee: 100,
        seq_num: SequenceNumber(42),
        cond: if max_time == 0 {
            Preconditions::None
        } else {
            Preconditions::Time(TimeBounds {
                min_time: TimePoint(0),
                max_time: TimePoint(max_time),
            })
        },
        memo: Memo::None,
        operations: operations.try_into().expect("at most 100 operations"),
        ext: TransactionExt::V0,
    }
}

fn invoke_operation(function: &str, args: Vec<ScVal>) -> Operation {
    Operation {
        source_account: None,
        body: OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function: HostFunction::InvokeContract(InvokeContractArgs {
                contract_address: contract_address(),
                function_name: symbol(function),
                args: args.try_into().expect("a reasonable argument count"),
            }),
            auth: Default::default(),
        }),
    }
}

/// A `payment_channel.pay` envelope.
pub fn payment_envelope(spec: PaymentSpec) -> String {
    let operation = invoke_operation(
        "pay",
        vec![
            address_val(&AGENT),
            ScVal::U64(1),
            address_val(&spec.recipient),
            i128_val(spec.amount),
            bytes_val(&spec.endpoint),
        ],
    );
    envelope(transaction_with(
        &spec.source,
        spec.max_time,
        vec![operation],
    ))
}

/// A `rate_limiter.set_limits` envelope — three `i128` ceilings, no spend.
pub fn set_limits_envelope(ceiling: i128) -> String {
    let operation = invoke_operation(
        "set_limits",
        vec![
            address_val(&AGENT),
            address_val(&AGENT),
            i128_val(ceiling),
            i128_val(ceiling),
            i128_val(ceiling),
            ScVal::U32(100),
        ],
    );
    envelope(transaction_with(&AGENT, 2_000_000_000, vec![operation]))
}

/// A call to a function no spec covers.
pub fn unknown_call_envelope(function: &str, value: i128) -> String {
    let operation = invoke_operation(function, vec![address_val(&RECIPIENT), i128_val(value)]);
    envelope(transaction_with(&AGENT, 2_000_000_000, vec![operation]))
}

/// `pay` with one argument too few — a contract and a spec that disagree.
pub fn wrong_arity_pay_envelope() -> String {
    let operation = invoke_operation("pay", vec![address_val(&AGENT), ScVal::U64(1), i128_val(1)]);
    envelope(transaction_with(&AGENT, 2_000_000_000, vec![operation]))
}

/// An otherwise valid envelope that already carries a signature.
pub fn pre_signed_envelope() -> String {
    let transaction = transaction_with(
        &AGENT,
        2_000_000_000,
        vec![invoke_operation(
            "pay",
            vec![
                address_val(&AGENT),
                ScVal::U64(1),
                address_val(&RECIPIENT),
                i128_val(1),
                bytes_val("x"),
            ],
        )],
    );
    let key = SigningKey::from_bytes(&AGENT_SEED);
    let signature = DecoratedSignature {
        hint: SignatureHint([0, 0, 0, 0]),
        signature: Signature(
            key.sign(b"anything")
                .to_bytes()
                .to_vec()
                .try_into()
                .expect("64 bytes"),
        ),
    };
    TransactionEnvelope::Tx(TransactionV1Envelope {
        tx: transaction,
        signatures: vec![signature].try_into().expect("one signature"),
    })
    .to_xdr_base64(Limits::none())
    .expect("encodes")
}

/// A fee-bump envelope wrapping someone else's transaction.
pub fn fee_bump_envelope() -> String {
    use stellar_xdr::curr::{
        FeeBumpTransaction, FeeBumpTransactionEnvelope, FeeBumpTransactionExt,
        FeeBumpTransactionInnerTx,
    };

    let inner = TransactionV1Envelope {
        tx: transaction_with(
            &AGENT,
            2_000_000_000,
            vec![invoke_operation(
                "pay",
                vec![
                    address_val(&AGENT),
                    ScVal::U64(1),
                    address_val(&RECIPIENT),
                    i128_val(1),
                    bytes_val("x"),
                ],
            )],
        ),
        signatures: Default::default(),
    };

    TransactionEnvelope::TxFeeBump(FeeBumpTransactionEnvelope {
        tx: FeeBumpTransaction {
            fee_source: account(&AGENT),
            fee: 1_000,
            inner_tx: FeeBumpTransactionInnerTx::Tx(inner),
            ext: FeeBumpTransactionExt::V0,
        },
        signatures: Default::default(),
    })
    .to_xdr_base64(Limits::none())
    .expect("encodes")
}

/// A pre-Protocol-13 v0 envelope.
pub fn v0_envelope() -> String {
    let key = SigningKey::from_bytes(&AGENT_SEED);
    TransactionEnvelope::TxV0(TransactionV0Envelope {
        tx: TransactionV0 {
            source_account_ed25519: Uint256(key.verifying_key().to_bytes()),
            fee: 100,
            seq_num: SequenceNumber(1),
            time_bounds: None,
            memo: Memo::None,
            operations: vec![Operation {
                source_account: None,
                body: OperationBody::Inflation,
            }]
            .try_into()
            .expect("one operation"),
            ext: TransactionV0Ext::V0,
        },
        signatures: Default::default(),
    })
    .to_xdr_base64(Limits::none())
    .expect("encodes")
}

/// A classic `Payment` operation — value moving with no contract involved.
pub fn classic_payment_envelope() -> String {
    let operation = Operation {
        source_account: None,
        body: OperationBody::Payment(PaymentOp {
            destination: account(&STRANGER),
            asset: Asset::Native,
            amount: 100_000_000,
        }),
    };
    envelope(transaction_with(&AGENT, 2_000_000_000, vec![operation]))
}

/// An operation that overrides the transaction's source account.
pub fn operation_source_override_envelope() -> String {
    let mut operation = invoke_operation(
        "pay",
        vec![
            address_val(&AGENT),
            ScVal::U64(1),
            address_val(&RECIPIENT),
            i128_val(1),
            bytes_val("x"),
        ],
    );
    operation.source_account = Some(account(&STRANGER));
    envelope(transaction_with(&AGENT, 2_000_000_000, vec![operation]))
}

/// An envelope uploading contract WASM.
pub fn upload_wasm_envelope() -> String {
    let operation = Operation {
        source_account: None,
        body: OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function: HostFunction::UploadContractWasm(
                vec![0u8; 8].try_into().expect("short wasm"),
            ),
            auth: Default::default(),
        }),
    };
    envelope(transaction_with(&AGENT, 2_000_000_000, vec![operation]))
}

/// An envelope carrying `count` payment operations.
pub fn multi_payment_envelope(count: usize, amount_each: i128) -> String {
    let operations: Vec<Operation> = (0..count)
        .map(|_| {
            invoke_operation(
                "pay",
                vec![
                    address_val(&AGENT),
                    ScVal::U64(1),
                    address_val(&RECIPIENT),
                    i128_val(amount_each),
                    bytes_val("https://api.example.com"),
                ],
            )
        })
        .collect();
    envelope(transaction_with(&AGENT, 2_000_000_000, operations))
}

/// A `SorobanAuthorizationEntry` authorising a payment.
pub fn auth_entry_xdr(nonce: i64, amount: i128) -> String {
    SorobanAuthorizationEntry {
        credentials: SorobanCredentials::Address(SorobanAddressCredentials {
            address: AGENT.parse().expect("a valid address"),
            nonce,
            signature_expiration_ledger: 0,
            signature: ScVal::Void,
        }),
        root_invocation: SorobanAuthorizedInvocation {
            function: SorobanAuthorizedFunction::ContractFn(InvokeContractArgs {
                contract_address: contract_address(),
                function_name: symbol("pay"),
                args: vec![
                    address_val(&AGENT),
                    ScVal::U64(1),
                    address_val(&RECIPIENT),
                    i128_val(amount),
                    bytes_val("https://api.example.com"),
                ]
                .try_into()
                .expect("five arguments"),
            }),
            sub_invocations: Default::default(),
        },
    }
    .to_xdr_base64(Limits::none())
    .expect("encodes")
}

/// An auth entry using source-account credentials, which needs no signature.
pub fn source_account_auth_entry_xdr() -> String {
    SorobanAuthorizationEntry {
        credentials: SorobanCredentials::SourceAccount,
        root_invocation: SorobanAuthorizedInvocation {
            function: SorobanAuthorizedFunction::ContractFn(InvokeContractArgs {
                contract_address: contract_address(),
                function_name: symbol("pay"),
                args: Default::default(),
            }),
            sub_invocations: Default::default(),
        },
    }
    .to_xdr_base64(Limits::none())
    .expect("encodes")
}

/// The account id form of the agent address, for building ledger fixtures.
pub fn agent_account_id() -> AccountId {
    AGENT.parse().expect("a valid account address")
}
