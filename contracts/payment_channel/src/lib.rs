#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env, Map};

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

    fn ledgers_per_period(period: &SpendPeriod) -> u32 {
        match period {
            SpendPeriod::PerLedger => 1,
            SpendPeriod::Hourly => 720, // ~5s ledgers
            SpendPeriod::Daily => 17_280,
        }
    }
}
