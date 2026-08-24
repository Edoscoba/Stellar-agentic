#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env,
};

fn setup() -> (
    Env,
    EscrowClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    // The dispute-timeout tests advance the ledger by DISPUTE_TIMEOUT_LEDGERS
    // (14400), far past the test host's default 4096-ledger entry lifetime.
    // Without a longer TTL the contract instance is archived part-way through
    // and every call fails with Storage(InternalError) rather than reaching
    // the contract logic under test.
    env.ledger().with_mut(|li| {
        li.min_temp_entry_ttl = DISPUTE_TIMEOUT_LEDGERS * 2;
        li.min_persistent_entry_ttl = DISPUTE_TIMEOUT_LEDGERS * 2;
        li.max_entry_ttl = DISPUTE_TIMEOUT_LEDGERS * 4;
    });

    let requester = Address::generate(&env);
    let worker = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let token_admin = Address::generate(&env);

    let token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    token::StellarAssetClient::new(&env, &token).mint(&requester, &10_000);

    let contract_id = env.register(Escrow, ());
    let client = EscrowClient::new(&env, &contract_id);

    (env, client, requester, worker, arbiter, token)
}

fn create_disputed_job(
    env: &Env,
    client: &EscrowClient<'static>,
    requester: &Address,
    worker: &Address,
    arbiter: &Address,
    token: &Address,
) -> u64 {
    let amount = 1000;
    let deadline = env.ledger().sequence() + 100;

    let job_id = client.create_job(
        requester,
        token,
        &amount,
        &Bytes::new(env),
        &deadline,
        &Some(arbiter.clone()),
    );

    client.accept_job(worker, &job_id);
    client.submit_result(worker, &job_id, &Bytes::new(env));
    client.dispute(requester, &job_id);

    job_id
}

#[test]
fn arbiter_rules_for_worker() {
    let (env, client, requester, worker, arbiter, token) = setup();
    let job_id = create_disputed_job(&env, &client, &requester, &worker, &arbiter, &token);

    client.resolve_dispute(&arbiter, &job_id, &true);

    let job = client.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&worker), 1000);
}

#[test]
fn arbiter_rules_for_requester() {
    let (env, client, requester, worker, arbiter, token) = setup();
    let token_client = token::Client::new(&env, &token);

    let job_id = create_disputed_job(&env, &client, &requester, &worker, &arbiter, &token);

    client.resolve_dispute(&arbiter, &job_id, &false);

    let job = client.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Refunded);

    assert_eq!(token_client.balance(&requester), 10000); // Refunded back
}

#[test]
#[should_panic(expected = "dispute deadline not reached yet")]
fn dispute_timeout_fallback_fails_early() {
    let (env, client, requester, worker, arbiter, token) = setup();
    let job_id = create_disputed_job(&env, &client, &requester, &worker, &arbiter, &token);

    // Advance ledger but not past timeout
    env.ledger()
        .with_mut(|li| li.sequence_number += DISPUTE_TIMEOUT_LEDGERS / 2);

    client.refund(&requester, &job_id);
}

#[test]
fn dispute_timeout_fallback_refunds() {
    let (env, client, requester, worker, arbiter, token) = setup();
    let job_id = create_disputed_job(&env, &client, &requester, &worker, &arbiter, &token);

    // Advance ledger past timeout
    env.ledger()
        .with_mut(|li| li.sequence_number += DISPUTE_TIMEOUT_LEDGERS + 1);

    client.refund(&requester, &job_id);

    let job = client.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Refunded);

    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&requester), 10000);
}

#[test]
#[should_panic(expected = "job not disputed")]
fn arbiter_resolves_non_disputed_panic() {
    let (env, client, requester, _worker, arbiter, token) = setup();
    let amount = 1000;
    let deadline = env.ledger().sequence() + 100;

    let job_id = client.create_job(
        &requester,
        &token,
        &amount,
        &Bytes::new(&env),
        &deadline,
        &Some(arbiter.clone()),
    );

    // Status is Open, not Disputed
    client.resolve_dispute(&arbiter, &job_id, &true);
}
