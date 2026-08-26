//! A minimal Soroban RPC stub, and the client tests that run against it.
//!
//! These exercise the whole invocation pipeline — build, simulate, sign
//! authorisations, assemble, submit, poll — over real HTTP against a local
//! stub, rather than mocking out the client's internals. That distinction
//! matters: a test that stubs `invoke` proves nothing about whether the
//! envelope this SDK produces is one a server would accept, and the pipeline
//! is exactly where the interesting mistakes live (a missing footprint, an
//! unsigned auth entry, a sequence number off by one).
//!
//! The stub is deliberately tiny and hand-rolled: one `TcpListener`, one
//! canned response per JSON-RPC method. Reaching for an HTTP-mocking crate
//! would add a dependency to make a test that is 150 lines of `tokio::net`
//! marginally shorter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use stellaragent::types::{
    Network, NetworkConfig, OpenChannelParams, PayForApiParams, SpendPeriod,
};
use stellaragent::xdr::{
    Int128Parts, Limits, ScVal, SorobanResources, SorobanTransactionData,
    SorobanTransactionDataExt, WriteXdr,
};
use stellaragent::{ErrorCode, StellarAgent};

const SECRET: &str = "SAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYPSBF5K";
const CONTRACT: &str = "CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE";
const PASSPHRASE: &str = "Standalone Network ; February 2017";

// ─── The stub ────────────────────────────────────────────────────────────────

/// Records every JSON-RPC method the client called, in order.
type CallLog = Arc<Mutex<Vec<String>>>;

/// A local JSON-RPC server that replays canned results.
///
/// Each method maps to a queue of results. The last one repeats once the queue
/// is drained, so a test only has to enumerate the *changes* it cares about —
/// `getTransaction` returning NOT_FOUND twice and then SUCCESS, say.
struct MockRpc {
    url: String,
    calls: CallLog,
}

impl MockRpc {
    async fn start(responses: HashMap<&'static str, Vec<Value>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a port");
        let addr = listener.local_addr().expect("local addr");
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));

        let queues: Arc<Mutex<HashMap<String, Vec<Value>>>> = Arc::new(Mutex::new(
            responses
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        ));

        let server_calls = Arc::clone(&calls);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let calls = Arc::clone(&server_calls);
                let queues = Arc::clone(&queues);

                tokio::spawn(async move {
                    let Some(body) = read_http_body(&mut socket).await else {
                        return;
                    };
                    let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let method = request
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    let id = request.get("id").cloned().unwrap_or(json!(1));

                    calls.lock().expect("call log").push(method.clone());

                    let result = {
                        let mut queues = queues.lock().expect("response queues");
                        match queues.get_mut(&method) {
                            // Keep the last response in place so it repeats.
                            Some(queue) if queue.len() > 1 => Some(queue.remove(0)),
                            Some(queue) => queue.first().cloned(),
                            None => None,
                        }
                    };

                    let payload = match result {
                        Some(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        None => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32601, "message": format!("no stub for {method}")}
                        }),
                    };
                    let body = payload.to_string();

                    // `Connection: close` keeps the stub to one request per
                    // socket, which is all the state machine it needs.
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        Self {
            url: format!("http://127.0.0.1:{}", addr.port()),
            calls,
        }
    }

    fn methods_called(&self) -> Vec<String> {
        self.calls.lock().expect("call log").clone()
    }
}

/// Read one HTTP request off the socket and return its body.
async fn read_http_body(socket: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];

    // Headers first, so `Content-Length` is known before reading the body.
    let header_end = loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };

    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);

    while buffer.len() < header_end + content_length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    Some(String::from_utf8_lossy(&buffer[header_end..header_end + content_length]).to_string())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ─── Canned payloads ─────────────────────────────────────────────────────────

/// A funded account entry for the agent's own address, at sequence 41.
fn account_entry(address: &str) -> Value {
    use stellaragent::xdr::{
        AccountEntry, AccountEntryExt, AccountId, LedgerEntryData, SequenceNumber, String32,
        Thresholds,
    };

    let account_id: AccountId = address.parse().expect("a valid account address");
    let entry = LedgerEntryData::Account(AccountEntry {
        account_id,
        balance: 1_234_500_000, // 123.45 XLM in stroops
        seq_num: SequenceNumber(41),
        num_sub_entries: 0,
        inflation_dest: None,
        flags: 0,
        home_domain: String32::default(),
        thresholds: Thresholds([1, 0, 0, 0]),
        signers: Default::default(),
        ext: AccountEntryExt::V0,
    });

    json!({
        "entries": [{
            "key": "",
            "xdr": entry.to_xdr_base64(Limits::none()).expect("encodes"),
            "lastModifiedLedgerSeq": 1000,
        }],
        "latestLedger": 1000,
    })
}

/// An empty-but-valid footprint, which is all the client needs to assemble.
fn transaction_data() -> String {
    SorobanTransactionData {
        ext: SorobanTransactionDataExt::V0,
        resources: SorobanResources {
            footprint: Default::default(),
            instructions: 1_000_000,
            disk_read_bytes: 0,
            write_bytes: 0,
        },
        resource_fee: 12_345,
    }
    .to_xdr_base64(Limits::none())
    .expect("encodes")
}

fn simulate_response(return_value: &ScVal) -> Value {
    json!({
        "latestLedger": 1000,
        "minResourceFee": "12345",
        "transactionData": transaction_data(),
        "results": [{
            "xdr": return_value.to_xdr_base64(Limits::none()).expect("encodes"),
            "auth": [],
        }],
    })
}

fn i128_scval(value: i128) -> ScVal {
    ScVal::I128(Int128Parts {
        hi: (value >> 64) as i64,
        lo: value as u64,
    })
}

/// A channel struct as `PaymentChannel.get_channel` returns it.
fn channel_entry(agent: &str) -> ScVal {
    use stellaragent::scval;

    scval::map(vec![
        ("agent", scval::address(agent).unwrap()),
        ("owner", scval::address(agent).unwrap()),
        ("token", scval::address(CONTRACT).unwrap()),
        ("limit_per_period", i128_scval(100_000_000)),
        ("period", scval::enum_variant("Hourly").unwrap()),
        ("spent_this_period", i128_scval(2_500_000)),
        ("period_start_ledger", scval::u32_value(950)),
        ("total_spent", i128_scval(9_900_000)),
        ("active", scval::bool_value(true)),
    ])
    .expect("a valid channel struct")
}

/// Build an agent pointed at the stub, with every contract set to `CONTRACT`.
async fn agent_against(mock: &MockRpc) -> StellarAgent {
    use stellaragent::contracts::ContractKey;

    let mut builder = StellarAgent::builder()
        .network(Network::Local)
        .network_config(NetworkConfig {
            rpc_url: mock.url.clone(),
            network_passphrase: PASSPHRASE.to_string(),
            horizon_url: mock.url.clone(),
        })
        .secret_key(SECRET);

    for key in ContractKey::ALL {
        builder = builder.contract(key, CONTRACT);
    }
    builder.build().await.expect("agent builds")
}

fn responses(pairs: Vec<(&'static str, Vec<Value>)>) -> HashMap<&'static str, Vec<Value>> {
    pairs.into_iter().collect()
}

// ─── Read-only calls ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_read_only_call_stops_after_simulation() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![simulate_response(&channel_entry(&address))],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let channel = agent.channel(7).await.expect("reads the channel");

    assert_eq!(channel.id, 7);
    assert_eq!(channel.agent, address);
    assert_eq!(channel.limit_per_period, 100_000_000);
    assert_eq!(channel.spent_this_period, 2_500_000);
    assert_eq!(channel.period, SpendPeriod::Hourly);
    assert!(channel.active);

    // Nothing was signed, submitted, or polled: a read-only call must not cost
    // a fee or need a funded account.
    let calls = mock.methods_called();
    assert!(!calls.contains(&"sendTransaction".to_string()), "{calls:?}");
    assert!(!calls.contains(&"getTransaction".to_string()), "{calls:?}");
}

#[tokio::test]
async fn check_rate_limit_returns_the_contracts_boolean() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![simulate_response(&ScVal::Bool(false))],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    assert!(!agent.check_rate_limit("5").await.expect("simulates"));
}

#[tokio::test]
async fn an_unconfigured_rate_limit_reports_itself_rather_than_erroring() {
    // `get_limits` panics on-chain when nothing was ever configured, and that
    // panic is the only signal for `configured: false`.
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![json!({
                "latestLedger": 1000,
                "error": "HostError: Error(Contract, #3), no rate limit for agent",
            })],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let status = agent
        .rate_limit_status(None)
        .await
        .expect("degrades gracefully");
    assert!(!status.configured);
    assert_eq!(status.max_per_tx, "0");
}

#[tokio::test]
async fn the_balance_is_read_from_the_account_entry() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![(
        "getLedgerEntries",
        vec![account_entry(&address)],
    )]))
    .await;

    let agent = agent_against(&mock).await;
    assert_eq!(
        agent.balance().await.expect("reads the balance"),
        "123.4500000"
    );
}

#[tokio::test]
async fn an_account_that_does_not_exist_reports_a_zero_balance() {
    // A freshly generated agent is unfunded, which is a normal state and not
    // worth propagating as an error out of a balance query.
    let mock = MockRpc::start(responses(vec![(
        "getLedgerEntries",
        vec![json!({"entries": [], "latestLedger": 1000})],
    )]))
    .await;

    let agent = agent_against(&mock).await;
    assert_eq!(agent.balance().await.expect("degrades to zero"), "0");
}

// ─── The full mutation pipeline ──────────────────────────────────────────────

#[tokio::test]
async fn opening_a_channel_runs_the_whole_pipeline_and_sets_the_active_channel() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![simulate_response(&ScVal::U64(42))],
        ),
        (
            "sendTransaction",
            vec![json!({"status": "PENDING", "hash": "abc123", "latestLedger": 1000})],
        ),
        (
            "getTransaction",
            vec![
                // The first poll finds nothing — the client must keep going
                // rather than treating NOT_FOUND as a failure.
                json!({"status": "NOT_FOUND", "latestLedger": 1000}),
                json!({
                    "status": "SUCCESS",
                    "latestLedger": 1002,
                    "ledger": 1001,
                    "returnValue": ScVal::U64(42).to_xdr_base64(Limits::none()).unwrap(),
                }),
            ],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let channel_id = agent
        .open_channel(&OpenChannelParams {
            deposit: "10".into(),
            limit_per_period: "1".into(),
            period: SpendPeriod::Hourly,
            token: None,
        })
        .await
        .expect("opens a channel");

    assert_eq!(channel_id, 42);
    assert_eq!(agent.active_channel_id(), Some(42));

    let calls = mock.methods_called();
    assert_eq!(
        calls,
        vec![
            "getLedgerEntries",
            "simulateTransaction",
            "sendTransaction",
            "getTransaction",
            "getTransaction",
        ],
        "the pipeline should build, simulate, submit, then poll to a terminal status"
    );
}

#[tokio::test]
async fn a_payment_draws_from_the_active_channel_without_being_told_which() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        ("simulateTransaction", vec![simulate_response(&ScVal::Void)]),
        (
            "sendTransaction",
            vec![json!({"status": "PENDING", "hash": "payhash", "latestLedger": 1000})],
        ),
        (
            "getTransaction",
            vec![json!({"status": "SUCCESS", "ledger": 1001, "latestLedger": 1002})],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    agent.set_active_channel_id(Some(3));

    let result = agent
        .pay_for_api(&PayForApiParams {
            endpoint: "https://api.example.com/inference".into(),
            amount: "0.001".into(),
            asset: Some("XLM".into()),
            channel_id: None,
            recipient: None,
            dest_asset: None,
            min_received: None,
        })
        .await
        .expect("pays");

    assert!(result.success);
    assert_eq!(result.hash, "payhash");
    assert_eq!(result.ledger, Some(1001));
}

#[tokio::test]
async fn a_duplicate_submission_is_treated_as_accepted() {
    // DUPLICATE means the same envelope was already queued — polling for it is
    // the right move, not reporting a failure.
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        ("simulateTransaction", vec![simulate_response(&ScVal::Void)]),
        (
            "sendTransaction",
            vec![json!({"status": "DUPLICATE", "hash": "dup", "latestLedger": 1000})],
        ),
        (
            "getTransaction",
            vec![json!({"status": "SUCCESS", "ledger": 1001, "latestLedger": 1002})],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    assert!(agent.release_payment(1).await.expect("releases").success);
}

// ─── Failure paths ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_spend_limit_panic_is_classified_rather_than_surfaced_as_prose() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![json!({
                "latestLedger": 1000,
                "error": "HostError: Error(Contract, #2), PaymentChannel: spend limit exceeded",
            })],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    agent.set_active_channel_id(Some(1));

    let error = agent
        .pay_for_api(&PayForApiParams {
            endpoint: "https://api.example.com".into(),
            amount: "1000000".into(),
            asset: None,
            channel_id: None,
            recipient: None,
            dest_asset: None,
            min_received: None,
        })
        .await
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::SpendLimitExceeded);
    assert!(
        !error.is_retryable(),
        "retrying will keep failing until the window rolls over"
    );
    assert!(error.message().starts_with("pay:"), "{}", error.message());
}

#[tokio::test]
async fn a_rejected_submission_names_the_transaction_it_failed_for() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        ("simulateTransaction", vec![simulate_response(&ScVal::Void)]),
        (
            "sendTransaction",
            vec![json!({
                "status": "ERROR",
                "hash": "rejected-hash",
                "latestLedger": 1000,
                "errorResultXdr": "AAAAAAAAAGT////9AAAAAA==",
            })],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let error = agent.accept_job(5).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::SubmissionFailed);
    assert_eq!(error.transaction_hash(), Some("rejected-hash"));
}

#[tokio::test]
async fn an_included_but_failed_transaction_is_reported_with_its_hash() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        ("simulateTransaction", vec![simulate_response(&ScVal::Void)]),
        (
            "sendTransaction",
            vec![json!({"status": "PENDING", "hash": "failhash", "latestLedger": 1000})],
        ),
        (
            "getTransaction",
            vec![json!({"status": "FAILED", "ledger": 1001, "latestLedger": 1002})],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let error = agent.accept_job(5).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::TransactionFailed);
    assert_eq!(error.transaction_hash(), Some("failhash"));
}

#[tokio::test]
async fn a_json_rpc_error_object_becomes_a_retryable_network_error() {
    // No stub is registered for `getLedgerEntries`, so the server replies with
    // a JSON-RPC error — the shape a real server uses for an internal failure.
    let mock = MockRpc::start(responses(vec![])).await;
    let agent = agent_against(&mock).await;

    let error = agent.channel(1).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::NetworkError);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn a_simulation_needing_an_archive_restore_says_so() {
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![json!({
                "latestLedger": 1000,
                "minResourceFee": "1",
                "transactionData": transaction_data(),
                "restorePreamble": {"minResourceFee": "500", "transactionData": transaction_data()},
            })],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let error = agent.channel(1).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::SimulationFailed);
    assert!(error.message().contains("archived"), "{}", error.message());
}

#[tokio::test]
async fn a_simulation_with_no_footprint_is_refused_before_submission() {
    // Submitting a Soroban transaction with no `transactionData` gets
    // `txSorobanInvalid` back from the network; failing here says why.
    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![json!({"latestLedger": 1000, "minResourceFee": "1", "results": []})],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let error = agent.accept_job(1).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::SimulationFailed);
    assert!(error.message().contains("footprint"), "{}", error.message());
}

// ─── Decoding ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_job_decodes_every_field_including_its_optionals() {
    use stellaragent::scval;
    use stellaragent::types::JobStatus;

    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let job = scval::map(vec![
        ("requester", scval::address(&address).unwrap()),
        ("worker", scval::void()),
        ("arbiter", scval::address(&address).unwrap()),
        ("token", scval::address(CONTRACT).unwrap()),
        ("amount", i128_scval(500_000)),
        (
            "task_description",
            scval::bytes_from_str("summarise this").unwrap(),
        ),
        ("result", scval::void()),
        ("deadline_ledger", scval::u32_value(1720)),
        ("status", scval::enum_variant("InProgress").unwrap()),
        ("created_at", scval::u32_value(1000)),
    ])
    .unwrap();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        ("simulateTransaction", vec![simulate_response(&job)]),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let decoded = agent.job(11).await.expect("decodes the job");

    assert_eq!(decoded.id, 11);
    assert_eq!(decoded.status, JobStatus::InProgress);
    assert_eq!(decoded.worker, None);
    assert_eq!(decoded.arbiter, Some(address));
    assert_eq!(decoded.result, None);
    assert_eq!(decoded.task_description, "summarise this");
    assert_eq!(decoded.amount, 500_000);
}

#[tokio::test]
async fn a_struct_missing_a_field_names_it_rather_than_returning_a_default() {
    use stellaragent::scval;

    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    // A contract one version behind, with no `total_spent`.
    let partial = scval::map(vec![
        ("agent", scval::address(&address).unwrap()),
        ("owner", scval::address(&address).unwrap()),
        ("token", scval::address(CONTRACT).unwrap()),
        ("limit_per_period", i128_scval(1)),
        ("period", scval::enum_variant("Daily").unwrap()),
        ("spent_this_period", i128_scval(0)),
        ("period_start_ledger", scval::u32_value(1)),
        ("active", scval::bool_value(true)),
    ])
    .unwrap();

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        ("simulateTransaction", vec![simulate_response(&partial)]),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let error = agent.channel(1).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::ContractError);
    assert!(
        error.message().contains("total_spent"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn an_unknown_enum_variant_is_reported_rather_than_guessed() {
    use stellaragent::scval;

    let address = stellaragent::signer::KeypairSigner::from_secret(SECRET)
        .unwrap()
        .address();

    let mut entries = vec![
        ("agent", scval::address(&address).unwrap()),
        ("owner", scval::address(&address).unwrap()),
        ("token", scval::address(CONTRACT).unwrap()),
        ("limit_per_period", i128_scval(1)),
        ("spent_this_period", i128_scval(0)),
        ("period_start_ledger", scval::u32_value(1)),
        ("total_spent", i128_scval(0)),
        ("active", scval::bool_value(true)),
    ];
    entries.push(("period", scval::enum_variant("Weekly").unwrap()));

    let mock = MockRpc::start(responses(vec![
        ("getLedgerEntries", vec![account_entry(&address)]),
        (
            "simulateTransaction",
            vec![simulate_response(&scval::map(entries).unwrap())],
        ),
    ]))
    .await;

    let agent = agent_against(&mock).await;
    let error = agent.channel(1).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::ContractError);
    assert!(error.message().contains("Weekly"), "{}", error.message());
}

// ─── Ledger-time estimation ──────────────────────────────────────────────────

#[tokio::test]
async fn the_ledger_close_estimate_is_derived_from_horizons_samples() {
    // Horizon is not JSON-RPC, so the stub answers it as an unknown method —
    // which is a JSON body with no `_embedded`, i.e. no samples. Point the
    // agent at a stub that does serve records instead.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let _ = read_http_body(&mut socket).await;
            let body = json!({
                "_embedded": {"records": [
                    {"sequence": 100, "closed_at": "2024-01-01T00:00:00Z"},
                    {"sequence": 110, "closed_at": "2024-01-01T00:01:00Z"},
                ]}
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    let agent = StellarAgent::builder()
        .network(Network::Local)
        .network_config(NetworkConfig {
            rpc_url: format!("http://127.0.0.1:{}", addr.port()),
            network_passphrase: PASSPHRASE.to_string(),
            horizon_url: format!("http://127.0.0.1:{}", addr.port()),
        })
        .secret_key(SECRET)
        .allow_unconfigured_contracts(true)
        .build()
        .await
        .unwrap();

    let estimate = agent.ledger_close_estimate().await.expect("estimates");
    assert_eq!(estimate.current_ledger, 110);
    assert_eq!(estimate.avg_ledger_close_seconds, 6.0);
    assert!(
        estimate.observed,
        "two samples is a measurement, not a guess"
    );
}
