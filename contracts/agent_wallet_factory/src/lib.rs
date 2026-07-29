#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, Env, Map, String, Vec,
};

// ─── Storage Keys ────────────────────────────────────────────────────────────

const ADMIN_KEY: &str = "admin";
const AGENTS_KEY: &str = "agents";
const AGENT_COUNT_KEY: &str = "agent_count";

// ─── Data Types ──────────────────────────────────────────────────────────────

/// Metadata stored on-chain for each agent wallet
#[contracttype]
#[derive(Clone, Debug)]
pub struct AgentInfo {
    /// The agent's Stellar address (its wallet)
    pub address: Address,
    /// Human-readable name for the agent
    pub name: String,
    /// Owner who controls this agent
    pub owner: Address,
    /// Whether this agent is currently active
    pub active: bool,
    /// Ledger number when the agent was created
    pub created_at: u32,
    /// Total operations this agent has performed
    pub total_ops: u64,
}

/// Events emitted by this contract
#[contracttype]
pub enum Event {
    AgentCreated,
    AgentDeactivated,
    AgentReactivated,
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct AgentWalletFactory;

#[contractimpl]
impl AgentWalletFactory {
    // ── Initialization ───────────────────────────────────────────────────────

    /// Initialize the factory with an admin address.
    /// Must be called once after deployment.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("already initialized");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
        env.storage().instance().set(&symbol_short!("count"), &0u64);

        // Initialize empty agents map
        let agents: Map<u64, AgentInfo> = Map::new(&env);
        env.storage()
            .instance()
            .set(&symbol_short!("agents"), &agents);
    }

    // ── Agent Management ─────────────────────────────────────────────────────

    /// Create a new agent wallet.
    /// The caller becomes the owner of this agent.
    ///
    /// # Arguments
    /// * `owner` - The address that will own and control this agent
    /// * `agent_address` - The Stellar address of the agent's wallet
    /// * `name` - A human-readable name for the agent
    ///
    /// # Returns
    /// The agent ID (incrementing counter)
    pub fn create_agent(env: Env, owner: Address, agent_address: Address, name: String) -> u64 {
        // Require the owner to authorize this call
        owner.require_auth();

        // Get current agent count and increment
        let count: u64 = env
            .storage()
            .instance()
            .get(&symbol_short!("count"))
            .unwrap_or(0);
        let agent_id = count + 1;

        // Build agent info
        let agent = AgentInfo {
            address: agent_address.clone(),
            name,
            owner: owner.clone(),
            active: true,
            created_at: env.ledger().sequence(),
            total_ops: 0,
        };

        // Store agent
        let mut agents: Map<u64, AgentInfo> = env
            .storage()
            .instance()
            .get(&symbol_short!("agents"))
            .unwrap_or(Map::new(&env));

        agents.set(agent_id, agent.clone());

        env.storage()
            .instance()
            .set(&symbol_short!("agents"), &agents);
        env.storage()
            .instance()
            .set(&symbol_short!("count"), &agent_id);

        // Emit creation event
        env.events().publish(
            (symbol_short!("factory"), symbol_short!("created")),
            (agent_id, agent_address, owner),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("agent")),
            (agent_id, agent),
        );

        agent_id
    }

    /// Deactivate an agent. Only the owner can deactivate their agent.
    pub fn deactivate_agent(env: Env, owner: Address, agent_id: u64) {
        owner.require_auth();

        let mut agents: Map<u64, AgentInfo> = env
            .storage()
            .instance()
            .get(&symbol_short!("agents"))
            .unwrap();

        let mut agent = agents.get(agent_id).expect("agent not found");

        if agent.owner != owner {
            panic!("not the agent owner");
        }

        agent.active = false;
        agents.set(agent_id, agent.clone());
        env.storage()
            .instance()
            .set(&symbol_short!("agents"), &agents);

        env.events().publish(
            (symbol_short!("factory"), symbol_short!("deactiv")),
            (agent_id, owner),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("agent")),
            (agent_id, agent),
        );
    }

    /// Reactivate a previously deactivated agent.
    pub fn reactivate_agent(env: Env, owner: Address, agent_id: u64) {
        owner.require_auth();

        let mut agents: Map<u64, AgentInfo> = env
            .storage()
            .instance()
            .get(&symbol_short!("agents"))
            .unwrap();

        let mut agent = agents.get(agent_id).expect("agent not found");

        if agent.owner != owner {
            panic!("not the agent owner");
        }

        agent.active = true;
        agents.set(agent_id, agent.clone());
        env.storage()
            .instance()
            .set(&symbol_short!("agents"), &agents);

        env.events().publish(
            (symbol_short!("factory"), symbol_short!("reactiv")),
            (agent_id, owner),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("agent")),
            (agent_id, agent),
        );
    }

    /// Increment the operation counter for an agent.
    /// Called by the PaymentChannel contract after a successful payment.
    pub fn record_operation(env: Env, agent_id: u64) {
        let mut agents: Map<u64, AgentInfo> = env
            .storage()
            .instance()
            .get(&symbol_short!("agents"))
            .unwrap();

        let mut agent = agents.get(agent_id).expect("agent not found");
        agent.total_ops += 1;
        agents.set(agent_id, agent.clone());
        env.storage()
            .instance()
            .set(&symbol_short!("agents"), &agents);
        env.events().publish(
            (symbol_short!("state"), symbol_short!("agent")),
            (agent_id, agent),
        );
    }

    // ── Queries ──────────────────────────────────────────────────────────────

    /// Get agent info by ID
    pub fn get_agent(env: Env, agent_id: u64) -> AgentInfo {
        let agents: Map<u64, AgentInfo> = env
            .storage()
            .instance()
            .get(&symbol_short!("agents"))
            .unwrap();

        agents.get(agent_id).expect("agent not found")
    }

    /// Get all agents owned by a specific address
    pub fn get_agents_by_owner(env: Env, owner: Address) -> Vec<AgentInfo> {
        let agents: Map<u64, AgentInfo> = env
            .storage()
            .instance()
            .get(&symbol_short!("agents"))
            .unwrap_or(Map::new(&env));

        let mut result = Vec::new(&env);
        let count: u64 = env
            .storage()
            .instance()
            .get(&symbol_short!("count"))
            .unwrap_or(0);

        for i in 1..=count {
            if let Some(agent) = agents.get(i) {
                if agent.owner == owner {
                    result.push_back(agent);
                }
            }
        }

        result
    }

    /// Total number of agents ever created
    pub fn total_agents(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&symbol_short!("count"))
            .unwrap_or(0)
    }

    /// Check if a specific address is a registered active agent
    pub fn is_active_agent(env: Env, address: Address) -> bool {
        let agents: Map<u64, AgentInfo> = env
            .storage()
            .instance()
            .get(&symbol_short!("agents"))
            .unwrap_or(Map::new(&env));

        let count: u64 = env
            .storage()
            .instance()
            .get(&symbol_short!("count"))
            .unwrap_or(0);

        for i in 1..=count {
            if let Some(agent) = agents.get(i) {
                if agent.address == address && agent.active {
                    return true;
                }
            }
        }

        false
    }

    /// Get the contract admin
    pub fn admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("admin"))
            .unwrap()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{Env, String};

    fn setup() -> (Env, AgentWalletFactoryClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AgentWalletFactory);
        let client = AgentWalletFactoryClient::new(&env, &contract_id);
        (env, client)
    }

    #[test]
    fn test_initialize() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        client.initialize(&admin);
        assert_eq!(client.admin(), admin);
        assert_eq!(client.total_agents(), 0);
    }

    #[test]
    fn test_create_agent() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let agent_addr = Address::generate(&env);

        client.initialize(&admin);

        let agent_id = client.create_agent(&owner, &agent_addr, &String::from_str(&env, "MyAgent"));

        assert_eq!(agent_id, 1);
        assert_eq!(client.total_agents(), 1);

        let agent = client.get_agent(&1);
        assert_eq!(agent.owner, owner);
        assert_eq!(agent.address, agent_addr);
        assert!(agent.active);
        assert_eq!(agent.total_ops, 0);
    }

    #[test]
    fn test_deactivate_reactivate() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let agent_addr = Address::generate(&env);

        client.initialize(&admin);
        let agent_id =
            client.create_agent(&owner, &agent_addr, &String::from_str(&env, "TestAgent"));

        client.deactivate_agent(&owner, &agent_id);
        assert!(!client.get_agent(&agent_id).active);

        client.reactivate_agent(&owner, &agent_id);
        assert!(client.get_agent(&agent_id).active);
    }

    #[test]
    fn test_get_agents_by_owner() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);

        client.initialize(&admin);

        client.create_agent(
            &owner,
            &Address::generate(&env),
            &String::from_str(&env, "Agent1"),
        );
        client.create_agent(
            &owner,
            &Address::generate(&env),
            &String::from_str(&env, "Agent2"),
        );

        let agents = client.get_agents_by_owner(&owner);
        assert_eq!(agents.len(), 2);
    }

    #[test]
    #[should_panic(expected = "not the agent owner")]
    fn test_deactivate_wrong_owner_panics() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let owner = Address::generate(&env);
        let attacker = Address::generate(&env);

        client.initialize(&admin);
        let agent_id = client.create_agent(
            &owner,
            &Address::generate(&env),
            &String::from_str(&env, "Agent"),
        );

        client.deactivate_agent(&attacker, &agent_id);
    }
}
