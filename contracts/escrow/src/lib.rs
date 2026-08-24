#![no_std]

//! # Escrow Contract
//!
//! Enables agent-to-agent job delegation with trustless payment.
//!
//! Flow:
//! 1. Agent A calls `create_job` — locks funds in escrow
//! 2. Agent B accepts and completes the job
//! 3. Agent B calls `submit_result` with proof of work
//! 4. Agent A (or arbiter) calls `release` — funds go to Agent B
//! 5. If Agent B doesn't deliver, Agent A calls `refund` after deadline

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Bytes, Env, Map, Symbol,
    Vec,
};

#[cfg(test)]
mod test;

const DISPUTE_TIMEOUT_LEDGERS: u32 = 14400;

// ─── Types ───────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus {
    /// Funded and waiting for a worker
    Open,
    /// Worker has accepted
    InProgress,
    /// Worker submitted result, awaiting release
    PendingRelease,
    /// Funds released to worker — job done
    Completed,
    /// Refunded to requester
    Refunded,
    /// Disputed — waiting for arbiter
    Disputed,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Job {
    /// Agent requesting the work
    pub requester: Address,
    /// Agent doing the work (set on accept)
    pub worker: Option<Address>,
    /// Optional trusted arbiter for disputes
    pub arbiter: Option<Address>,
    /// Payment token
    pub token: Address,
    /// Locked payment amount
    pub amount: i128,
    /// IPFS hash or description of the task
    pub task_description: Bytes,
    /// Result submitted by worker
    pub result: Option<Bytes>,
    /// Ledger deadline — refund available after this
    pub deadline_ledger: u32,
    pub dispute_deadline_ledger: Option<u32>,
    pub status: JobStatus,
    pub created_at: u32,
}

// ─── Contract ────────────────────────────────────────────────────────────────

#[contract]
pub struct Escrow;

#[contractimpl]
impl Escrow {
    /// Create an escrow job. Locks `amount` tokens from requester.
    ///
    /// # Arguments
    /// * `requester` - Agent requesting the work
    /// * `token` - Token to pay with
    /// * `amount` - Payment for completing the job
    /// * `task_description` - Description / IPFS hash of what needs doing
    /// * `deadline_ledger` - After this ledger, requester can claim a refund
    /// * `arbiter` - Optional trusted address for dispute resolution
    pub fn create_job(
        env: Env,
        requester: Address,
        token: Address,
        amount: i128,
        task_description: Bytes,
        deadline_ledger: u32,
        arbiter: Option<Address>,
    ) -> u64 {
        requester.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }
        if deadline_ledger <= env.ledger().sequence() {
            panic!("deadline must be in the future");
        }

        // Lock funds in this contract
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&requester, &env.current_contract_address(), &amount);

        let job_id = Self::next_id(&env);
        let job = Job {
            requester: requester.clone(),
            worker: None,
            arbiter,
            token,
            amount,
            task_description,
            result: None,
            deadline_ledger,
            dispute_deadline_ledger: None,
            status: JobStatus::Open,
            created_at: env.ledger().sequence(),
        };

        Self::save_job(&env, job_id, job.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("escrow"),
                soroban_sdk::symbol_short!("created"),
            ),
            (job_id, requester, amount),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("job")),
            (job_id, job),
        );

        job_id
    }

    /// Worker agent accepts an open job
    pub fn accept_job(env: Env, worker: Address, job_id: u64) {
        worker.require_auth();

        let mut job = Self::load_job(&env, job_id);

        if job.status != JobStatus::Open {
            panic!("job is not open");
        }
        if env.ledger().sequence() >= job.deadline_ledger {
            panic!("job has expired");
        }

        job.worker = Some(worker.clone());
        job.status = JobStatus::InProgress;
        Self::save_job(&env, job_id, job.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("escrow"),
                soroban_sdk::symbol_short!("accepted"),
            ),
            (job_id, worker),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("job")),
            (job_id, job),
        );
    }

    /// Worker submits their result. Requester must then release or dispute.
    ///
    /// # Arguments
    /// * `result` - Proof of work (IPFS hash, output hash, etc.)
    pub fn submit_result(env: Env, worker: Address, job_id: u64, result: Bytes) {
        worker.require_auth();

        let mut job = Self::load_job(&env, job_id);

        if job.status != JobStatus::InProgress {
            panic!("job not in progress");
        }

        let assigned_worker = job.worker.as_ref().expect("no worker assigned");
        if *assigned_worker != worker {
            panic!("not the assigned worker");
        }

        job.result = Some(result);
        job.status = JobStatus::PendingRelease;
        Self::save_job(&env, job_id, job.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("escrow"),
                soroban_sdk::symbol_short!("result"),
            ),
            (job_id, worker),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("job")),
            (job_id, job),
        );
    }

    /// Requester (or arbiter) releases payment to the worker
    pub fn release(env: Env, releaser: Address, job_id: u64) {
        Self::require_not_paused(&env);

        releaser.require_auth();

        let mut job = Self::load_job(&env, job_id);

        if job.status != JobStatus::PendingRelease {
            panic!("job not pending release");
        }

        // Only requester can release
        if job.requester != releaser {
            panic!("not authorized to release");
        }

        let worker = job.worker.clone().expect("no worker");

        // Pay the worker
        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&env.current_contract_address(), &worker, &job.amount);

        job.status = JobStatus::Completed;
        Self::save_job(&env, job_id, job.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("escrow"),
                soroban_sdk::symbol_short!("released"),
            ),
            (job_id, worker, job.amount),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("job")),
            (job_id, job),
        );
    }

    /// Requester reclaims funds if deadline passed with no result
    pub fn refund(env: Env, requester: Address, job_id: u64) {
        requester.require_auth();

        let mut job = Self::load_job(&env, job_id);

        if job.requester != requester {
            panic!("not the job requester");
        }

        let refundable = job.status == JobStatus::Open
            || job.status == JobStatus::InProgress
            || job.status == JobStatus::PendingRelease
            || job.status == JobStatus::Disputed;

        if !refundable {
            panic!("job cannot be refunded");
        }

        if job.status == JobStatus::Disputed {
            let dispute_deadline = job
                .dispute_deadline_ledger
                .expect("missing dispute deadline");
            if env.ledger().sequence() <= dispute_deadline {
                panic!("dispute deadline not reached yet");
            }
        } else {
            if env.ledger().sequence() < job.deadline_ledger && job.status != JobStatus::Open {
                panic!("deadline not reached yet");
            }
        }

        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&env.current_contract_address(), &requester, &job.amount);

        job.status = JobStatus::Refunded;
        Self::save_job(&env, job_id, job.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("escrow"),
                soroban_sdk::symbol_short!("refunded"),
            ),
            (job_id, requester, job.amount),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("job")),
            (job_id, job),
        );
    }

    /// Requester raises a dispute — locks funds until arbiter resolves
    pub fn dispute(env: Env, requester: Address, job_id: u64) {
        requester.require_auth();

        let mut job = Self::load_job(&env, job_id);

        if job.requester != requester {
            panic!("not the job requester");
        }
        if job.arbiter.is_none() {
            panic!("no arbiter set for this job");
        }
        if job.status != JobStatus::PendingRelease {
            panic!("can only dispute pending release jobs");
        }

        job.status = JobStatus::Disputed;
        job.dispute_deadline_ledger = Some(env.ledger().sequence() + DISPUTE_TIMEOUT_LEDGERS);
        Self::save_job(&env, job_id, job.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("escrow"),
                soroban_sdk::symbol_short!("disputed"),
            ),
            (job_id, requester),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("job")),
            (job_id, job),
        );
    }

    /// Arbiter resolves a dispute
    pub fn resolve_dispute(env: Env, arbiter: Address, job_id: u64, favor_worker: bool) {
        Self::require_not_paused(&env);
        arbiter.require_auth();

        let mut job = Self::load_job(&env, job_id);

        if job.status != JobStatus::Disputed {
            panic!("job not disputed");
        }

        let job_arbiter = job.arbiter.as_ref().expect("no arbiter set");
        if *job_arbiter != arbiter {
            panic!("not the arbiter");
        }

        let token_client = token::Client::new(&env, &job.token);

        if favor_worker {
            let worker = job.worker.clone().expect("no worker");
            token_client.transfer(&env.current_contract_address(), &worker, &job.amount);
            job.status = JobStatus::Completed;
        } else {
            token_client.transfer(&env.current_contract_address(), &job.requester, &job.amount);
            job.status = JobStatus::Refunded;
        }

        Self::save_job(&env, job_id, job.clone());

        env.events().publish(
            (
                soroban_sdk::symbol_short!("escrow"),
                soroban_sdk::symbol_short!("resolved"),
            ),
            (job_id, arbiter, favor_worker),
        );
        env.events().publish(
            (symbol_short!("state"), symbol_short!("job")),
            (job_id, job),
        );
    }

    /// Wire this escrow contract up to a deployed CircuitBreaker contract.
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

    // ── Queries ──────────────────────────────────────────────────────────────

    pub fn get_job(env: Env, job_id: u64) -> Job {
        Self::load_job(&env, job_id)
    }

    pub fn job_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("count"))
            .unwrap_or(0)
    }

    // ── Internals ────────────────────────────────────────────────────────────

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

    fn load_job(env: &Env, job_id: u64) -> Job {
        let jobs: Map<u64, Job> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("jobs"))
            .unwrap_or(Map::new(env));
        jobs.get(job_id).expect("job not found")
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

    fn save_job(env: &Env, job_id: u64, job: Job) {
        let mut jobs: Map<u64, Job> = env
            .storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("jobs"))
            .unwrap_or(Map::new(env));
        jobs.set(job_id, job);
        env.storage()
            .instance()
            .set(&soroban_sdk::symbol_short!("jobs"), &jobs);
    }
}
