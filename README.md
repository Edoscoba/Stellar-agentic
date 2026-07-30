# StellarAgent 🤖⚡

> **AI Agent Payment Rails built on the Stellar blockchain.**  
> The fastest, cheapest way to give AI agents autonomous payment capabilities.

[![CI](https://github.com/Enniwealth/Stellar-agentic/actions/workflows/ci.yml/badge.svg)](https://github.com/Enniwealth/Stellar-agentic/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Stellar](https://img.shields.io/badge/Built%20on-Stellar-blue)](https://stellar.org)
[![Soroban](https://img.shields.io/badge/Smart%20Contracts-Soroban-purple)](https://soroban.stellar.org)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)
[![Discord](https://img.shields.io/badge/Discord-Join%20Community-5865F2)](https://discord.gg/stellaragent)

---

## Why StellarAgent?

AI agents need money. Today, giving an agent a wallet means dealing with:
- 🔴 Ethereum gas fees that cost more than the payment itself
- 🔴 Slow finality that breaks real-time workflows
- 🔴 No spend controls — agents can drain wallets
- 🔴 No audit trail for compliance

**StellarAgent fixes all of this.** Stellar's 2.5s finality and near-zero fees mean an agent can pay $0.001 per API call without the fee eating the payment. Our Soroban contracts add spend limits, escrow, and full audit trails on top.

---

## What's Inside

```
stellaragent/
├── contracts/        # Soroban smart contracts (Rust)
│   ├── agent_wallet_factory/
│   ├── payment_channel/
│   ├── escrow/
│   ├── rate_limiter/
│   ├── circuit_breaker/
│   ├── price_oracle/
│   └── amm_swap/
├── packages/
│   ├── core/         # @stellaragent/core — the TypeScript SDK
│   ├── react/        # @stellaragent/react — hooks
│   └── cli/          # @stellaragent/cli
├── python/           # stellaragent — the Python SDK
├── dashboard/        # React + Tailwind business dashboard
├── fixtures/         # Shared TS ↔ Python determinism fixtures
├── scripts/          # Deployment and fixture tooling
├── zk/               # Solvency-proof circuits (Rust)
└── docs/             # Documentation
```

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                   Your AI Agent                      │
└──────────────────────┬──────────────────────────────┘
                       │ stellaragent SDK
┌──────────────────────▼──────────────────────────────┐
│              SDK Layer (TypeScript/Python)            │
│  createAgentWallet() │ payForAPI() │ requestWork()   │
└──────────────────────┬──────────────────────────────┘
                       │ Soroban RPC
┌──────────────────────▼──────────────────────────────┐
│           Soroban Smart Contracts (Rust)              │
│  AgentWalletFactory │ PaymentChannel │ Escrow        │
│  RateLimiter        │ AuditLog                       │
└──────────────────────┬──────────────────────────────┘
                       │ Stellar Network
┌──────────────────────▼──────────────────────────────┐
│              Stellar Blockchain                       │
│         USDC · XLM · 2.5s finality · ~$0             │
└─────────────────────────────────────────────────────┘
```

---

## Quick Start

### SDK (TypeScript)

```bash
npm install @stellaragent/sdk
```

```typescript
import { StellarAgent } from '@stellaragent/sdk';

const agent = await StellarAgent.create({
  network: 'testnet',
});

await agent.createAgentWallet('research-agent');
await agent.openChannel({
  token: 'XLM',
  deposit: '10',
  limitPerPeriod: '5',
  period: 'hourly',
});

// Pay for an API call
await agent.payForAPI({
  endpoint: 'https://api.example.com/inference',
  recipient: 'G...API_PROVIDER_ADDRESS',
  amount: '0.001',
  asset: 'XLM',
});

// Agent-to-agent escrow job
const job = await agent.requestWork({
  workerAgent: 'G...AGENT_ADDRESS',
  task: 'Summarize this document',
  escrowAmount: '0.05',
  asset: 'XLM',
});
```

Contract IDs must be configured through `contracts` or the
`STELLARAGENT_<NETWORK>_*` environment variables described in the deployment
guide. Friendly non-XLM asset codes also need an `assetContracts` mapping to
their deployed Stellar Asset Contract ID.

#### Production: keep the key out of the agent process

Holding a raw secret in a long-lived process is a real risk once an agent has
funds. Pass a `signer` instead — the key stays behind a network boundary and
never enters this process:

```typescript
import { StellarAgent, RemoteSigner } from '@stellaragent/sdk';

const agent = await StellarAgent.create({
  network: 'mainnet',
  signer: new RemoteSigner({
    url: 'https://signer.internal',
    token: process.env.SIGNER_TOKEN,
    expectedPublicKey: process.env.AGENT_ADDRESS,
  }),
});

agent.holdsSecretKey; // false
```

See [docs/signing.md](docs/signing.md) for the protocol, the hardware-wallet
adapter, and why a signing service rather than Ledger.

### SDK (Python)

```bash
pip install stellaragent
```

```python
from stellaragent import StellarAgent, SpendLimit, PayForAPIParams

agent = StellarAgent.create(
    network="testnet",
    spend_limit=SpendLimit(amount="10", asset="USDC", period="hourly"),
)

agent.pay_for_api(
    PayForAPIParams(
        endpoint="https://api.example.com/inference",
        amount="0.001",
        asset="USDC",
    )
)
```

The Python SDK's deterministic math is a strict semantic port of the
TypeScript one — both are verified byte-identical against
[643 shared fixtures](fixtures/determinism.json) as a required CI check, so a
mixed TS/Python agent fleet cannot disagree about a bid score. See
[python/README.md](python/README.md).

### Contracts (Rust/Soroban)

There are seven contracts, and four of them need initializing in a specific
order before three others can be wired to them — so deploy them with the
script rather than by hand:

```bash
pnpm deploy:contracts --network testnet --source alice
```

It builds every WASM, deploys, initializes, cross-wires, and writes
`deployments/testnet.json` plus a matching `.env` block. Full runbook:
[docs/deployment.md](docs/deployment.md).

### Dashboard

```bash
cd dashboard
npm install
npm run dev
```

---

## Roadmap

- [x] Project scaffolding & architecture
- [ ] `AgentWalletFactory` Soroban contract
- [ ] `PaymentChannel` Soroban contract
- [ ] `Escrow` Soroban contract
- [ ] `RateLimiter` Soroban contract
- [ ] TypeScript SDK core
- [ ] Python SDK
- [ ] Business dashboard (React + Tailwind)
- [ ] Stellar Community Fund grant application
- [ ] Mainnet deployment

---

## Contributing

We welcome contributors! See [CONTRIBUTING.md](CONTRIBUTING.md) for how to get started.

Good first issues are labeled [`good first issue`](https://github.com/yourusername/stellaragent/labels/good%20first%20issue).

---

## License

MIT © StellarAgent Contributors
