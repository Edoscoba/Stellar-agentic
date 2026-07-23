use crate::{AmmSwap, AmmSwapClient, RATE_SCALE};
use soroban_sdk::{testutils::Address as _, token, Address, Env};

struct Harness<'a> {
    env: Env,
    amm: AmmSwapClient<'a>,
    admin: Address,
    from_token: Address,
    to_token: Address,
}

fn setup() -> Harness<'static> {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(AmmSwap, ());
    let amm = AmmSwapClient::new(&env, &contract_id);
    amm.initialize(&admin);

    let from_admin = Address::generate(&env);
    let from_token = env
        .register_stellar_asset_contract_v2(from_admin.clone())
        .address();

    let to_admin = Address::generate(&env);
    let to_token = env
        .register_stellar_asset_contract_v2(to_admin.clone())
        .address();

    // Fund the AMM's reserve of `to_token` so it can pay out swaps.
    let to_asset_client = token::StellarAssetClient::new(&env, &to_token);
    to_asset_client.mint(&admin, &1_000_000_000);
    amm.fund(&admin, &to_token, &1_000_000_000);

    Harness {
        env,
        amm,
        admin,
        from_token,
        to_token,
    }
}

#[test]
fn swap_pays_out_at_configured_rate() {
    let h = setup();
    // 1 from_token == 5 to_token
    h.amm
        .set_rate(&h.admin, &h.from_token, &h.to_token, &(5 * RATE_SCALE));

    let from_asset_client = token::StellarAssetClient::new(&h.env, &h.from_token);
    from_asset_client.mint(&h.amm.address, &1_000);

    let recipient = Address::generate(&h.env);
    let out = h
        .amm
        .execute_swap(&h.from_token, &1_000, &h.to_token, &4_000, &recipient);

    assert_eq!(out, 5_000);
    let to_client = token::Client::new(&h.env, &h.to_token);
    assert_eq!(to_client.balance(&recipient), 5_000);
}

#[test]
#[should_panic(expected = "swap output below min_out")]
fn swap_reverts_when_below_min_out() {
    let h = setup();
    h.amm
        .set_rate(&h.admin, &h.from_token, &h.to_token, &(5 * RATE_SCALE));

    let from_asset_client = token::StellarAssetClient::new(&h.env, &h.from_token);
    from_asset_client.mint(&h.amm.address, &1_000);

    let recipient = Address::generate(&h.env);
    h.amm
        .execute_swap(&h.from_token, &1_000, &h.to_token, &6_000, &recipient);
}

#[test]
#[should_panic(expected = "no rate configured")]
fn swap_reverts_without_a_configured_rate() {
    let h = setup();
    let recipient = Address::generate(&h.env);
    h.amm
        .execute_swap(&h.from_token, &1_000, &h.to_token, &0, &recipient);
}
