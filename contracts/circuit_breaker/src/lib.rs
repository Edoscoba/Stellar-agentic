#![no_std]

//! # Circuit Breaker Contract
//!
//! Multi-sig emergency pause mechanism for the agent payment rails.
//!
//! A fixed, on-chain-stored set of trusted node addresses (never secret
//! keys — those must stay off-chain and client-side only) can each call
//! `propose_pause`. Once >= [`QUORUM`] distinct trusted nodes have proposed
//! a pause within `propose_window_ledgers` of one another, anyone can call
//! `execute_pause` to flip the global `is_paused` flag. `unpause` works the
//! same way, gated behind a second, independent quorum of proposals.
//!
//! Other contracts (e.g. `PaymentChannel`, `Escrow`) call `is_paused` at the
//! top of their state-changing entry points and refuse to operate while the
//! system is paused — this is the actual emergency-stop mechanism.
//!
//! The trusted node set itself is rotated only by the admin, via
//! `set_trusted_nodes`.

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Map, Vec};

/// Minimum number of distinct trusted nodes that must propose (within the
/// validity window) before a pause or unpause can be executed.
pub const QUORUM: u32 = 5;

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Admin,
    TrustedNodes,
    ProposeWindow,
    PauseProposals,
    UnpauseProposals,
    Paused,
}

#[contract]
pub struct CircuitBreaker;

#[contractimpl]
impl CircuitBreaker {
    /// One-time setup. Stores the admin, the initial trusted node set, and
    /// the proposal validity window (in ledgers).
    pub fn initialize(
        env: Env,
        admin: Address,
        trusted_nodes: Vec<Address>,
        propose_window_ledgers: u32,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        admin.require_auth();

        if propose_window_ledgers == 0 {
            panic!("propose window must be positive");
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::TrustedNodes, &trusted_nodes);
        env.storage()
            .instance()
            .set(&DataKey::ProposeWindow, &propose_window_ledgers);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::PauseProposals, &Map::<Address, u32>::new(&env));
        env.storage()
            .instance()
            .set(&DataKey::UnpauseProposals, &Map::<Address, u32>::new(&env));
    }

    /// Admin-only: rotate the trusted node set. Clears any in-flight
    /// proposals so a stale proposal from a removed node can never count,
    /// and a rotation can't be used to instantly complete a quorum that was
    /// forming under the old set.
    pub fn set_trusted_nodes(env: Env, admin: Address, trusted_nodes: Vec<Address>) {
        Self::require_admin(&env, &admin);

        env.storage()
            .instance()
            .set(&DataKey::TrustedNodes, &trusted_nodes);
        env.storage()
            .instance()
            .set(&DataKey::PauseProposals, &Map::<Address, u32>::new(&env));
        env.storage()
            .instance()
            .set(&DataKey::UnpauseProposals, &Map::<Address, u32>::new(&env));

        env.events()
            .publish((symbol_short!("cb"), symbol_short!("nodes")), trusted_nodes);
    }

    /// Admin-only: update how long a proposal remains valid.
    pub fn set_propose_window(env: Env, admin: Address, propose_window_ledgers: u32) {
        Self::require_admin(&env, &admin);

        if propose_window_ledgers == 0 {
            panic!("propose window must be positive");
        }
        env.storage()
            .instance()
            .set(&DataKey::ProposeWindow, &propose_window_ledgers);
    }

    /// A trusted node records its approval to pause the system.
    pub fn propose_pause(env: Env, node: Address) {
        node.require_auth();
        Self::require_trusted(&env, &node);

        let mut proposals = Self::pause_proposals(&env);
        proposals.set(node.clone(), env.ledger().sequence());
        env.storage()
            .instance()
            .set(&DataKey::PauseProposals, &proposals);

        env.events()
            .publish((symbol_short!("cb"), symbol_short!("propose")), node);
    }

    /// Flip `is_paused` to true once quorum has been reached among proposals
    /// still within the validity window. Callable by anyone once quorum is
    /// met — the security comes from requiring `QUORUM` distinct trusted
    /// signers to have proposed, not from restricting who can flip the
    /// switch afterwards.
    pub fn execute_pause(env: Env) {
        let proposals = Self::pause_proposals(&env);
        let count = Self::count_valid(&env, &proposals);
        if count < QUORUM {
            panic!("quorum not reached");
        }

        env.storage().instance().set(&DataKey::Paused, &true);
        env.storage()
            .instance()
            .set(&DataKey::PauseProposals, &Map::<Address, u32>::new(&env));

        env.events()
            .publish((symbol_short!("cb"), symbol_short!("paused")), true);
    }

    /// A trusted node records its approval to unpause the system.
    pub fn propose_unpause(env: Env, node: Address) {
        node.require_auth();
        Self::require_trusted(&env, &node);

        let mut proposals = Self::unpause_proposals(&env);
        proposals.set(node.clone(), env.ledger().sequence());
        env.storage()
            .instance()
            .set(&DataKey::UnpauseProposals, &proposals);

        env.events()
            .publish((symbol_short!("cb"), symbol_short!("unpropos")), node);
    }

    /// Flip `is_paused` back to false once unpause quorum is reached.
    pub fn unpause(env: Env) {
        let proposals = Self::unpause_proposals(&env);
        let count = Self::count_valid(&env, &proposals);
        if count < QUORUM {
            panic!("quorum not reached");
        }

        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::UnpauseProposals, &Map::<Address, u32>::new(&env));

        env.events()
            .publish((symbol_short!("cb"), symbol_short!("unpause")), true);
    }

    // ── Queries ──────────────────────────────────────────────────────────────

    /// Whether the system is currently paused. Defaults to `false` if the
    /// contract has not been initialized (fail-open before setup).
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized")
    }

    pub fn get_trusted_nodes(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::TrustedNodes)
            .unwrap_or(Vec::new(&env))
    }

    pub fn get_propose_window(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ProposeWindow)
            .unwrap_or(0)
    }

    /// Number of distinct trusted-node pause proposals currently within the
    /// validity window (i.e. how close the system is to a quorum pause).
    pub fn pause_quorum_count(env: Env) -> u32 {
        let proposals = Self::pause_proposals(&env);
        Self::count_valid(&env, &proposals)
    }

    /// Number of distinct trusted-node unpause proposals currently within
    /// the validity window.
    pub fn unpause_quorum_count(env: Env) -> u32 {
        let proposals = Self::unpause_proposals(&env);
        Self::count_valid(&env, &proposals)
    }

    // ── Internals ────────────────────────────────────────────────────────────

    fn require_admin(env: &Env, admin: &Address) {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        if stored_admin != *admin {
            panic!("not the admin");
        }
    }

    fn require_trusted(env: &Env, node: &Address) {
        let trusted: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::TrustedNodes)
            .unwrap_or(Vec::new(env));
        if !trusted.contains(node) {
            panic!("not a trusted node");
        }
    }

    fn pause_proposals(env: &Env) -> Map<Address, u32> {
        env.storage()
            .instance()
            .get(&DataKey::PauseProposals)
            .unwrap_or(Map::new(env))
    }

    fn unpause_proposals(env: &Env) -> Map<Address, u32> {
        env.storage()
            .instance()
            .get(&DataKey::UnpauseProposals)
            .unwrap_or(Map::new(env))
    }

    /// Counts distinct *currently trusted* nodes whose proposal is still
    /// within `propose_window_ledgers` of the current ledger. A stale
    /// proposal from days ago falls out of the window and stops counting;
    /// a proposal from a node that has since been rotated out never counts
    /// (we only iterate the current trusted set).
    fn count_valid(env: &Env, proposals: &Map<Address, u32>) -> u32 {
        let trusted: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::TrustedNodes)
            .unwrap_or(Vec::new(env));
        let window: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ProposeWindow)
            .unwrap_or(0);
        let current_ledger = env.ledger().sequence();
        let cutoff = current_ledger.saturating_sub(window);

        let mut count = 0u32;
        for node in trusted.iter() {
            if let Some(proposed_at) = proposals.get(node) {
                if proposed_at >= cutoff {
                    count += 1;
                }
            }
        }
        count
    }
}

#[cfg(test)]
mod test;
