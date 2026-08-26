//! Voucher settlement tests.
//!
//! Grouped by the property each set exists to establish, because for a payment
//! protocol "does it work" is the least interesting question. The ones that
//! matter are: can a payer pay less than they owe, can a voucher be replayed
//! somewhere it should not be, and can the contract ever pay out more than it
//! holds.

use ed25519_dalek::{Signer as _, SigningKey};
use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::{token, Address, Bytes, BytesN, Env};

use crate::{
    Allocation, PaymentChannel, PaymentChannelClient, SpendPeriod, DEFAULT_DISPUTE_LEDGERS,
    MIN_DISPUTE_LEDGERS,
};

const DEPOSIT: i128 = 1_000_000;
const LIMIT: i128 = 1_000_000;

/// Everything a voucher test needs, wired together.
struct Fixture {
    env: Env,
    client: PaymentChannelClient<'static>,
    contract_id: Address,
    token: token::Client<'static>,
    owner: Address,
    agent: Address,
    recipient: Address,
    channel_id: u64,
    signing_key: SigningKey,
}

impl Fixture {
    fn new() -> Self {
        Self::with_dispute_window(DEFAULT_DISPUTE_LEDGERS)
    }

    fn with_dispute_window(dispute_ledgers: u32) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        // The dispute window is ~24 hours of ledgers, and the test host
        // archives entries whose TTL lapses while the sequence jumps forward.
        // That is a test-environment artifact, not the behaviour under test, so
        // the entry lifetime is raised past the longest window a test uses.
        env.ledger().set_max_entry_ttl(DEFAULT_DISPUTE_LEDGERS * 8);

        let owner = Address::generate(&env);
        let agent = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token_admin = Address::generate(&env);

        let token_address = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();
        token::StellarAssetClient::new(&env, &token_address).mint(&owner, &(DEPOSIT * 10));

        let contract_id = env.register(PaymentChannel, ());
        let client = PaymentChannelClient::new(&env, &contract_id);

        let channel_id = client.open_channel(
            &owner,
            &agent,
            &token_address,
            &DEPOSIT,
            &LIMIT,
            &SpendPeriod::Daily,
        );

        // A deterministic voucher key, so a failure names the same bytes on
        // every machine.
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let public = BytesN::from_array(&env, &signing_key.verifying_key().to_bytes());
        client.enable_vouchers(&owner, &channel_id, &public, &dispute_ledgers);

        Self {
            token: token::Client::new(&env, &token_address),
            env,
            client,
            contract_id,
            owner,
            agent,
            recipient,
            channel_id,
            signing_key,
        }
    }

    fn allocate(&self, amount: i128) {
        self.client
            .allocate(&self.owner, &self.channel_id, &self.recipient, &amount);
    }

    /// Sign a voucher exactly the way an SDK would.
    fn voucher(&self, sequence: u64, cumulative: i128) -> BytesN<64> {
        self.voucher_for(&self.recipient, self.channel_id, sequence, cumulative)
    }

    fn voucher_for(
        &self,
        recipient: &Address,
        channel_id: u64,
        sequence: u64,
        cumulative: i128,
    ) -> BytesN<64> {
        let preimage = self.env.as_contract(&self.contract_id, || {
            crate::voucher::preimage(&self.env, channel_id, recipient, sequence, cumulative)
        });
        self.sign(&preimage)
    }

    fn sign(&self, message: &Bytes) -> BytesN<64> {
        let mut buffer = [0u8; crate::voucher::PREIMAGE_LEN as usize];
        message.copy_into_slice(&mut buffer);
        BytesN::from_array(&self.env, &self.signing_key.sign(&buffer).to_bytes())
    }

    fn advance_ledgers(&self, count: u32) {
        // Keep this channel's entries alive across the jump, for the same
        // reason as `set_max_entry_ttl` above.
        self.env.as_contract(&self.contract_id, || {
            let ttl = count.saturating_add(DEFAULT_DISPUTE_LEDGERS);
            self.env.storage().instance().extend_ttl(ttl, ttl);
        });
        let current = self.env.ledger().sequence();
        self.env.ledger().set_sequence_number(current + count);
    }

    fn allocation(&self) -> Allocation {
        self.client
            .get_allocation(&self.channel_id, &self.recipient)
    }
}

// ─── The happy path ──────────────────────────────────────────────────────────

#[test]
fn a_cooperative_close_pays_immediately() {
    let f = Fixture::new();
    f.allocate(100_000);

    let paid = f.client.close_cooperative(
        &f.channel_id,
        &f.recipient,
        &42,
        &75_000,
        &f.voucher(42, 75_000),
    );

    assert_eq!(paid, 75_000);
    assert_eq!(f.token.balance(&f.recipient), 75_000);
    // The unclaimed 25,000 goes back to free collateral rather than staying
    // locked.
    let channel = f.client.get_channel(&f.channel_id);
    assert_eq!(channel.allocated, 0);
    assert_eq!(channel.collateral, DEPOSIT - 75_000);
}

#[test]
fn ten_thousand_off_chain_payments_settle_in_one_transaction() {
    // The definition of done, and the whole point of the scheme.
    let f = Fixture::new();
    f.allocate(1_000_000);

    // Ten thousand payments of 100 stroops. Only the last voucher is ever sent
    // on-chain; the other 9,999 exist only as signatures.
    let mut last = BytesN::from_array(&f.env, &[0u8; 64]);
    let mut cumulative = 0i128;
    for sequence in 1..=10_000u64 {
        cumulative += 100;
        last = f.voucher(sequence, cumulative);
    }
    assert_eq!(cumulative, 1_000_000);

    let paid = f
        .client
        .close_cooperative(&f.channel_id, &f.recipient, &10_000, &cumulative, &last);

    assert_eq!(paid, 1_000_000);
    assert_eq!(f.token.balance(&f.recipient), 1_000_000);
}

#[test]
fn a_unilateral_close_pays_after_the_window() {
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.recipient,
        &f.channel_id,
        &f.recipient,
        &5,
        &50_000,
        &f.voucher(5, 50_000),
    );
    assert_eq!(f.token.balance(&f.recipient), 0, "nothing pays out yet");

    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS);
    let paid = f.client.finalize(&f.channel_id, &f.recipient);

    assert_eq!(paid, 50_000);
    assert_eq!(f.token.balance(&f.recipient), 50_000);
}

// ─── A payer must not be able to pay less than they owe ─────────────────────

#[test]
fn a_later_voucher_supersedes_a_stale_unilateral_close() {
    let f = Fixture::new();
    f.allocate(100_000);

    // The payer closes with an old voucher worth 10,000...
    f.client.close_unilateral(
        &f.owner,
        &f.channel_id,
        &f.recipient,
        &1,
        &10_000,
        &f.voucher(1, 10_000),
    );

    // ...and the recipient produces the real one, worth 80,000.
    f.client.challenge(
        &f.channel_id,
        &f.recipient,
        &9,
        &80_000,
        &f.voucher(9, 80_000),
    );

    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS);
    let paid = f.client.finalize(&f.channel_id, &f.recipient);

    // 80,000 owed, plus a penalty equal to the 70,000 that was withheld —
    // capped at the 20,000 the owner would otherwise have reclaimed.
    assert_eq!(paid, 100_000);
    assert_eq!(f.token.balance(&f.recipient), 100_000);
}

#[test]
fn cheating_costs_the_payer_more_than_honesty() {
    // The property the penalty exists for: a payer who tries a stale close and
    // is caught must end up worse off than one who simply settled.
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.owner,
        &f.channel_id,
        &f.recipient,
        &1,
        &10_000,
        &f.voucher(1, 10_000),
    );
    f.client.challenge(
        &f.channel_id,
        &f.recipient,
        &2,
        &40_000,
        &f.voucher(2, 40_000),
    );
    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS);
    let paid = f.client.finalize(&f.channel_id, &f.recipient);

    let honest_cost = 40_000;
    assert!(
        paid > honest_cost,
        "cheating cost {paid}, honesty would have cost {honest_cost}"
    );
    assert_eq!(paid, 70_000, "40,000 owed + 30,000 withheld as penalty");
}

#[test]
fn a_recipient_closing_unilaterally_is_not_penalised() {
    // Only a payer benefits from a stale voucher. Penalising a recipient for
    // closing would deter the one action they need when a payer goes dark.
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.recipient,
        &f.channel_id,
        &f.recipient,
        &1,
        &10_000,
        &f.voucher(1, 10_000),
    );
    f.client.challenge(
        &f.channel_id,
        &f.recipient,
        &2,
        &40_000,
        &f.voucher(2, 40_000),
    );
    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS);

    assert_eq!(f.client.finalize(&f.channel_id, &f.recipient), 40_000);
}

#[test]
#[should_panic(expected = "does not supersede")]
fn a_smaller_voucher_cannot_supersede() {
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.recipient,
        &f.channel_id,
        &f.recipient,
        &9,
        &80_000,
        &f.voucher(9, 80_000),
    );
    // A perfectly valid signature over a smaller amount.
    f.client.challenge(
        &f.channel_id,
        &f.recipient,
        &10,
        &10_000,
        &f.voucher(10, 10_000),
    );
}

#[test]
#[should_panic(expected = "does not supersede")]
fn an_enormous_sequence_with_a_tiny_amount_cannot_erase_the_claim() {
    // The attack that sequence-ordered settlement would allow. The payer holds
    // the signing key, so they can mint this voucher freely — it must simply
    // not do anything.
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.recipient,
        &f.channel_id,
        &f.recipient,
        &9,
        &80_000,
        &f.voucher(9, 80_000),
    );
    f.client.challenge(
        &f.channel_id,
        &f.recipient,
        &u64::MAX,
        &1,
        &f.voucher(u64::MAX, 1),
    );
}

#[test]
#[should_panic(expected = "challenge window has closed")]
fn a_challenge_after_the_window_is_refused() {
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.owner,
        &f.channel_id,
        &f.recipient,
        &1,
        &10_000,
        &f.voucher(1, 10_000),
    );
    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS);
    f.client.challenge(
        &f.channel_id,
        &f.recipient,
        &2,
        &80_000,
        &f.voucher(2, 80_000),
    );
}

#[test]
#[should_panic(expected = "window is still open")]
fn finalizing_early_is_refused() {
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.owner,
        &f.channel_id,
        &f.recipient,
        &1,
        &10_000,
        &f.voucher(1, 10_000),
    );
    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS - 1);
    f.client.finalize(&f.channel_id, &f.recipient);
}

#[test]
fn the_window_does_not_reset_on_challenge() {
    // Otherwise a payer could hold a channel open forever by drip-feeding
    // vouchers one stroop apart.
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.owner,
        &f.channel_id,
        &f.recipient,
        &1,
        &10_000,
        &f.voucher(1, 10_000),
    );
    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS / 2);
    f.client.challenge(
        &f.channel_id,
        &f.recipient,
        &2,
        &20_000,
        &f.voucher(2, 20_000),
    );

    // Half the window remains, measured from the *open*, not the challenge.
    f.advance_ledgers(DEFAULT_DISPUTE_LEDGERS / 2);
    assert!(f.client.finalize(&f.channel_id, &f.recipient) >= 20_000);
}

// ─── Replay ──────────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Crypto, InvalidInput)")]
fn a_voucher_for_another_channel_does_not_verify() {
    let f = Fixture::new();
    f.allocate(100_000);

    // Same key, same recipient, same amount — different channel.
    let foreign = f.voucher_for(&f.recipient, f.channel_id + 1, 1, 50_000);
    f.client
        .close_cooperative(&f.channel_id, &f.recipient, &1, &50_000, &foreign);
}

#[test]
#[should_panic(expected = "Error(Crypto, InvalidInput)")]
fn a_voucher_for_another_recipient_does_not_verify() {
    let f = Fixture::new();
    f.allocate(100_000);

    let someone_else = Address::generate(&f.env);
    let foreign = f.voucher_for(&someone_else, f.channel_id, 1, 50_000);
    f.client
        .close_cooperative(&f.channel_id, &f.recipient, &1, &50_000, &foreign);
}

#[test]
#[should_panic(expected = "Error(Crypto, InvalidInput)")]
fn changing_the_amount_invalidates_the_signature() {
    let f = Fixture::new();
    f.allocate(100_000);

    let signed_for_less = f.voucher(1, 10_000);
    f.client
        .close_cooperative(&f.channel_id, &f.recipient, &1, &90_000, &signed_for_less);
}

#[test]
#[should_panic(expected = "Error(Crypto, InvalidInput)")]
fn a_voucher_signed_by_the_wrong_key_does_not_verify() {
    let mut f = Fixture::new();
    f.signing_key = SigningKey::from_bytes(&[9u8; 32]);
    f.allocate(100_000);

    f.client.close_cooperative(
        &f.channel_id,
        &f.recipient,
        &1,
        &50_000,
        &f.voucher(1, 50_000),
    );
}

#[test]
#[should_panic(expected = "Error(Crypto, InvalidInput)")]
fn rotating_the_signer_repudiates_unsettled_vouchers() {
    // The intended blast radius of a key rotation: everything the old key
    // signed and has not yet settled stops verifying.
    let f = Fixture::new();
    f.allocate(100_000);
    let old = f.voucher(1, 50_000);

    let rotated = SigningKey::from_bytes(&[3u8; 32]);
    let public = BytesN::from_array(&f.env, &rotated.verifying_key().to_bytes());
    f.client
        .enable_vouchers(&f.owner, &f.channel_id, &public, &DEFAULT_DISPUTE_LEDGERS);

    f.client
        .close_cooperative(&f.channel_id, &f.recipient, &1, &50_000, &old);
}

#[test]
#[should_panic(expected = "already been settled")]
fn a_settled_voucher_cannot_be_replayed() {
    let f = Fixture::new();
    f.allocate(100_000);
    f.client.close_cooperative(
        &f.channel_id,
        &f.recipient,
        &1,
        &50_000,
        &f.voucher(1, 50_000),
    );

    // The allocation is released, so this now fails on the allocation bound
    // before it ever reaches the signature.
    f.allocate(100_000);
    f.client.close_cooperative(
        &f.channel_id,
        &f.recipient,
        &1,
        &50_000,
        &f.voucher(1, 50_000),
    );
}

// ─── The payout bound ────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "exceeds the allocation")]
fn a_voucher_larger_than_the_allocation_is_refused() {
    let f = Fixture::new();
    f.allocate(50_000);
    f.client.close_cooperative(
        &f.channel_id,
        &f.recipient,
        &1,
        &50_001,
        &f.voucher(1, 50_001),
    );
}

#[test]
#[should_panic(expected = "insufficient free collateral")]
fn allocations_cannot_exceed_the_deposit() {
    let f = Fixture::new();
    f.allocate(DEPOSIT);
    // A second recipient cannot be promised money that is already promised.
    f.client
        .allocate(&f.owner, &f.channel_id, &Address::generate(&f.env), &1);
}

#[test]
fn the_payout_never_exceeds_the_deposit_across_many_recipients() {
    // The property test the definition of done asks for, expressed over the
    // thing that actually matters: whatever sequence of allocations, vouchers
    // and closes happens, the contract cannot pay out more than it was given.
    let f = Fixture::new();

    let recipients: soroban_sdk::Vec<Address> = soroban_sdk::vec![
        &f.env,
        Address::generate(&f.env),
        Address::generate(&f.env),
        Address::generate(&f.env),
        Address::generate(&f.env),
    ];

    // Allocate the entire deposit across four recipients, then have each claim
    // its full allocation — the worst case for the bound.
    let each = DEPOSIT / 4;
    for recipient in recipients.iter() {
        f.client
            .allocate(&f.owner, &f.channel_id, &recipient, &each);
    }

    let mut total_paid = 0i128;
    for (index, recipient) in recipients.iter().enumerate() {
        let signature = f.voucher_for(&recipient, f.channel_id, index as u64 + 1, each);
        total_paid += f.client.close_cooperative(
            &f.channel_id,
            &recipient,
            &(index as u64 + 1),
            &each,
            &signature,
        );
    }

    assert_eq!(total_paid, DEPOSIT);
    assert!(
        total_paid <= DEPOSIT,
        "paid out {total_paid} against a {DEPOSIT} deposit"
    );
    assert_eq!(f.token.balance(&f.contract_id), 0);
    assert_eq!(f.client.get_channel(&f.channel_id).collateral, 0);
}

#[test]
fn vouchers_and_on_chain_payments_draw_on_the_same_collateral() {
    // Without this, an agent could spend the deposit on-chain and the
    // recipient's voucher would bounce at settlement.
    let f = Fixture::new();
    f.allocate(600_000);

    // 400,000 is free; the agent may spend that and no more.
    f.client.pay(
        &f.agent,
        &f.channel_id,
        &Address::generate(&f.env),
        &400_000,
        &Bytes::new(&f.env),
    );

    let channel = f.client.get_channel(&f.channel_id);
    assert_eq!(channel.collateral, 600_000);
    assert_eq!(channel.allocated, 600_000);

    // The voucher is still fully backed.
    assert_eq!(
        f.client.close_cooperative(
            &f.channel_id,
            &f.recipient,
            &1,
            &600_000,
            &f.voucher(1, 600_000)
        ),
        600_000
    );
    assert_eq!(f.token.balance(&f.recipient), 600_000);
}

#[test]
#[should_panic(expected = "insufficient channel collateral")]
fn the_on_chain_path_cannot_spend_allocated_collateral() {
    let f = Fixture::new();
    f.allocate(600_000);

    f.client.pay(
        &f.agent,
        &f.channel_id,
        &Address::generate(&f.env),
        &400_001,
        &Bytes::new(&f.env),
    );
}

// ─── Configuration ───────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "below the minimum")]
fn a_dispute_window_below_the_floor_is_refused() {
    // Without a floor, an owner could close and finalise before any recipient
    // could physically respond, making the dispute mechanism decorative.
    Fixture::with_dispute_window(MIN_DISPUTE_LEDGERS - 1);
}

#[test]
#[should_panic(expected = "vouchers are not enabled")]
fn a_channel_without_a_voucher_signer_cannot_allocate() {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    token::StellarAssetClient::new(&env, &token_address).mint(&owner, &DEPOSIT);

    let client = PaymentChannelClient::new(&env, &env.register(PaymentChannel, ()));
    let channel_id = client.open_channel(
        &owner,
        &Address::generate(&env),
        &token_address,
        &DEPOSIT,
        &LIMIT,
        &SpendPeriod::Daily,
    );

    client.allocate(&owner, &channel_id, &Address::generate(&env), &1);
}

#[test]
#[should_panic(expected = "already in flight")]
fn a_cooperative_close_cannot_sidestep_an_open_dispute() {
    // Otherwise a payer could open a window with a stale voucher and finalise
    // at the stale amount the moment the recipient co-signed anything.
    let f = Fixture::new();
    f.allocate(100_000);

    f.client.close_unilateral(
        &f.owner,
        &f.channel_id,
        &f.recipient,
        &1,
        &10_000,
        &f.voucher(1, 10_000),
    );
    f.client.close_cooperative(
        &f.channel_id,
        &f.recipient,
        &2,
        &20_000,
        &f.voucher(2, 20_000),
    );
}

// ─── The collateral bug fix ──────────────────────────────────────────────────

#[test]
fn closing_a_channel_returns_the_deposit() {
    // Before this work `close_channel` set a flag and transferred nothing, so
    // every deposit was stranded permanently. No test covered it.
    let f = Fixture::new();
    let before = f.token.balance(&f.owner);

    f.client.close_channel(&f.owner, &f.channel_id);

    assert_eq!(f.token.balance(&f.owner), before + DEPOSIT);
    assert_eq!(f.client.get_channel(&f.channel_id).collateral, 0);
}

#[test]
fn closing_a_channel_leaves_allocated_collateral_settleable() {
    // An owner must not be able to close their way out of an obligation.
    let f = Fixture::new();
    f.allocate(100_000);
    let before = f.token.balance(&f.owner);

    f.client.close_channel(&f.owner, &f.channel_id);
    assert_eq!(
        f.token.balance(&f.owner),
        before + DEPOSIT - 100_000,
        "only free collateral comes back"
    );

    // The voucher still settles.
    assert_eq!(
        f.client.close_cooperative(
            &f.channel_id,
            &f.recipient,
            &1,
            &60_000,
            &f.voucher(1, 60_000)
        ),
        60_000
    );
    // ...and the remainder is swept afterwards.
    assert_eq!(f.client.withdraw_free(&f.owner, &f.channel_id), 40_000);
    assert_eq!(f.token.balance(&f.contract_id), 0);
}

#[test]
fn a_top_up_increases_what_the_channel_can_spend() {
    // Previously the transfer happened and nothing recorded it.
    let f = Fixture::new();
    f.client.top_up(&f.owner, &f.channel_id, &500_000);

    assert_eq!(
        f.client.get_channel(&f.channel_id).collateral,
        DEPOSIT + 500_000
    );
    f.allocate(DEPOSIT + 500_000);
    assert_eq!(f.allocation().amount, DEPOSIT + 500_000);
}

#[test]
#[should_panic(expected = "insufficient channel collateral")]
fn a_channel_cannot_outspend_its_deposit_across_periods() {
    // The unbounded-spend bug: `spent_this_period` resets each period, so
    // without a collateral check an agent could spend `limit_per_period` per
    // period forever, out of other owners' deposits.
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let agent = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    token::StellarAssetClient::new(&env, &token_address).mint(&owner, &DEPOSIT);

    let client = PaymentChannelClient::new(&env, &env.register(PaymentChannel, ()));
    let channel_id = client.open_channel(
        &owner,
        &agent,
        &token_address,
        &DEPOSIT,
        &DEPOSIT,
        &SpendPeriod::PerLedger,
    );

    let recipient = Address::generate(&env);
    // Spend the whole deposit in one period...
    client.pay(&agent, &channel_id, &recipient, &DEPOSIT, &Bytes::new(&env));

    // ...then let the period roll over and try again. The rate limit allows
    // it; the collateral must not.
    let current = env.ledger().sequence();
    env.ledger().set_sequence_number(current + 10);
    client.pay(&agent, &channel_id, &recipient, &1, &Bytes::new(&env));
}
