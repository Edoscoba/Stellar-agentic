use crate::{CircuitBreaker, CircuitBreakerClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    vec, Address, Env,
};

const WINDOW: u32 = 100;

fn setup(
    env: &Env,
    num_nodes: usize,
) -> (
    CircuitBreakerClient<'static>,
    Address,
    soroban_sdk::Vec<Address>,
) {
    let admin = Address::generate(env);
    let mut nodes = soroban_sdk::Vec::new(env);
    for _ in 0..num_nodes {
        nodes.push_back(Address::generate(env));
    }

    let contract_id = env.register(CircuitBreaker, ());
    let client = CircuitBreakerClient::new(env, &contract_id);
    client.initialize(&admin, &nodes, &WINDOW);

    (client, admin, nodes)
}

#[test]
fn starts_unpaused() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, _nodes) = setup(&env, 5);

    assert!(!client.is_paused());
}

#[test]
fn quorum_reached_triggers_pause() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    for node in nodes.iter() {
        client.propose_pause(&node);
    }

    client.execute_pause();
    assert!(client.is_paused());
}

#[test]
#[should_panic(expected = "quorum not reached")]
fn quorum_not_reached_does_not_pause() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    // Only 4 of the 5 trusted nodes propose — one short of quorum.
    for node in nodes.iter().take(4) {
        client.propose_pause(&node);
    }

    client.execute_pause();
}

#[test]
fn duplicate_proposals_from_same_node_only_count_once() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    let first = nodes.get(0).unwrap();
    // Same node proposes repeatedly instead of 5 distinct nodes.
    for _ in 0..10 {
        client.propose_pause(&first);
    }
    for node in nodes.iter().skip(1).take(3) {
        client.propose_pause(&node);
    }

    // Only 4 distinct nodes total — quorum still not reached.
    assert_eq!(client.pause_quorum_count(), 4);
}

#[test]
#[should_panic(expected = "not a trusted node")]
fn untrusted_node_cannot_propose() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, _nodes) = setup(&env, 5);

    let stranger = Address::generate(&env);
    client.propose_pause(&stranger);
}

#[test]
fn expired_proposals_do_not_count_toward_quorum() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    // 4 nodes propose early...
    env.ledger().set_sequence_number(1_000);
    for node in nodes.iter().take(4) {
        client.propose_pause(&node);
    }

    // ...time passes well beyond the validity window...
    env.ledger().set_sequence_number(1_000 + WINDOW + 1);

    // ...and only the 5th node proposes now. The first 4 have expired, so
    // only 1 valid proposal remains — quorum is not reached.
    let fifth = nodes.get(4).unwrap();
    client.propose_pause(&fifth);

    assert_eq!(client.pause_quorum_count(), 1);
}

#[test]
#[should_panic(expected = "quorum not reached")]
fn execute_pause_panics_when_proposals_expired() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    env.ledger().set_sequence_number(1_000);
    for node in nodes.iter() {
        client.propose_pause(&node);
    }

    env.ledger().set_sequence_number(1_000 + WINDOW + 1);
    client.execute_pause();
}

#[test]
fn proposals_within_window_still_count() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    env.ledger().set_sequence_number(1_000);
    for node in nodes.iter() {
        client.propose_pause(&node);
    }

    // Still inside the window.
    env.ledger().set_sequence_number(1_000 + WINDOW);
    client.execute_pause();
    assert!(client.is_paused());
}

#[test]
fn unpause_requires_its_own_quorum() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    for node in nodes.iter() {
        client.propose_pause(&node);
    }
    client.execute_pause();
    assert!(client.is_paused());

    for node in nodes.iter().take(4) {
        client.propose_unpause(&node);
    }
    assert_eq!(client.unpause_quorum_count(), 4);

    let fifth = nodes.get(4).unwrap();
    client.propose_unpause(&fifth);
    client.unpause();

    assert!(!client.is_paused());
}

#[test]
#[should_panic(expected = "quorum not reached")]
fn unpause_panics_without_quorum() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    for node in nodes.iter() {
        client.propose_pause(&node);
    }
    client.execute_pause();

    for node in nodes.iter().take(3) {
        client.propose_unpause(&node);
    }
    client.unpause();
}

#[test]
fn admin_can_rotate_trusted_nodes() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin, _old_nodes) = setup(&env, 5);

    let new_nodes = vec![
        &env,
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    ];
    client.set_trusted_nodes(&admin, &new_nodes);

    assert_eq!(client.get_trusted_nodes(), new_nodes);
}

#[test]
fn rotating_trusted_nodes_clears_in_flight_proposals() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin, nodes) = setup(&env, 5);

    for node in nodes.iter().take(4) {
        client.propose_pause(&node);
    }
    assert_eq!(client.pause_quorum_count(), 4);

    // Rotate in a brand new set (even if it happens to overlap, proposals reset).
    let new_nodes = vec![
        &env,
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    ];
    client.set_trusted_nodes(&admin, &new_nodes);

    assert_eq!(client.pause_quorum_count(), 0);
}

#[test]
#[should_panic(expected = "not the admin")]
fn non_admin_cannot_rotate_trusted_nodes() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, nodes) = setup(&env, 5);

    let impostor = Address::generate(&env);
    client.set_trusted_nodes(&impostor, &nodes);
}

#[test]
#[should_panic(expected = "already initialized")]
fn cannot_reinitialize() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin, nodes) = setup(&env, 5);

    client.initialize(&admin, &nodes, &WINDOW);
}
