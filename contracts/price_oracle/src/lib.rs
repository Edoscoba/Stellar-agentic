#![no_std]

//! # Price Oracle Contract
//!
//! A minimal, single-admin price-feed contract used by `PaymentChannel` to
//! get a trusted reference price when converting between assets in
//! `pay_with_conversion`.
//!
//! ## Trust model
//!
//! This is intentionally the simplest possible design: one admin key
//! publishes prices via `set_price`, and every reader trusts whatever was
//! last published. That is a **new external trust dependency** the rest of
//! this contract suite doesn't otherwise have — `PaymentChannel`,
//! `Escrow`, and `RateLimiter` are all self-contained modulo the
//! `CircuitBreaker` (which itself requires a 5-of-N quorum of trusted
//! nodes, not a single key).
//!
//! A single admin is a reasonable *starting point* (explicitly suggested
//! by the design brief this contract implements) because it keeps the
//! on-chain surface area small and easy to audit, and because
//! `PaymentChannel` only ever uses the price as a *safety bound* on top of
//! an already-executed AMM swap (see `pay_with_conversion`'s doc comment)
//! rather than as the sole determinant of how much value moves. It is
//! **not** sufficient for a production deployment moving meaningful value:
//! a compromised or careless admin can publish a bad price and degrade the
//! slippage protection it's supposed to provide. Before mainnet use this
//! should be replaced with (or layered under) a decentralized/aggregated
//! feed such as Reflector on Stellar, or a multi-admin quorum mirroring
//! `CircuitBreaker`'s trusted-node model.
//!
//! ## Price representation
//!
//! `set_price(admin, base, quote, price)` publishes: `price` units of
//! `quote`'s base-units are equivalent to `PRICE_SCALE` (1e7, matching
//! Stellar's 7-decimal-place convention) base-units of `base`. i.e.
//! `quote_amount = base_amount * price / PRICE_SCALE`.
//!
//! `get_price` panics if no price has been published for the requested
//! pair. Callers (e.g. `PaymentChannel`) MUST treat that panic as "price
//! unavailable" and abort the whole operation rather than assuming a
//! fallback rate — an unpriced conversion must never be treated as an
//! unlimited/free one.

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Map};

/// Fixed-point scale for published prices, matching Stellar's 7-decimal
/// convention. Must match `payment_channel::PRICE_SCALE`.
pub const PRICE_SCALE: i128 = 10_000_000;

#[contract]
pub struct PriceOracle;

#[contractimpl]
impl PriceOracle {
    /// One-time setup. The first caller becomes the admin.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("already initialized");
        }
        admin.require_auth();
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
    }

    /// Admin-only: publish (or update) the trusted price of `base` in terms
    /// of `quote`. See module docs for the fixed-point convention.
    pub fn set_price(env: Env, admin: Address, base: Address, quote: Address, price: i128) {
        Self::require_admin(&env, &admin);

        if price <= 0 {
            panic!("price must be positive");
        }

        let mut by_base = Self::prices_for(&env, &base);
        by_base.set(quote, price);
        Self::save_prices_for(&env, &base, &by_base);

        env.events().publish(
            (symbol_short!("oracle"), symbol_short!("price")),
            (base, price),
        );
    }

    /// Returns the trusted price of `base` in terms of `quote`. Identity
    /// pairs (`base == quote`) always return `PRICE_SCALE` without needing
    /// a stored entry. Panics if no price has been published — callers
    /// must fail safe on this, not substitute a default rate.
    pub fn get_price(env: Env, base: Address, quote: Address) -> i128 {
        if base == quote {
            return PRICE_SCALE;
        }
        let by_base = Self::prices_for(&env, &base);
        by_base.get(quote).expect("price not available")
    }

    /// Whether a price has been published for this pair, without panicking.
    /// Useful for callers that want to check availability before spending
    /// gas on a call that would otherwise panic.
    pub fn has_price(env: Env, base: Address, quote: Address) -> bool {
        if base == quote {
            return true;
        }
        Self::prices_for(&env, &base).contains_key(quote)
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .expect("not initialized")
    }

    // ── Internals ────────────────────────────────────────────────────────────

    fn require_admin(env: &Env, admin: &Address) {
        admin.require_auth();
        let stored: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .expect("not initialized");
        if stored != *admin {
            panic!("not the admin");
        }
    }

    fn prices_for(env: &Env, base: &Address) -> Map<Address, i128> {
        let all: Map<Address, Map<Address, i128>> = env
            .storage()
            .instance()
            .get(&symbol_short!("prices"))
            .unwrap_or(Map::new(env));
        all.get(base.clone()).unwrap_or(Map::new(env))
    }

    fn save_prices_for(env: &Env, base: &Address, by_base: &Map<Address, i128>) {
        let mut all: Map<Address, Map<Address, i128>> = env
            .storage()
            .instance()
            .get(&symbol_short!("prices"))
            .unwrap_or(Map::new(env));
        all.set(base.clone(), by_base.clone());
        env.storage().instance().set(&symbol_short!("prices"), &all);
    }
}

#[cfg(test)]
mod test;
