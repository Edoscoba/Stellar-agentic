#![no_std]

//! # Rate Limiter Contract
//!
//! Prevents runaway agents from draining wallets.
//! Enforces per-transaction, per-minute, and per-hour caps on-chain.
//! Works as a standalone guard composable with PaymentChannel.

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Map};

// ─── Types ───────────────────────────────────────────────────────────────────

/// Rate limit configuration for an agent
#[contracttype]
#[derive(Clone, Debug)]
pub struct RateLimit {
    /// The agent being rate-limited
    pub agent: Address,
    /// The owner who set this limit
    pub owner: Address,
    /// Max per single transaction
    pub max_per_tx: i128,
    /// Max total per hour (~720 ledgers)
    pub max_per_hour: i128,
    /// Max total per day (~17280 ledgers)
    pub max_per_day: i128,
    /// Max number of transactions per hour
    pub max_txs_per_hour: u32,

    // ── Rolling window state ──
    pub hourly_spend: i128,
    pub daily_spend: i128,
    pub hourly_tx_count: u32,
    pub hour_window_start: u32,
    pub day_window_start: u32,

    pub active: bool,
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct RateLimiter;

#[contractimpl]
impl RateLimiter {
    /// Register rate limits for an agent.
    /// Call this when setting up a new agent.
    pub fn set_limits(
        env: Env,
        owner: Address,
        agent: Address,
        max_per_tx: i128,
        max_per_hour: i128,
        max_per_day: i128,
        max_txs_per_hour: u32,
    ) {
        owner.require_auth();

        if max_per_tx <= 0 || max_per_hour <= 0 || max_per_day <= 0 {
            panic!("limits must be positive");
        }
        if max_per_hour > max_per_day {
            panic!("hourly limit cannot exceed daily limit");
        }
        if max_per_tx > max_per_hour {
            panic!("per-tx limit cannot exceed hourly limit");
        }

        let current_ledger = env.ledger().sequence();
        let limit = RateLimit {
            agent: agent.clone(),
            owner,
            max_per_tx,
            max_per_hour,
            max_per_day,
            max_txs_per_hour,
            hourly_spend: 0,
            daily_spend: 0,
            hourly_tx_count: 0,
            hour_window_start: current_ledger,
            day_window_start: current_ledger,
            active: true,
        };

        Self::save_limit(&env, &agent, limit.clone());
        env.events().publish(
            (
                soroban_sdk::symbol_short!("state"),
                soroban_sdk::symbol_short!("limit"),
            ),
            (agent, limit),
        );
    }

    /// Check if a proposed payment passes rate limits.
    /// Returns true if allowed, false if it would be blocked.
    /// Does NOT modify state — call `record_payment` after a successful tx.
    pub fn check(env: Env, agent: Address, amount: i128) -> bool {
        if !Self::has_limit(&env, &agent) {
            return true; // no limit configured = allow
        }

        let mut limit = Self::load_limit(&env, &agent);
        let current_ledger = env.ledger().sequence();

        // Reset windows if expired
        Self::reset_windows_if_needed(&mut limit, current_ledger);

        // Per-tx check
        if amount > limit.max_per_tx {
            return false;
        }

        // Hourly spend check
        if limit.hourly_spend + amount > limit.max_per_hour {
            return false;
        }

        // Daily spend check
        if limit.daily_spend + amount > limit.max_per_day {
            return false;
        }

        // Hourly tx count check
        if limit.hourly_tx_count >= limit.max_txs_per_hour {
            return false;
        }

        true
    }

    /// Record a payment after it has been successfully executed.
    /// Must be called by the payment channel or an authorized contract.
    pub fn record_payment(env: Env, recorder: Address, agent: Address, amount: i128) {
        recorder.require_auth();

        if !Self::has_limit(&env, &agent) {
            return;
        }

        let mut limit = Self::load_limit(&env, &agent);
        let current_ledger = env.ledger().sequence();

        Self::reset_windows_if_needed(&mut limit, current_ledger);

        limit.hourly_spend += amount;
        limit.daily_spend += amount;
        limit.hourly_tx_count += 1;

        Self::save_limit(&env, &agent, limit.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("rl"),
                soroban_sdk::symbol_short!("recorded"),
            ),
            (agent.clone(), amount),
        );
        env.events().publish(
            (
                soroban_sdk::symbol_short!("state"),
                soroban_sdk::symbol_short!("limit"),
            ),
            (agent, limit),
        );
    }

    /// Owner can update limits for an agent
    pub fn update_limits(
        env: Env,
        owner: Address,
        agent: Address,
        max_per_tx: i128,
        max_per_hour: i128,
        max_per_day: i128,
        max_txs_per_hour: u32,
    ) {
        owner.require_auth();

        let mut limit = Self::load_limit(&env, &agent);

        if limit.owner != owner {
            panic!("not the limit owner");
        }

        limit.max_per_tx = max_per_tx;
        limit.max_per_hour = max_per_hour;
        limit.max_per_day = max_per_day;
        limit.max_txs_per_hour = max_txs_per_hour;

        Self::save_limit(&env, &agent, limit.clone());
        env.events().publish(
            (
                soroban_sdk::symbol_short!("state"),
                soroban_sdk::symbol_short!("limit"),
            ),
            (agent, limit),
        );
    }

    /// Emergency kill switch — disable an agent immediately
    pub fn kill_agent(env: Env, owner: Address, agent: Address) {
        owner.require_auth();

        let mut limit = Self::load_limit(&env, &agent);

        if limit.owner != owner {
            panic!("not the limit owner");
        }

        limit.active = false;
        Self::save_limit(&env, &agent, limit.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("rl"),
                soroban_sdk::symbol_short!("killed"),
            ),
            agent.clone(),
        );
        env.events().publish(
            (
                soroban_sdk::symbol_short!("state"),
                soroban_sdk::symbol_short!("limit"),
            ),
            (agent, limit),
        );
    }

    // ── Queries ──────────────────────────────────────────────────────────────

    pub fn get_limits(env: Env, agent: Address) -> RateLimit {
        Self::load_limit(&env, &agent)
    }

    pub fn is_active(env: Env, agent: Address) -> bool {
        if !Self::has_limit(&env, &agent) {
            return true;
        }
        Self::load_limit(&env, &agent).active
    }

    // ── Internals ────────────────────────────────────────────────────────────

    fn reset_windows_if_needed(limit: &mut RateLimit, current_ledger: u32) {
        const LEDGERS_PER_HOUR: u32 = 720;
        const LEDGERS_PER_DAY: u32 = 17_280;

        if current_ledger >= limit.hour_window_start + LEDGERS_PER_HOUR {
            limit.hourly_spend = 0;
            limit.hourly_tx_count = 0;
            limit.hour_window_start = current_ledger;
        }

        if current_ledger >= limit.day_window_start + LEDGERS_PER_DAY {
            limit.daily_spend = 0;
            limit.day_window_start = current_ledger;
        }
    }

    fn has_limit(env: &Env, agent: &Address) -> bool {
        let limits: Map<Address, RateLimit> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("limits"))
            .unwrap_or(Map::new(env));
        limits.contains_key(agent.clone())
    }

    fn load_limit(env: &Env, agent: &Address) -> RateLimit {
        let limits: Map<Address, RateLimit> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("limits"))
            .unwrap();
        limits.get(agent.clone()).expect("no rate limit for agent")
    }

    fn save_limit(env: &Env, agent: &Address, limit: RateLimit) {
        let mut limits: Map<Address, RateLimit> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("limits"))
            .unwrap_or(Map::new(env));
        limits.set(agent.clone(), limit);
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("limits"), &limits);
    }
}
