# Deploying the StellarAgent contracts

There are **seven** Soroban contracts, four of which need a one-time
`initialize` call, and three of which hold addresses of the others that can
only be set once every contract exists. Deploying one contract with
`stellar contract deploy` — which is all `CONTRIBUTING.md` used to describe —
leaves a half-wired system whose failures surface much later as opaque RPC
errors from inside a payment.

Use the script. It does the whole sequence in the right order:

```bash
pnpm deploy:contracts --network local --source alice
```

---

## Contents

- [Prerequisites](#prerequisites)
- [Quick start](#quick-start)
- [What the script does](#what-the-script-does)
- [Wiring the SDK to a deployment](#wiring-the-sdk-to-a-deployment)
- [The fast-fail check](#the-fast-fail-check)
- [Manual runbook](#manual-runbook)
- [Redeploying](#redeploying)
- [Troubleshooting](#troubleshooting)

---

## Prerequisites

- [Rust](https://rustup.rs/) with the `wasm32-unknown-unknown` target
- [Stellar CLI](https://developers.stellar.org/docs/tools/stellar-cli) v21+
- Node.js 18+ and pnpm
- A funded identity on the target network

```bash
rustup target add wasm32-unknown-unknown
stellar keys generate alice --network testnet --fund
```

---

## Quick start

### Local standalone network

```bash
stellar network start local
stellar keys generate alice --network local --fund

pnpm deploy:contracts --network local --source alice
```

### Testnet

```bash
stellar keys generate alice --network testnet --fund

pnpm deploy:contracts \
  --network testnet \
  --source alice \
  --trusted-nodes G...NODE1,G...NODE2,G...NODE3,G...NODE4,G...NODE5
```

### Options

| Flag | Default | Meaning |
|------|---------|---------|
| `--network <name>` | `local` | Target network |
| `--source <identity>` | *(required)* | Stellar CLI identity, public key, or secret key |
| `--admin <G...>` | resolved from `--source` | Admin address for all four `initialize` calls |
| `--trusted-nodes <G,...>` | *(empty)* | Circuit-breaker trusted node set |
| `--propose-window <n>` | `720` | Circuit-breaker proposal validity window, in ledgers |
| `--out <path>` | `deployments/<network>.json` | Where to write the config |
| `--skip-build` | off | Reuse existing WASM artifacts |
| `--dry-run` | off | Print every command without running it |

`--dry-run` is the fastest way to see the exact command sequence without
touching a network.

> **On `--source`:** prefer a saved identity name. A secret key passed on the
> command line lands in your shell history.

---

## What the script does

### 1. Build

```bash
cargo build --target wasm32-unknown-unknown --release
```

### 2. Deploy all seven contracts

| Crate | SDK field |
|-------|-----------|
| `agent_wallet_factory` | `agentWalletFactory` |
| `payment_channel` | `paymentChannel` |
| `escrow` | `escrow` |
| `rate_limiter` | `rateLimiter` |
| `circuit_breaker` | `circuitBreaker` |
| `price_oracle` | — |
| `amm_swap` | — |

`price_oracle` and `amm_swap` have no `ContractAddresses` field because the
SDK never calls them directly — `payment_channel.pay_with_conversion` does.
They still have to be deployed and initialized, which is exactly the step a
single-contract runbook misses.

### 3. Initialize — order matters

```
agent_wallet_factory.initialize(admin)
circuit_breaker.initialize(admin, trusted_nodes, propose_window_ledgers)
price_oracle.initialize(admin)
amm_swap.initialize(admin)
```

`agent_wallet_factory.initialize(admin)` must land before anything references
the factory. The circuit breaker must hold its trusted-node set before
`payment_channel` and `escrow` are pointed at it — otherwise those two are
wired to a breaker that can never reach quorum.

`payment_channel`, `escrow` and `rate_limiter` have no `initialize`: the first
two take their admin from whoever first calls a `set_*` entrypoint, and
`rate_limiter` is configured per-agent via `set_limits`.

### 4. Cross-wire

```
payment_channel.set_circuit_breaker(admin, circuit_breaker)
payment_channel.set_price_oracle(admin, price_oracle)
payment_channel.set_amm(admin, amm_swap)
escrow.set_circuit_breaker(admin, circuit_breaker)
```

Skipping any of these leaves a contract that behaves as though the emergency
pause does not exist.

### 5. Write the config

`deployments/<network>.json`:

```json
{
  "network": "testnet",
  "admin": "G...",
  "deployedAt": "2026-07-27T18:00:00.000Z",
  "contracts": {
    "agentWalletFactory": "C...",
    "paymentChannel": "C...",
    "escrow": "C...",
    "rateLimiter": "C...",
    "circuitBreaker": "C..."
  },
  "crates": { "…": "including price_oracle and amm_swap" },
  "circuitBreaker": { "trustedNodes": ["G..."], "proposeWindowLedgers": 720 }
}
```

The script also prints a ready-to-paste `.env` block.

---

## Wiring the SDK to a deployment

Addresses resolve in this order, highest priority first:

1. **`config.contracts`** passed to `StellarAgent.create()`
2. **`STELLARAGENT_<NETWORK>_<CONTRACT>`** environment variable
3. **`STELLARAGENT_<CONTRACT>`** environment variable
4. the per-network *unconfigured sentinel* — which always fails the check

### Environment variables

```bash
export STELLARAGENT_TESTNET_AGENT_WALLET_FACTORY=C...
export STELLARAGENT_TESTNET_PAYMENT_CHANNEL=C...
export STELLARAGENT_TESTNET_ESCROW=C...
export STELLARAGENT_TESTNET_RATE_LIMITER=C...
export STELLARAGENT_TESTNET_CIRCUIT_BREAKER=C...
```

The network-scoped form lets one process talk to more than one network.

### Explicit config

```typescript
import deployment from '../deployments/testnet.json' with { type: 'json' };

const agent = await StellarAgent.create({
  network: 'testnet',
  contracts: deployment.contracts,
});
```

> **Why not have the SDK read `deployments/*.json` itself?**
> `@stellaragent/core` is bundled for browsers as well as Node — the dashboard
> imports it directly — so it cannot `fs.readFile` a path at runtime without
> breaking that build. Environment variables are the portable channel;
> importing the JSON yourself, as above, is the file-based equivalent.

---

## The fast-fail check

`StellarAgent.create()` validates every contract address before returning and
throws `ContractsNotDeployedError` if any is not a real deployed contract ID:

```
Contracts not deployed for network "testnet" — see docs/deployment.md

Unconfigured or invalid: agentWalletFactory, paymentChannel, escrow, rateLimiter, circuitBreaker

Fix this by either:
  1. Deploying them:  pnpm deploy:contracts --network testnet
     (writes deployments/testnet.json and prints an .env block)
  2. Passing known addresses explicitly:
       StellarAgent.create({ network, contracts: { paymentChannel: 'C...' } })
  3. Setting environment variables:
       STELLARAGENT_TESTNET_AGENT_WALLET_FACTORY=C...
       …
```

Validation is a strkey checksum check, not a pattern match against the known
placeholders, so it also catches truncated addresses, addresses pasted from
the wrong network, and single-character typos.

### Opting out

`getBalance()` is a Horizon query that touches no contract. To use an agent
for read-only work on a network with nothing deployed:

```typescript
const agent = await StellarAgent.create({
  network: 'testnet',
  allowUnconfiguredContracts: true,
});
await agent.getBalance();   // fine
await agent.openChannel(…); // still fails
```

---

## Manual runbook

If you cannot run the script — an air-gapped signer, a hardware wallet, a
network the CLI has no profile for — this is the same sequence by hand. Set
`SRC` and `ADMIN` first.

```bash
export SRC=alice
export NET=testnet
export ADMIN=$(stellar keys public-key $SRC)

cd contracts
cargo build --target wasm32-unknown-unknown --release
WASM=target/wasm32-unknown-unknown/release

# ── 1. Deploy all seven ────────────────────────────────────────────────────
deploy() { stellar contract deploy --wasm $WASM/$1.wasm --source-account $SRC --network $NET; }

FACTORY=$(deploy agent_wallet_factory)
CHANNEL=$(deploy payment_channel)
ESCROW=$(deploy escrow)
LIMITER=$(deploy rate_limiter)
BREAKER=$(deploy circuit_breaker)
ORACLE=$(deploy price_oracle)
AMM=$(deploy amm_swap)

# ── 2. Initialize, in this order ───────────────────────────────────────────
stellar contract invoke --id $FACTORY --source-account $SRC --network $NET \
  -- initialize --admin $ADMIN

stellar contract invoke --id $BREAKER --source-account $SRC --network $NET \
  -- initialize --admin $ADMIN \
     --trusted_nodes '["G...1","G...2","G...3","G...4","G...5"]' \
     --propose_window_ledgers 720

stellar contract invoke --id $ORACLE --source-account $SRC --network $NET \
  -- initialize --admin $ADMIN

stellar contract invoke --id $AMM --source-account $SRC --network $NET \
  -- initialize --admin $ADMIN

# ── 3. Cross-wire, only after all seven exist ──────────────────────────────
stellar contract invoke --id $CHANNEL --source-account $SRC --network $NET \
  -- set_circuit_breaker --admin $ADMIN --circuit_breaker $BREAKER
stellar contract invoke --id $CHANNEL --source-account $SRC --network $NET \
  -- set_price_oracle --admin $ADMIN --price_oracle $ORACLE
stellar contract invoke --id $CHANNEL --source-account $SRC --network $NET \
  -- set_amm --admin $ADMIN --amm $AMM
stellar contract invoke --id $ESCROW --source-account $SRC --network $NET \
  -- set_circuit_breaker --admin $ADMIN --circuit_breaker $BREAKER

# ── 4. Export for the SDK ──────────────────────────────────────────────────
cat <<EOF
STELLARAGENT_${NET^^}_AGENT_WALLET_FACTORY=$FACTORY
STELLARAGENT_${NET^^}_PAYMENT_CHANNEL=$CHANNEL
STELLARAGENT_${NET^^}_ESCROW=$ESCROW
STELLARAGENT_${NET^^}_RATE_LIMITER=$LIMITER
STELLARAGENT_${NET^^}_CIRCUIT_BREAKER=$BREAKER
EOF
```

> **Trusted nodes:** `contracts/circuit_breaker/src/lib.rs` sets `QUORUM = 5`.
> Initializing with fewer than five trusted nodes deploys a breaker that can
> never be triggered. The script warns about this; by hand, nothing will.

---

## Redeploying

`stellar contract deploy` always produces a **new** contract ID — there is no
in-place upgrade here. A redeploy means new addresses and empty state:
existing channels, jobs and rate limits do not carry over.

Re-running the script against contracts that are already initialized is safe:
`initialize` panics with `already initialized`, which the script reports and
skips rather than aborting mid-sequence.

### Testnet redeploys in CI

`.github/workflows/deploy-testnet.yml` runs the same script on manual
dispatch. It is deliberately **not** wired to every push to `main` — each run
mints new contract IDs and orphans all existing testnet state, which is not
something a merge should do silently. Trigger it from the Actions tab, then
update the `STELLARAGENT_TESTNET_*` repository secrets from its output.

---

## Troubleshooting

**`Contracts not deployed for network "…"`**
Nothing is configured. Deploy, or set the environment variables the error
lists.

**`already initialized`**
The contract was initialized by an earlier run. Expected on a redeploy; the
script skips it.

**`WASM not found: …`**
You passed `--skip-build` without having built. Drop the flag.

**`quorum not reached` when pausing**
Fewer than five distinct trusted nodes have called `propose_pause` inside the
validity window, or the trusted set has fewer than five members. Rotate it
with `circuit_breaker.set_trusted_nodes`.

**Calls fail after a successful deploy**
Cross-wiring was skipped. Re-run steps 3–4 of the manual runbook, or re-run
the script — the `set_*` entrypoints are idempotent for the same admin.
