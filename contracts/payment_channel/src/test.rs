extern crate std;

use crate::{PaymentChannel, PaymentChannelClient, SpendPeriod, MAX_SLIPPAGE_BPS, PRICE_SCALE};
use amm_swap::{AmmSwap, AmmSwapClient, RATE_SCALE};
use price_oracle::{PriceOracle, PriceOracleClient};
use soroban_sdk::{testutils::Address as _, token, Address, Bytes, Env};

/// `settlement_token` stands in for the channel's funding asset (e.g.
/// USDC); `dest_token` stands in for a recipient's preferred asset (e.g.
/// XLM) that the channel was never funded in. The oracle and AMM both
/// quote 1 `settlement_token` == 5 `dest_token`, matching the example in
/// the design brief (agent funded in USDC, provider only accepts XLM).
struct Harness<'a> {
    env: Env,
    channel: PaymentChannelClient<'a>,
    oracle: PriceOracleClient<'a>,
    amm: AmmSwapClient<'a>,
    owner: Address,
    agent: Address,
    settlement_token: Address,
    dest_token: Address,
}

const RATE: i128 = 5;

fn setup() -> Harness<'static> {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let agent = Address::generate(&env);

    let settlement_admin = Address::generate(&env);
    let settlement_token = env
        .register_stellar_asset_contract_v2(settlement_admin)
        .address();
    token::StellarAssetClient::new(&env, &settlement_token).mint(&owner, &10_000_000);

    let dest_admin = Address::generate(&env);
    let dest_token = env.register_stellar_asset_contract_v2(dest_admin).address();

    let channel_id = env.register(PaymentChannel, ());
    let channel = PaymentChannelClient::new(&env, &channel_id);

    let oracle_admin = Address::generate(&env);
    let oracle_id = env.register(PriceOracle, ());
    let oracle = PriceOracleClient::new(&env, &oracle_id);
    oracle.initialize(&oracle_admin);
    oracle.set_price(
        &oracle_admin,
        &settlement_token,
        &dest_token,
        &(RATE * PRICE_SCALE),
    );

    let amm_admin = Address::generate(&env);
    let amm_id = env.register(AmmSwap, ());
    let amm = AmmSwapClient::new(&env, &amm_id);
    amm.initialize(&amm_admin);
    amm.set_rate(
        &amm_admin,
        &settlement_token,
        &dest_token,
        &(RATE * RATE_SCALE),
    );
    token::StellarAssetClient::new(&env, &dest_token).mint(&amm_admin, &1_000_000_000);
    amm.fund(&amm_admin, &dest_token, &1_000_000_000);

    channel.set_price_oracle(&oracle_admin, &oracle_id);
    channel.set_amm(&amm_admin, &amm_id);

    Harness {
        env,
        channel,
        oracle,
        amm,
        owner,
        agent,
        settlement_token,
        dest_token,
    }
}

fn open_channel(h: &Harness, deposit: i128, limit: i128) -> u64 {
    h.channel.open_channel(
        &h.owner,
        &h.agent,
        &h.settlement_token,
        &deposit,
        &limit,
        &SpendPeriod::Daily,
    )
}

fn memo(env: &Env) -> Bytes {
    Bytes::new(env)
}

// ── Baseline: existing `pay()` behavior is untouched ───────────────────────

#[test]
fn pay_transfers_settlement_token_and_tracks_spend() {
    let h = setup();
    let channel_id = open_channel(&h, 1_000_000, 500_000);
    let recipient = Address::generate(&h.env);

    h.channel
        .pay(&h.agent, &channel_id, &recipient, &1_000, &memo(&h.env));

    let settlement_client = token::Client::new(&h.env, &h.settlement_token);
    assert_eq!(settlement_client.balance(&recipient), 1_000);

    let info = h.channel.get_channel(&channel_id);
    assert_eq!(info.spent_this_period, 1_000);
    assert_eq!(info.total_spent, 1_000);
}

#[test]
#[should_panic(expected = "spend limit exceeded for this period")]
fn pay_still_enforces_the_spend_limit() {
    let h = setup();
    let channel_id = open_channel(&h, 1_000_000, 500_000);
    let recipient = Address::generate(&h.env);

    h.channel
        .pay(&h.agent, &channel_id, &recipient, &600_000, &memo(&h.env));
}

// ── Same-asset `pay_with_conversion` == `pay()`, no regression ─────────────

#[test]
fn pay_with_conversion_same_asset_behaves_like_pay() {
    // Deliberately does NOT call set_price_oracle / set_amm on this
    // channel, to prove the same-asset path never touches either — it is
    // purely additive on top of today's `pay()`.
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let recipient = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let settlement_token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    token::StellarAssetClient::new(&env, &settlement_token).mint(&owner, &1_000_000);

    let channel_id_contract = env.register(PaymentChannel, ());
    let channel = PaymentChannelClient::new(&env, &channel_id_contract);
    let channel_id = channel.open_channel(
        &owner,
        &agent,
        &settlement_token,
        &1_000_000,
        &500_000,
        &SpendPeriod::Daily,
    );

    let received = channel.pay_with_conversion(
        &agent,
        &channel_id,
        &recipient,
        &1_000,
        &settlement_token,
        &1_000,
        &memo(&env),
    );

    assert_eq!(received, 1_000);
    let settlement_client = token::Client::new(&env, &settlement_token);
    assert_eq!(settlement_client.balance(&recipient), 1_000);

    let info = channel.get_channel(&channel_id);
    assert_eq!(info.spent_this_period, 1_000);
    assert_eq!(info.total_spent, 1_000);
}

// ── Cross-asset conversion ──────────────────────────────────────────────────

#[test]
fn cross_asset_payment_within_slippage_updates_normalized_spend() {
    let h = setup();
    let channel_id = open_channel(&h, 1_000_000, 500_000);
    let recipient = Address::generate(&h.env);

    // Spend 1_000 settlement_token -> expect 5_000 dest_token at the 5x
    // rate; accept any amount >= 4_900 (well within MAX_SLIPPAGE_BPS).
    let received = h.channel.pay_with_conversion(
        &h.agent,
        &channel_id,
        &recipient,
        &1_000,
        &h.dest_token,
        &4_900,
        &memo(&h.env),
    );

    assert_eq!(received, 5_000);

    let dest_client = token::Client::new(&h.env, &h.dest_token);
    assert_eq!(dest_client.balance(&recipient), 5_000);

    // The spend limit is charged in settlement_token units (1_000), not
    // dest_token units (5_000) — this is what "normalized" means here.
    let info = h.channel.get_channel(&channel_id);
    assert_eq!(info.spent_this_period, 1_000);
    assert_eq!(info.total_spent, 1_000);
}

#[test]
#[should_panic(expected = "slippage tolerance exceeds maximum allowed deviation")]
fn cross_asset_payment_exceeding_slippage_tolerance_reverts() {
    let h = setup();
    let channel_id = open_channel(&h, 1_000_000, 500_000);
    let recipient = Address::generate(&h.env);

    // Fair value is 5_000 dest_token; MAX_SLIPPAGE_BPS allows down to
    // 5_000 * (10_000 - 500) / 10_000 = 4_750. Ask for far less than that.
    let min_received = 3_000;
    assert!(min_received < 5_000 * (10_000 - MAX_SLIPPAGE_BPS) / 10_000);

    h.channel.pay_with_conversion(
        &h.agent,
        &channel_id,
        &recipient,
        &1_000,
        &h.dest_token,
        &min_received,
        &memo(&h.env),
    );
}

#[test]
fn price_feed_unavailable_fails_safely() {
    let h = setup();
    let channel_id = open_channel(&h, 1_000_000, 500_000);
    let recipient = Address::generate(&h.env);

    // A token the oracle has never been told a price for.
    let unpriced_token = Address::generate(&h.env);

    let before = h.channel.get_channel(&channel_id);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.channel.pay_with_conversion(
            &h.agent,
            &channel_id,
            &recipient,
            &1_000,
            &unpriced_token,
            &0,
            &memo(&h.env),
        );
    }));

    assert!(
        result.is_err(),
        "payment with no price feed must fail, not silently proceed unpriced"
    );

    // Fails safe: nothing transferred, spend counters untouched.
    let after = h.channel.get_channel(&channel_id);
    assert_eq!(after.spent_this_period, before.spent_this_period);
    assert_eq!(after.total_spent, before.total_spent);

    let settlement_client = token::Client::new(&h.env, &h.settlement_token);
    assert_eq!(settlement_client.balance(&recipient), 0);
}

#[test]
#[should_panic(expected = "swap output below min_out")]
fn cross_asset_payment_reverts_if_amm_cannot_clear_min_received() {
    let h = setup();
    let channel_id = open_channel(&h, 1_000_000, 500_000);
    let recipient = Address::generate(&h.env);

    // min_received of 5_000 clears the oracle-fairness floor (fair value
    // is exactly 5_000 at the configured 5x rate) but asking for one more
    // than the AMM can actually produce must revert via the AMM's own
    // min_out check.
    h.channel.pay_with_conversion(
        &h.agent,
        &channel_id,
        &recipient,
        &1_000,
        &h.dest_token,
        &5_001,
        &memo(&h.env),
    );
}

// ── Acceptance criteria: normalized limit across a same/cross-asset mix ────

#[test]
fn spend_limit_enforced_in_normalized_terms_across_same_and_cross_asset_payments() {
    let h = setup();
    let channel_id = open_channel(&h, 1_000_000, 10_000);
    let recipient = Address::generate(&h.env);

    // 1) Plain same-asset pay(): 3_000 settlement_token.
    h.channel
        .pay(&h.agent, &channel_id, &recipient, &3_000, &memo(&h.env));

    // 2) pay_with_conversion, same asset: 2_000 settlement_token.
    h.channel.pay_with_conversion(
        &h.agent,
        &channel_id,
        &recipient,
        &2_000,
        &h.settlement_token,
        &2_000,
        &memo(&h.env),
    );

    // 3) pay_with_conversion, cross asset: 4_000 settlement_token ->
    //    20_000 dest_token.
    let received = h.channel.pay_with_conversion(
        &h.agent,
        &channel_id,
        &recipient,
        &4_000,
        &h.dest_token,
        &19_000,
        &memo(&h.env),
    );
    assert_eq!(received, 20_000);

    // Total charged against the limit: 3_000 + 2_000 + 4_000 = 9_000,
    // regardless of which asset each payment actually settled in.
    let info = h.channel.get_channel(&channel_id);
    assert_eq!(info.spent_this_period, 9_000);
    assert_eq!(info.total_spent, 9_000);
    assert_eq!(h.channel.remaining_this_period(&channel_id), 1_000);

    // One more settlement-token unit than the remaining 1_000 must still
    // be rejected, whether same-asset or cross-asset.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.channel
            .pay(&h.agent, &channel_id, &recipient, &1_001, &memo(&h.env));
    }));
    assert!(result.is_err());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.channel.pay_with_conversion(
            &h.agent,
            &channel_id,
            &recipient,
            &1_001,
            &h.dest_token,
            &0,
            &memo(&h.env),
        );
    }));
    assert!(result.is_err());

    // But exactly the remaining 1_000 still goes through, from either path.
    h.channel
        .pay(&h.agent, &channel_id, &recipient, &1_000, &memo(&h.env));
    let info = h.channel.get_channel(&channel_id);
    assert_eq!(info.spent_this_period, 10_000);
    assert_eq!(h.channel.remaining_this_period(&channel_id), 0);
}

// ── Admin gating on the new wiring endpoints ────────────────────────────────

#[test]
#[should_panic(expected = "not the price oracle admin")]
fn set_price_oracle_is_admin_gated() {
    let h = setup();
    let impostor = Address::generate(&h.env);
    h.channel.set_price_oracle(&impostor, &h.oracle.address);
}

#[test]
#[should_panic(expected = "not the amm admin")]
fn set_amm_is_admin_gated() {
    let h = setup();
    let impostor = Address::generate(&h.env);
    h.channel.set_amm(&impostor, &h.amm.address);
}
