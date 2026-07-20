//! Integration tests spanning CircuitBreaker + PaymentChannel + Escrow.
//!
//! Verifies the actual emergency-stop mechanism: once a quorum of trusted
//! nodes pauses the system, PaymentChannel.pay and Escrow.release both
//! refuse to operate, and once a quorum unpauses it, normal operation
//! resumes.

use circuit_breaker::{CircuitBreaker, CircuitBreakerClient};
use escrow::{Escrow, EscrowClient};
use payment_channel::{PaymentChannel, PaymentChannelClient, SpendPeriod};
use soroban_sdk::{testutils::Address as _, token, Address, Bytes, Env, Vec};

const WINDOW: u32 = 1_000;

struct Harness<'a> {
    env: Env,
    cb: CircuitBreakerClient<'a>,
    channel: PaymentChannelClient<'a>,
    escrow: EscrowClient<'a>,
    nodes: Vec<Address>,
    token: Address,
}

fn setup<'a>() -> Harness<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);

    let mut nodes = Vec::new(&env);
    for _ in 0..5 {
        nodes.push_back(Address::generate(&env));
    }

    let cb_id = env.register(CircuitBreaker, ());
    let cb = CircuitBreakerClient::new(&env, &cb_id);
    cb.initialize(&admin, &nodes, &WINDOW);

    let channel_id = env.register(PaymentChannel, ());
    let channel = PaymentChannelClient::new(&env, &channel_id);
    channel.set_circuit_breaker(&admin, &cb_id);

    let escrow_id = env.register(Escrow, ());
    let escrow = EscrowClient::new(&env, &escrow_id);
    escrow.set_circuit_breaker(&admin, &cb_id);

    let token_admin = Address::generate(&env);
    let token_contract = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token = token_contract.address();
    let token_asset_client = token::StellarAssetClient::new(&env, &token);
    token_asset_client.mint(&token_admin, &1_000_000);

    Harness {
        env,
        cb,
        channel,
        escrow,
        nodes,
        token,
    }
}

fn trigger_pause(h: &Harness) {
    for node in h.nodes.iter() {
        h.cb.propose_pause(&node);
    }
    h.cb.execute_pause();
}

fn trigger_unpause(h: &Harness) {
    for node in h.nodes.iter() {
        h.cb.propose_unpause(&node);
    }
    h.cb.unpause();
}

fn open_channel(h: &Harness, owner: &Address, agent: &Address) -> u64 {
    let token_asset_client = token::StellarAssetClient::new(&h.env, &h.token);
    token_asset_client.mint(owner, &1_000_000);

    h.channel.open_channel(
        owner,
        agent,
        &h.token,
        &1_000_000,
        &500_000,
        &SpendPeriod::Daily,
    )
}

fn create_job(h: &Harness, requester: &Address, worker: &Address) -> u64 {
    let token_asset_client = token::StellarAssetClient::new(&h.env, &h.token);
    token_asset_client.mint(requester, &1_000_000);

    let deadline = h.env.ledger().sequence() + 10_000;
    let job_id = h.escrow.create_job(
        requester,
        &h.token,
        &100_000,
        &Bytes::new(&h.env),
        &deadline,
        &None,
    );
    h.escrow.accept_job(worker, &job_id);
    h.escrow.submit_result(worker, &job_id, &Bytes::new(&h.env));
    job_id
}

#[test]
fn payment_succeeds_before_pause() {
    let h = setup();
    let owner = Address::generate(&h.env);
    let agent = Address::generate(&h.env);
    let recipient = Address::generate(&h.env);

    let channel_id = open_channel(&h, &owner, &agent);
    h.channel
        .pay(&agent, &channel_id, &recipient, &1_000, &Bytes::new(&h.env));
}

#[test]
#[should_panic(expected = "system paused")]
fn payment_fails_once_quorum_pause_triggered() {
    let h = setup();
    let owner = Address::generate(&h.env);
    let agent = Address::generate(&h.env);
    let recipient = Address::generate(&h.env);

    let channel_id = open_channel(&h, &owner, &agent);

    trigger_pause(&h);
    assert!(h.cb.is_paused());

    h.channel
        .pay(&agent, &channel_id, &recipient, &1_000, &Bytes::new(&h.env));
}

#[test]
#[should_panic(expected = "system paused")]
fn escrow_release_fails_once_quorum_pause_triggered() {
    let h = setup();
    let requester = Address::generate(&h.env);
    let worker = Address::generate(&h.env);

    let job_id = create_job(&h, &requester, &worker);

    trigger_pause(&h);

    h.escrow.release(&requester, &job_id);
}

#[test]
fn payment_succeeds_again_after_quorum_unpause() {
    let h = setup();
    let owner = Address::generate(&h.env);
    let agent = Address::generate(&h.env);
    let recipient = Address::generate(&h.env);

    let channel_id = open_channel(&h, &owner, &agent);

    trigger_pause(&h);
    assert!(h.cb.is_paused());

    trigger_unpause(&h);
    assert!(!h.cb.is_paused());

    // Provably succeeds again post quorum-unpause.
    h.channel
        .pay(&agent, &channel_id, &recipient, &1_000, &Bytes::new(&h.env));

    let channel = h.channel.get_channel(&channel_id);
    assert_eq!(channel.total_spent, 1_000);
}

#[test]
fn escrow_release_succeeds_again_after_quorum_unpause() {
    let h = setup();
    let requester = Address::generate(&h.env);
    let worker = Address::generate(&h.env);

    let job_id = create_job(&h, &requester, &worker);

    trigger_pause(&h);
    trigger_unpause(&h);

    h.escrow.release(&requester, &job_id);

    let job = h.escrow.get_job(&job_id);
    assert_eq!(job.status, escrow::JobStatus::Completed);
}
