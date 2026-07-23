use crate::{PriceOracle, PriceOracleClient, PRICE_SCALE};
use soroban_sdk::{testutils::Address as _, Address, Env};

fn setup() -> (Env, PriceOracleClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let contract_id = env.register(PriceOracle, ());
    let client = PriceOracleClient::new(&env, &contract_id);
    client.initialize(&admin);
    (env, client, admin)
}

#[test]
fn identity_pair_is_always_priced() {
    let (env, client, _admin) = setup();
    let token = Address::generate(&env);
    assert_eq!(client.get_price(&token, &token), PRICE_SCALE);
    assert!(client.has_price(&token, &token));
}

#[test]
fn admin_can_publish_and_read_a_price() {
    let (env, client, admin) = setup();
    let usdc = Address::generate(&env);
    let xlm = Address::generate(&env);

    // 1 USDC ~= 10 XLM
    client.set_price(&admin, &usdc, &xlm, &(10 * PRICE_SCALE));

    assert_eq!(client.get_price(&usdc, &xlm), 10 * PRICE_SCALE);
    assert!(client.has_price(&usdc, &xlm));
    // No price published in the other direction.
    assert!(!client.has_price(&xlm, &usdc));
}

#[test]
#[should_panic(expected = "price not available")]
fn unpublished_pair_panics() {
    let (env, client, _admin) = setup();
    let usdc = Address::generate(&env);
    let xlm = Address::generate(&env);

    client.get_price(&usdc, &xlm);
}

#[test]
#[should_panic(expected = "not the admin")]
fn non_admin_cannot_set_price() {
    let (env, client, _admin) = setup();
    let impostor = Address::generate(&env);
    let usdc = Address::generate(&env);
    let xlm = Address::generate(&env);

    client.set_price(&impostor, &usdc, &xlm, &PRICE_SCALE);
}
