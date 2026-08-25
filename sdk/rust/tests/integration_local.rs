//! The full payment lifecycle against a real local network.
//!
//! Off by default and behind two gates, because it needs a running network and
//! deployed contracts that CI does not have:
//!
//! ```bash
//! # 1. Start a standalone network (quickstart or `stellar container start local`)
//! # 2. Deploy the contracts and export their addresses:
//! pnpm deploy:contracts --network local
//! export STELLARAGENT_LOCAL_PAYMENT_CHANNEL=C...
//! export STELLARAGENT_LOCAL_ESCROW=C...
//! export STELLARAGENT_LOCAL_RATE_LIMITER=C...
//! export STELLARAGENT_LOCAL_AGENT_WALLET_FACTORY=C...
//! export STELLARAGENT_LOCAL_CIRCUIT_BREAKER=C...
//! # 3. A funded account on that network:
//! export STELLARAGENT_LOCAL_SECRET=S...
//!
//! cargo test --features integration -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` is not optional: every test here submits from the same
//! account, and two transactions built concurrently would claim the same
//! sequence number, so one of them would be rejected with `txBadSeq`.
//!
//! # Why this is not in the default `cargo test`
//!
//! CI has to stay hermetic. A suite that silently passes when the network is
//! absent is worse than one that is not run at all — it reports a green tick
//! for coverage nobody has. So these are `#[ignore]`d *and* compiled out
//! without the `integration` feature, and they fail loudly rather than skipping
//! when the environment they need is only half-configured.

#![cfg(feature = "integration")]

use std::env;

use stellaragent::types::{
    Network, NetworkConfig, OpenChannelParams, PayForApiParams, RateLimitConfig, RequestWorkParams,
    SpendPeriod,
};
use stellaragent::{ErrorCode, StellarAgent};

/// Read an environment variable, failing the test with instructions rather
/// than skipping. A skipped integration test is indistinguishable from a
/// passing one in a CI summary.
fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is not set. See the module docs in tests/integration_local.rs for the \
             full setup — this suite deliberately fails rather than skipping, so a \
             half-configured environment cannot look like a pass."
        )
    })
}

/// An agent against the local network, resolving contracts from the
/// `STELLARAGENT_LOCAL_*` environment variables the deploy script prints.
async fn local_agent() -> StellarAgent {
    let secret = required_env("STELLARAGENT_LOCAL_SECRET");

    let config = env::var("STELLARAGENT_LOCAL_RPC_URL")
        .ok()
        .map(|rpc_url| NetworkConfig {
            rpc_url,
            network_passphrase: env::var("STELLARAGENT_LOCAL_PASSPHRASE")
                .unwrap_or_else(|_| Network::Local.config().network_passphrase),
            horizon_url: env::var("STELLARAGENT_LOCAL_HORIZON_URL")
                .unwrap_or_else(|_| Network::Local.config().horizon_url),
        })
        .unwrap_or_else(|| Network::Local.config());

    let mut builder = StellarAgent::builder()
        .network(Network::Local)
        .network_config(config)
        .secret_key(secret);

    if let Ok(usdc) = env::var("STELLARAGENT_LOCAL_USDC") {
        builder = builder.asset_contract("USDC", usdc);
    }

    builder
        .build()
        .await
        .expect("the local agent should build — check the STELLARAGENT_LOCAL_* variables")
}

#[tokio::test]
#[ignore = "needs a local network; run with --features integration -- --ignored"]
async fn the_network_is_reachable_and_the_account_is_funded() {
    let agent = local_agent().await;

    let ledger = agent
        .rpc()
        .latest_ledger()
        .await
        .expect("the local RPC should answer");
    assert!(
        ledger.sequence > 0,
        "a live network has closed at least one ledger"
    );

    let balance = agent.balance().await.expect("balance query");
    assert_ne!(
        balance, "0",
        "the account must be funded before it can submit anything"
    );
}

#[tokio::test]
#[ignore = "needs a local network; run with --features integration -- --ignored"]
async fn the_full_payment_lifecycle_runs_end_to_end() {
    // This is the definition-of-done test: open a channel, pay through it,
    // read the accounting back, and close it.
    let agent = local_agent().await;

    let channel_id = agent
        .open_channel(&OpenChannelParams {
            deposit: "10".into(),
            limit_per_period: "5".into(),
            period: SpendPeriod::Hourly,
            token: None,
        })
        .await
        .expect("opens a channel");

    assert_eq!(agent.active_channel_id(), Some(channel_id));

    let channel = agent
        .channel(channel_id)
        .await
        .expect("reads the channel back");
    assert!(channel.active);
    assert_eq!(channel.agent, agent.address());
    assert_eq!(channel.period, SpendPeriod::Hourly);
    assert_eq!(channel.spent_this_period, 0);

    let payment = agent
        .pay_for_api(&PayForApiParams {
            endpoint: "https://api.example.com/inference".into(),
            amount: "0.001".into(),
            asset: None,
            channel_id: None,
            recipient: None,
            dest_asset: None,
            min_received: None,
        })
        .await
        .expect("pays through the channel");

    assert!(payment.success);
    assert!(!payment.hash.is_empty());
    assert!(
        payment.ledger.is_some(),
        "a settled payment names its ledger"
    );

    let report = agent.spend_report().await.expect("reads the spend report");
    assert_eq!(report.spent_this_period, "0.0010000");
    assert_eq!(report.remaining_this_period, "4.9990000");

    let closed = agent
        .close_channel(Some(channel_id))
        .await
        .expect("closes the channel");
    assert!(closed.success);
    assert_eq!(agent.active_channel_id(), None);
}

#[tokio::test]
#[ignore = "needs a local network; run with --features integration -- --ignored"]
async fn a_payment_over_the_limit_is_refused_by_the_chain() {
    // The same boundary `math::is_within_spend_limit` predicts off-chain. If
    // these two ever disagree, the off-chain predictor is lying to callers.
    let agent = local_agent().await;

    let channel_id = agent
        .open_channel(&OpenChannelParams {
            deposit: "10".into(),
            limit_per_period: "1".into(),
            period: SpendPeriod::Hourly,
            token: None,
        })
        .await
        .expect("opens a channel");

    let error = agent
        .pay_for_api(&PayForApiParams {
            endpoint: "https://api.example.com/inference".into(),
            amount: "2".into(),
            asset: None,
            channel_id: Some(channel_id),
            recipient: None,
            dest_asset: None,
            min_received: None,
        })
        .await
        .expect_err("2 XLM against a 1 XLM limit must be refused");

    assert_eq!(error.code(), ErrorCode::SpendLimitExceeded);

    let _ = agent.close_channel(Some(channel_id)).await;
}

#[tokio::test]
#[ignore = "needs a local network; run with --features integration -- --ignored"]
async fn the_off_chain_predictor_agrees_with_the_chain() {
    use stellaragent::math::{predict_payment_outcome, ChannelSpendState, PredictPaymentParams};

    let agent = local_agent().await;
    let channel_id = agent
        .open_channel(&OpenChannelParams {
            deposit: "10".into(),
            limit_per_period: "1".into(),
            period: SpendPeriod::Hourly,
            token: None,
        })
        .await
        .expect("opens a channel");

    let channel = agent.channel(channel_id).await.expect("reads the channel");
    let current_ledger = agent.rpc().latest_ledger().await.expect("ledger").sequence;

    let state = ChannelSpendState {
        active: channel.active,
        limit_per_period: channel.limit_per_period.to_string(),
        spent_this_period: channel.spent_this_period.to_string(),
        period_start_ledger: channel.period_start_ledger,
        period: channel.period,
    };

    // 2 XLM is 20,000,000 stroops, against a 10,000,000 stroop limit.
    let prediction = predict_payment_outcome(PredictPaymentParams {
        channel_state: Some(&state),
        rate_limit_state: None,
        amount: "20000000",
        current_ledger,
    })
    .expect("predicts");
    assert!(
        prediction.would_block,
        "the predictor should see the overrun"
    );

    let chain_result = agent
        .pay_for_api(&PayForApiParams {
            endpoint: "https://api.example.com/inference".into(),
            amount: "2".into(),
            asset: None,
            channel_id: Some(channel_id),
            recipient: None,
            dest_asset: None,
            min_received: None,
        })
        .await;
    assert!(
        chain_result.is_err(),
        "the chain must agree with the off-chain prediction"
    );

    let _ = agent.close_channel(Some(channel_id)).await;
}

#[tokio::test]
#[ignore = "needs a local network; run with --features integration -- --ignored"]
async fn an_escrow_job_can_be_created_and_read_back() {
    let agent = local_agent().await;

    let job_id = agent
        .request_work(&RequestWorkParams {
            worker_agent: agent.address().to_string(),
            task: "summarise ipfs://Qm...".into(),
            escrow_amount: "0.05".into(),
            asset: None,
            deadline_ledgers: Some(720),
            arbiter: None,
        })
        .await
        .expect("creates a job");

    let job = agent.job(job_id).await.expect("reads the job back");
    assert_eq!(job.id, job_id);
    assert_eq!(job.requester, agent.address());
    assert_eq!(job.task_description, "summarise ipfs://Qm...");
    assert_eq!(job.worker, None, "a newly posted job has no worker yet");
    assert!(job.deadline_ledger > 0);
}

#[tokio::test]
#[ignore = "needs a local network; run with --features integration -- --ignored"]
async fn rate_limits_can_be_set_and_read_back() {
    let agent = local_agent().await;

    agent
        .set_rate_limits(&RateLimitConfig {
            max_per_tx: "1".into(),
            max_per_hour: "5".into(),
            max_per_day: "20".into(),
            max_txs_per_hour: 100,
        })
        .await
        .expect("sets limits");

    let status = agent.rate_limit_status(None).await.expect("reads limits");
    assert!(status.configured);
    assert_eq!(status.max_per_tx, "1.0000000");
    assert_eq!(status.max_per_hour, "5.0000000");
    assert_eq!(status.max_txs_per_hour, 100);

    // The contract's own view of a payment, for comparison with the local one.
    assert!(agent.check_rate_limit("0.5").await.expect("checks"));
    assert!(
        !agent.check_rate_limit("2").await.expect("checks"),
        "over max_per_tx"
    );
}

#[tokio::test]
#[ignore = "needs a local network; run with --features integration -- --ignored"]
async fn the_ledger_close_estimate_reflects_the_real_network() {
    let agent = local_agent().await;
    let estimate = agent
        .ledger_close_estimate()
        .await
        .expect("Horizon should answer on a local network");

    assert!(estimate.current_ledger > 0);
    assert!(
        estimate.avg_ledger_close_seconds > 0.0,
        "a measured close time is positive"
    );
}
