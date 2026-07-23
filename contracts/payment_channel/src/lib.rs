#![no_std]
// `pay_with_conversion` needs one more parameter than `pay()` to carry the
// cross-asset conversion inputs (dest_token, min_received); the
// `#[contractimpl]` macro's generated Client/WASM-export wrappers don't
// inherit a function- or impl-level `#[allow]`, so this is set crate-wide.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contractimpl, contracttype,
    crypto::bls12_381::{Fr, G1Affine, G2Affine},
    symbol_short, token, Address, Env, IntoVal, Map, Symbol, Vec, U256,
};

// ─── Cross-asset conversion constants ─────────────────────────────────────────

/// Fixed-point scale used when reading prices from the configured
/// `PriceOracle`. Must match `price_oracle::PRICE_SCALE`; the two crates
/// don't share a dependency so this is duplicated deliberately (a
/// cross-contract call only ever exchanges an `i128`, not a shared type).
pub const PRICE_SCALE: i128 = 10_000_000;

/// Maximum allowed deviation, in basis points, between the caller-supplied
/// `min_received` (in `pay_with_conversion`) and the `PriceOracle`'s
/// fair-value quote for the same trade. This is a hard, contract-enforced
/// floor layered *on top of* whatever slippage tolerance the caller asks
/// for: even if a caller (or a buggy/compromised client) requests a
/// `min_received` far below fair value, the oracle keeps the trade honest.
/// 5% is a conservative default for agent-initiated payments; tune per
/// deployment.
pub const MAX_SLIPPAGE_BPS: i128 = 500;
const BPS_DENOMINATOR: i128 = 10_000;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Period over which a spend limit resets
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum SpendPeriod {
    /// Per-ledger limit (useful for testing)
    PerLedger,
    /// Hourly limit (~720 ledgers at 5s each)
    Hourly,
    /// Daily limit (~17280 ledgers)
    Daily,
}

/// A configured payment channel authorizing an agent to spend
#[contracttype]
#[derive(Clone, Debug)]
pub struct Channel {
    /// Agent address authorized to spend
    pub agent: Address,
    /// Owner who funded this channel
    pub owner: Address,
    /// Token contract address (e.g. USDC)
    pub token: Address,
    /// Max spend per period (in stroops / token base units)
    pub limit_per_period: i128,
    /// Period type
    pub period: SpendPeriod,
    /// Amount spent in the current period
    pub spent_this_period: i128,
    /// Ledger number when current period started
    pub period_start_ledger: u32,
    /// Total lifetime spend through this channel
    pub total_spent: i128,
    /// Whether this channel is open
    pub active: bool,
}

/// A single payment record
#[contracttype]
#[derive(Clone, Debug)]
pub struct PaymentRecord {
    pub agent: Address,
    pub recipient: Address,
    pub amount: i128,
    pub token: Address,
    pub ledger: u32,
    pub memo: soroban_sdk::Bytes,
}

/// A Groth16 verifying key for the solvency circuit (see
/// `zk/solvency_proof`), encoded as native BLS12-381 points so it can be
/// checked on-chain via `env.crypto().bls12_381().pairing_check`.
///
/// `gamma_abc_g1` must have exactly 3 entries: the constant term followed
/// by one entry per public input (`limit_per_period`, `total_spent`, in
/// that order), per the circuit's declared public inputs.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SolvencyVerifyingKey {
    pub alpha_g1: G1Affine,
    pub beta_g2: G2Affine,
    pub gamma_g2: G2Affine,
    pub delta_g2: G2Affine,
    pub gamma_abc_g1: Vec<G1Affine>,
}

/// A Groth16 proof for the solvency circuit.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SolvencyProof {
    pub a: G1Affine,
    pub b: G2Affine,
    pub c: G1Affine,
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct PaymentChannel;

#[contractimpl]
impl PaymentChannel {
    /// Open a payment channel for an agent.
    /// Owner deposits tokens and sets a per-period spend limit.
    ///
    /// # Arguments
    /// * `owner` - Who is funding the channel
    /// * `agent` - The AI agent authorized to spend
    /// * `token` - Token contract address (USDC, XLM, etc.)
    /// * `deposit` - Initial deposit amount
    /// * `limit_per_period` - Max the agent can spend per period
    /// * `period` - Reset period (Hourly, Daily, etc.)
    pub fn open_channel(
        env: Env,
        owner: Address,
        agent: Address,
        token: Address,
        deposit: i128,
        limit_per_period: i128,
        period: SpendPeriod,
    ) -> u64 {
        owner.require_auth();

        if deposit <= 0 {
            panic!("deposit must be positive");
        }
        if limit_per_period <= 0 {
            panic!("limit must be positive");
        }
        if limit_per_period > deposit {
            panic!("limit cannot exceed deposit");
        }

        // Transfer deposit from owner to this contract
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&owner, &env.current_contract_address(), &deposit);

        // Build channel
        let channel = Channel {
            agent: agent.clone(),
            owner: owner.clone(),
            token,
            limit_per_period,
            period,
            spent_this_period: 0,
            period_start_ledger: env.ledger().sequence(),
            total_spent: 0,
            active: true,
        };

        // Store channel, keyed by incrementing ID
        let channel_id = Self::next_id(&env);
        let mut channels: Map<u64, Channel> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("channels"))
            .unwrap_or(Map::new(&env));

        channels.set(channel_id, channel);
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("channels"), &channels);

        env.events().publish(
            (
                soroban_sdk::symbol_short!("channel"),
                soroban_sdk::symbol_short!("opened"),
            ),
            (channel_id, agent, owner, deposit),
        );

        channel_id
    }

    /// Agent executes a payment to a recipient.
    /// Enforces the spend limit for the current period.
    ///
    /// # Arguments
    /// * `agent` - Must be the authorized agent for this channel
    /// * `channel_id` - Which channel to spend from
    /// * `recipient` - Who receives the payment
    /// * `amount` - How much to send
    /// * `memo` - Optional memo (e.g. API endpoint being paid for)
    pub fn pay(
        env: Env,
        agent: Address,
        channel_id: u64,
        recipient: Address,
        amount: i128,
        memo: soroban_sdk::Bytes,
    ) {
        Self::require_not_paused(&env);

        agent.require_auth();

        let mut channels: Map<u64, Channel> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("channels"))
            .unwrap();

        let mut channel = channels.get(channel_id).expect("channel not found");

        if !channel.active {
            panic!("channel is closed");
        }
        if channel.agent != agent {
            panic!("not the authorized agent");
        }
        if amount <= 0 {
            panic!("amount must be positive");
        }

        // Reset period if needed
        let ledgers_per_period = Self::ledgers_per_period(&channel.period);
        let current_ledger = env.ledger().sequence();

        if current_ledger >= channel.period_start_ledger + ledgers_per_period {
            channel.spent_this_period = 0;
            channel.period_start_ledger = current_ledger;
        }

        // Check spend limit
        if channel.spent_this_period + amount > channel.limit_per_period {
            panic!("spend limit exceeded for this period");
        }

        // Execute the transfer
        let token_client = token::Client::new(&env, &channel.token);
        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        // Update channel state
        channel.spent_this_period += amount;
        channel.total_spent += amount;
        channels.set(channel_id, channel.clone());
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("channels"), &channels);

        // Emit payment event (audit trail)
        env.events().publish(
            (
                soroban_sdk::symbol_short!("channel"),
                soroban_sdk::symbol_short!("paid"),
            ),
            (channel_id, agent, recipient, amount, memo),
        );
    }

    /// Agent executes a payment where the recipient wants a different asset
    /// than the channel's settlement token (`channel.token`) — e.g. the
    /// channel is funded in USDC but this recipient only accepts XLM.
    ///
    /// # Design: normalization stays in `channel.token`, not a new unit
    ///
    /// `amount` here is in `channel.token` units, exactly like `pay()`'s
    /// `amount`. The spend limit (`limit_per_period` / `spent_this_period`)
    /// is therefore *already* normalized: it's always measured in the one
    /// asset this channel ever actually custodies (deposited at
    /// `open_channel` / `top_up`), regardless of which asset the recipient
    /// ends up receiving. There's no need to invent a separate reference
    /// unit for the ledger itself, and doing so would risk changing
    /// same-asset `pay()` behavior — which must stay identical.
    ///
    /// # Design: what the price oracle is actually for
    ///
    /// What *isn't* automatically safe is the conversion itself. This
    /// contract calls out to a configured AMM (`set_amm`) to swap
    /// `channel.token` -> `dest_token`. An AMM's on-chain quote can be
    /// manipulated (e.g. a pool temporarily imbalanced by a large trade in
    /// the same transaction/block): if this contract simply trusted
    /// "whatever the AMM says it paid out," a manipulated pool could report
    /// a fair-looking trade while actually draining far more `dest_token`
    /// value than `amount` of `channel.token` is worth — silently
    /// laundering unbounded value through a single spend-limited `amount`.
    ///
    /// To close that hole, a configured `PriceOracle` (`set_price_oracle`)
    /// supplies an independent, trusted reference price for
    /// `channel.token` -> `dest_token`. The caller's `min_received` must
    /// clear `MAX_SLIPPAGE_BPS` off that reference price *before* any
    /// transfer happens, in addition to being enforced by the AMM itself.
    /// If the oracle has no price for this pair — or `set_price_oracle` /
    /// `set_amm` was never called — the entire call panics and reverts
    /// with nothing transferred and `spent_this_period` untouched: this is
    /// a deliberate fail-safe (unpriced must never mean unlimited).
    ///
    /// This introduces a trust assumption the rest of this contract suite
    /// doesn't otherwise have: whoever holds the `PriceOracle` admin key
    /// controls the effective slippage floor for every cross-asset
    /// payment. See `price_oracle`'s crate docs for why a single admin key
    /// is an explicit, reasonable *starting point* here — and why it
    /// should be replaced with a decentralized/aggregated feed before this
    /// path is used to move meaningful value in production.
    ///
    /// # Arguments
    /// * `agent` - Must be the authorized agent for this channel
    /// * `channel_id` - Which channel to spend from
    /// * `recipient` - Who receives the payment
    /// * `amount` - How much `channel.token` to debit (same unit as `pay()`)
    /// * `dest_token` - Asset the recipient should actually receive
    /// * `min_received` - Slippage floor, in `dest_token` units
    /// * `memo` - Optional memo
    ///
    /// Returns the actual amount of `dest_token` the recipient received.
    pub fn pay_with_conversion(
        env: Env,
        agent: Address,
        channel_id: u64,
        recipient: Address,
        amount: i128,
        dest_token: Address,
        min_received: i128,
        memo: soroban_sdk::Bytes,
    ) -> i128 {
        Self::require_not_paused(&env);

        agent.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }
        if min_received < 0 {
            panic!("min_received cannot be negative");
        }

        let mut channels: Map<u64, Channel> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("channels"))
            .unwrap();

        let mut channel = channels.get(channel_id).expect("channel not found");

        if !channel.active {
            panic!("channel is closed");
        }
        if channel.agent != agent {
            panic!("not the authorized agent");
        }

        // Reset period if needed
        let ledgers_per_period = Self::ledgers_per_period(&channel.period);
        let current_ledger = env.ledger().sequence();

        if current_ledger >= channel.period_start_ledger + ledgers_per_period {
            channel.spent_this_period = 0;
            channel.period_start_ledger = current_ledger;
        }

        // Check spend limit — always in channel.token units, unaffected by
        // which asset the recipient ends up receiving.
        if channel.spent_this_period + amount > channel.limit_per_period {
            panic!("spend limit exceeded for this period");
        }

        let received = if dest_token == channel.token {
            // Same-asset path: behaves exactly like `pay()`. No oracle or
            // AMM call — nothing to convert.
            if min_received > amount {
                panic!("min_received cannot exceed amount for a same-asset payment");
            }
            let token_client = token::Client::new(&env, &channel.token);
            token_client.transfer(&env.current_contract_address(), &recipient, &amount);
            amount
        } else {
            Self::execute_conversion(
                &env,
                &channel.token,
                amount,
                &dest_token,
                min_received,
                &recipient,
            )
        };

        // Update channel state
        channel.spent_this_period += amount;
        channel.total_spent += amount;
        channels.set(channel_id, channel.clone());
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("channels"), &channels);

        // Emit payment event (audit trail)
        env.events().publish(
            (
                soroban_sdk::symbol_short!("channel"),
                soroban_sdk::symbol_short!("convpaid"),
            ),
            (
                channel_id, agent, recipient, amount, dest_token, received, memo,
            ),
        );

        received
    }

    /// Owner tops up a channel with more tokens
    pub fn top_up(env: Env, owner: Address, channel_id: u64, amount: i128) {
        owner.require_auth();

        let channels: Map<u64, Channel> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("channels"))
            .unwrap();

        let channel = channels.get(channel_id).expect("channel not found");

        if channel.owner != owner {
            panic!("not the channel owner");
        }

        let token_client = token::Client::new(&env, &channel.token);
        token_client.transfer(&owner, &env.current_contract_address(), &amount);

        env.events().publish(
            (
                soroban_sdk::symbol_short!("channel"),
                soroban_sdk::symbol_short!("topup"),
            ),
            (channel_id, owner, amount),
        );
    }

    /// Owner closes a channel and reclaims unspent funds
    pub fn close_channel(env: Env, owner: Address, channel_id: u64) {
        owner.require_auth();

        let mut channels: Map<u64, Channel> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("channels"))
            .unwrap();

        let mut channel = channels.get(channel_id).expect("channel not found");

        if channel.owner != owner {
            panic!("not the channel owner");
        }

        channel.active = false;
        channels.set(channel_id, channel.clone());
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("channels"), &channels);

        env.events().publish(
            (
                soroban_sdk::symbol_short!("channel"),
                soroban_sdk::symbol_short!("closed"),
            ),
            (channel_id, owner),
        );
    }

    /// Wire this channel contract up to a deployed CircuitBreaker contract.
    /// The first caller to set it becomes the admin for future rotations.
    pub fn set_circuit_breaker(env: Env, admin: Address, circuit_breaker: Address) {
        admin.require_auth();

        let admin_key = symbol_short!("cb_admin");
        match env.storage().instance().get::<_, Address>(&admin_key) {
            Some(stored_admin) => {
                if stored_admin != admin {
                    panic!("not the circuit breaker admin");
                }
            }
            None => {
                env.storage().instance().set(&admin_key, &admin);
            }
        }

        env.storage()
            .instance()
            .set(&symbol_short!("cb"), &circuit_breaker);
    }

    /// Wire this channel contract up to a deployed `PriceOracle` contract,
    /// used by `pay_with_conversion` as the trusted reference price for
    /// cross-asset conversions. The first caller to set it becomes the
    /// admin for future rotations, mirroring `set_circuit_breaker`.
    pub fn set_price_oracle(env: Env, admin: Address, price_oracle: Address) {
        admin.require_auth();

        let admin_key = symbol_short!("po_admin");
        match env.storage().instance().get::<_, Address>(&admin_key) {
            Some(stored_admin) => {
                if stored_admin != admin {
                    panic!("not the price oracle admin");
                }
            }
            None => {
                env.storage().instance().set(&admin_key, &admin);
            }
        }

        env.storage()
            .instance()
            .set(&symbol_short!("po"), &price_oracle);
    }

    /// Wire this channel contract up to a deployed AMM/DEX contract, used
    /// by `pay_with_conversion` to actually execute cross-asset swaps. The
    /// first caller to set it becomes the admin for future rotations,
    /// mirroring `set_circuit_breaker`.
    pub fn set_amm(env: Env, admin: Address, amm: Address) {
        admin.require_auth();

        let admin_key = symbol_short!("amm_admin");
        match env.storage().instance().get::<_, Address>(&admin_key) {
            Some(stored_admin) => {
                if stored_admin != admin {
                    panic!("not the amm admin");
                }
            }
            None => {
                env.storage().instance().set(&admin_key, &admin);
            }
        }

        env.storage().instance().set(&symbol_short!("amm"), &amm);
    }

    // ── Queries ──────────────────────────────────────────────────────────────

    pub fn get_channel(env: Env, channel_id: u64) -> Channel {
        let channels: Map<u64, Channel> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("channels"))
            .unwrap();
        channels.get(channel_id).expect("channel not found")
    }

    pub fn remaining_this_period(env: Env, channel_id: u64) -> i128 {
        let channel = Self::get_channel(env, channel_id);
        channel.limit_per_period - channel.spent_this_period
    }

    // ── Solvency proofs (ZK) ─────────────────────────────────────────────────

    /// Admin-only: install (or rotate) the Groth16 verifying key used by
    /// `verify_solvency_proof`. The first caller to set it becomes the
    /// admin for future rotations, mirroring `set_circuit_breaker`.
    pub fn set_solvency_vk(env: Env, admin: Address, vk: SolvencyVerifyingKey) {
        admin.require_auth();

        if vk.gamma_abc_g1.len() != 3 {
            panic!("solvency vk must have exactly 3 gamma_abc_g1 entries");
        }

        let admin_key = symbol_short!("sv_admin");
        match env.storage().instance().get::<_, Address>(&admin_key) {
            Some(stored_admin) => {
                if stored_admin != admin {
                    panic!("not the solvency vk admin");
                }
            }
            None => {
                env.storage().instance().set(&admin_key, &admin);
            }
        }

        env.storage().instance().set(&symbol_short!("sv_vk"), &vk);
    }

    /// Verifies a Groth16 proof that some private payment history is
    /// consistent with this channel's own public `limit_per_period` and
    /// `total_spent` — i.e. that some ordering of undisclosed payments into
    /// spend-limit periods never exceeded the limit, and summed to exactly
    /// the channel's recorded total. Returns `true`/`false`; never panics
    /// on a bad proof (only if no verifying key has been configured).
    ///
    /// See `zk/solvency_proof` for the prover and `docs/zk-solvency-design.md`
    /// for the full circuit description.
    pub fn verify_solvency_proof(env: Env, channel_id: u64, proof: SolvencyProof) -> bool {
        let channel = Self::get_channel(env.clone(), channel_id);
        let vk: SolvencyVerifyingKey = env
            .storage()
            .instance()
            .get(&symbol_short!("sv_vk"))
            .expect("solvency verifying key not set");

        let bls = env.crypto().bls12_381();

        let limit_scalar = Fr::from_u256(U256::from_u128(&env, channel.limit_per_period as u128));
        let total_scalar = Fr::from_u256(U256::from_u128(&env, channel.total_spent as u128));

        // vk_x = gamma_abc_g1[0] + limit*gamma_abc_g1[1] + total_spent*gamma_abc_g1[2]
        let public_term = bls.g1_msm(
            Vec::from_array(
                &env,
                [
                    vk.gamma_abc_g1.get(1).unwrap(),
                    vk.gamma_abc_g1.get(2).unwrap(),
                ],
            ),
            Vec::from_array(&env, [limit_scalar, total_scalar]),
        );
        let vk_x = bls.g1_add(&vk.gamma_abc_g1.get(0).unwrap(), &public_term);

        // Groth16 verification equation, rearranged into Soroban's
        // product-of-pairings-equals-identity form:
        //   e(A,B) * e(-vk_x,gamma) * e(-C,delta) * e(-alpha,beta) == 1
        // which holds iff e(A,B) == e(alpha,beta) * e(vk_x,gamma) * e(C,delta).
        let g1_points = Vec::from_array(&env, [proof.a, -vk_x, -proof.c, -vk.alpha_g1]);
        let g2_points = Vec::from_array(&env, [proof.b, vk.gamma_g2, vk.delta_g2, vk.beta_g2]);

        bls.pairing_check(g1_points, g2_points)
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn next_id(env: &Env) -> u64 {
        let count: u64 = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("count"))
            .unwrap_or(0);
        let next = count + 1;
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("count"), &next);
        next
    }

    /// Panics if a CircuitBreaker is configured and reports the system as
    /// paused. If no CircuitBreaker has been wired up via
    /// `set_circuit_breaker`, this is a no-op (fail-open before setup).
    fn require_not_paused(env: &Env) {
        let cb: Option<Address> = env.storage().instance().get(&symbol_short!("cb"));
        if let Some(circuit_breaker) = cb {
            let is_paused: bool = env.invoke_contract(
                &circuit_breaker,
                &Symbol::new(env, "is_paused"),
                Vec::new(env),
            );
            if is_paused {
                panic!("system paused");
            }
        }
    }

    /// Converts `send_amount` of `send_token` into `dest_token` via the
    /// configured AMM, bounded below by both the caller's `min_received`
    /// and an independent `PriceOracle`-derived fair-value floor. See
    /// `pay_with_conversion`'s doc comment for the full rationale. Panics
    /// (reverting the whole call, no partial transfers) if the oracle or
    /// AMM aren't configured, if the oracle has no price for this pair, or
    /// if the trade doesn't clear either floor.
    fn execute_conversion(
        env: &Env,
        send_token: &Address,
        send_amount: i128,
        dest_token: &Address,
        min_received: i128,
        recipient: &Address,
    ) -> i128 {
        let price_oracle: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("po"))
            .expect("price oracle not configured");
        let amm: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("amm"))
            .expect("amm not configured");

        // Independent, trusted reference price. Fails safe: if the oracle
        // has no quote for this pair, this panics and the whole payment
        // reverts rather than proceeding unpriced.
        let price: i128 = env.invoke_contract(
            &price_oracle,
            &Symbol::new(env, "get_price"),
            Vec::from_array(env, [send_token.into_val(env), dest_token.into_val(env)]),
        );
        if price <= 0 {
            panic!("invalid price from oracle");
        }

        let expected_dest = send_amount
            .checked_mul(price)
            .expect("overflow computing expected output")
            / PRICE_SCALE;
        let floor = expected_dest * (BPS_DENOMINATOR - MAX_SLIPPAGE_BPS) / BPS_DENOMINATOR;
        if min_received < floor {
            panic!("slippage tolerance exceeds maximum allowed deviation from oracle price");
        }

        // Push funds to the AMM (self-authorized: this contract is the
        // direct caller/source, same pattern `pay()` uses to pay
        // recipients directly), then ask it to execute the swap.
        let send_client = token::Client::new(env, send_token);
        send_client.transfer(&env.current_contract_address(), &amm, &send_amount);

        // The AMM itself enforces `min_out` and panics (reverting this
        // whole call, including the transfer above) if it can't clear it,
        // so `received >= min_received` is guaranteed here rather than
        // re-checked.
        env.invoke_contract(
            &amm,
            &Symbol::new(env, "execute_swap"),
            Vec::from_array(
                env,
                [
                    send_token.into_val(env),
                    send_amount.into_val(env),
                    dest_token.into_val(env),
                    min_received.into_val(env),
                    recipient.into_val(env),
                ],
            ),
        )
    }

    fn ledgers_per_period(period: &SpendPeriod) -> u32 {
        match period {
            SpendPeriod::PerLedger => 1,
            SpendPeriod::Hourly => 720, // ~5s ledgers
            SpendPeriod::Daily => 17_280,
        }
    }
}

#[cfg(test)]
mod test;
