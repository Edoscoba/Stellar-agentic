# StellarAgent 🤖⚡

> **AI Agent Payment Rails built on the Stellar blockchain.**  
> The fastest, cheapest way to give AI agents autonomous payment capabilities.

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
│   └── rate_limiter/
├── sdk/              # TypeScript SDK
│   └── src/
├── dashboard/        # React + Tailwind business dashboard
│   └── src/
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
  spendLimit: { amount: '10', asset: 'USDC', period: 'hourly' },
});

// Pay for an API call
await agent.payForAPI({
  endpoint: 'https://api.example.com/inference',
  amount: '0.001',
  asset: 'USDC',
});

// Agent-to-agent escrow job
const job = await agent.requestWork({
  workerAgent: 'G...AGENT_ADDRESS',
  task: 'Summarize this document',
  escrowAmount: '0.05',
  asset: 'USDC',
});
```

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
