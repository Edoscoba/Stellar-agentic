# Contributing to StellarAgent

Thank you for your interest in contributing! StellarAgent is an open-source project and we welcome contributions of all kinds — bug fixes, new features, documentation, tests, and more.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Project Structure](#project-structure)
- [Development Setup](#development-setup)
- [How to Contribute](#how-to-contribute)
- [Commit Convention](#commit-convention)
- [Pull Request Process](#pull-request-process)
- [Good First Issues](#good-first-issues)

---

## Code of Conduct

Be respectful. Be constructive. We're all here to build something great together.

---

## Project Structure

| Directory | Language | What it is |
|-----------|----------|------------|
| `contracts/` | Rust | Soroban smart contracts on Stellar |
| `packages/core/` | TypeScript | `@stellaragent/core` — the SDK developers install |
| `packages/react/` | TypeScript | `@stellaragent/react` — React hooks over the SDK |
| `packages/cli/` | TypeScript | `@stellaragent/cli` — the `stellaragent` command |
| `dashboard/` | React + TypeScript + Tailwind | Business monitoring dashboard |
| `zk/` | Rust | Solvency-proof circuits |
| `docs/` | Markdown | Documentation |

The TypeScript packages are a pnpm workspace driven by Turborepo — run
commands from the repo root, not from inside a package.

---

## Development Setup

### Prerequisites

- [Rust](https://rustup.rs/) 1.84+ with the `wasm32v1-none` target
- [Stellar CLI](https://developers.stellar.org/docs/tools/stellar-cli)
- Node.js 18+
- Git

### Setup

```bash
# Clone the repo
git clone https://github.com/yourusername/stellaragent.git
cd stellaragent

# Install Rust wasm target
rustup target add wasm32v1-none

# Install every workspace package in one shot (pnpm, from the repo root)
pnpm install

# Run testnet locally (optional)
stellar network start local
```

---

## Testing

All TypeScript tests run from the repo root through Turborepo:

```bash
pnpm test          # every package: core, react, cli, dashboard e2e
pnpm typecheck     # tsc --noEmit across the workspace
pnpm lint          # eslint across the workspace
```

To run one package's suite:

```bash
pnpm --filter @stellaragent/core test
pnpm --filter @stellaragent/core test:watch
```

### Coverage gate on `packages/core/src/math`

`packages/core/src/math` is the correctness-critical part of the SDK — every
agent bid score and spend-limit check flows through it, and its whole reason
for existing is that native floats round differently on x86 and ARM. A
regression there is a silent cross-platform determinism break, not a crash,
so it is gated at **100% line, branch, function and statement coverage**:

```bash
pnpm --filter @stellaragent/core test:coverage
```

CI fails if coverage drops below that. Thresholds live in
[`packages/core/vitest.config.ts`](packages/core/vitest.config.ts). If you add
a helper to `math/`, add tests for it in the same PR.

### Dashboard e2e (Playwright)

```bash
cd dashboard
pnpm exec playwright install chromium   # one-time browser download
pnpm test                               # builds, serves, and runs the specs
pnpm test:ui                            # interactive runner
```

The specs live in [`dashboard/e2e/`](dashboard/e2e/) and run against a
production `vite preview` build, so CI exercises the same bundle that ships.

### Local-network integration tests

`packages/core/src/__tests__/integration.local.test.ts` runs against a Soroban
standalone network and is **skipped by default**. To run it you need a local
network and the contracts deployed:

```bash
stellar network start local
pnpm deploy:contracts --network local --source alice
# export the STELLARAGENT_LOCAL_* values printed by the deploy command
STELLAR_LOCAL_INTEGRATION=1 pnpm --filter @stellaragent/core test
```

The suite funds isolated owner/worker accounts through local friendbot and
exercises agent registration, the complete payment-channel lifecycle,
rate-limit queries, and the complete escrow lifecycle. It uses the native XLM
asset contract, so no custom token deployment is required.

### Rust contracts

```bash
cd contracts
cargo test --all
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

### Python SDK

```bash
cd python
python -m venv .venv && source .venv/bin/activate
pip install -e ".[dev]"

pytest              # includes the cross-language determinism suite
ruff check .
mypy
```

### Cross-language determinism (TS ↔ Python)

`packages/core/src/math` and `python/src/stellaragent` must produce
**byte-identical** output. [`fixtures/determinism.json`](fixtures/determinism.json)
holds 643 cases generated from the TypeScript implementation, and both test
suites assert against that same file:

```bash
pnpm fixtures:generate   # regenerate from packages/core (the reference)
pnpm fixtures:check      # fail if the committed file is stale
```

If you change either math implementation:

1. Make the change.
2. Run `pnpm fixtures:check`. If it fails, the numeric contract changed.
3. If that was intended, run `pnpm fixtures:generate` and **review the diff** —
   it shows exactly which values moved.
4. Run both suites and make the other language match.

The `Determinism (TS ↔ Python)` CI job runs all three steps and is a required
check. Comparison is string equality, never numeric closeness — "close enough"
is precisely what makes two machines disagree about a bid score.

---

## How to Contribute

1. **Find an issue** — Look for [`good first issue`](https://github.com/yourusername/stellaragent/labels/good%20first%20issue) or [`help wanted`](https://github.com/yourusername/stellaragent/labels/help%20wanted) labels.
2. **Comment on the issue** — Let us know you're working on it so we don't duplicate effort.
3. **Fork & branch** — Fork the repo and create a branch: `git checkout -b feat/your-feature-name`
4. **Build & test** — Make sure tests pass before submitting.
5. **Submit a PR** — Fill out the PR template and link the issue.

---

## Commit Convention

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(sdk): add payForAPI method
fix(contracts): correct rate limiter overflow
docs: update quick start guide
test(contracts): add escrow release tests
chore: update dependencies
```

Types: `feat`, `fix`, `docs`, `test`, `chore`, `refactor`, `perf`

---

## Pull Request Process

1. Ensure your branch is up to date with `main`
2. All CI checks must pass (build, lint, tests)
3. At least one maintainer review required
4. Squash commits before merge (maintainer will do this)

---

## Deploying contracts

There are seven Soroban contracts, four need a one-time `initialize`, and
three hold addresses of the others that can only be set once all seven exist.
Do not deploy them by hand — use the script:

```bash
pnpm deploy:contracts --network local --source alice
pnpm deploy:contracts --network local --source alice --dry-run   # preview only
```

It builds every WASM, deploys all seven, initializes them in the required
order, cross-wires the references, and writes `deployments/<network>.json`
plus a matching `.env` block.

Point the SDK at the result with the printed `STELLARAGENT_<NETWORK>_*`
environment variables, or by passing `contracts:` to `StellarAgent.create()`.
An agent created against undeployed contracts throws
`ContractsNotDeployedError` immediately rather than failing later inside an
RPC call.

Full runbook — including the by-hand sequence and the initialization ordering
constraints — is in **[docs/deployment.md](docs/deployment.md)**.

---

## Good First Issues

If you're new to the project, start here:

- **Contracts**: Write unit tests for the `RateLimiter` contract
- **SDK**: Add JSDoc comments to all exported functions
- **Dashboard**: Improve mobile responsiveness of the agent table
- **Docs**: Add a tutorial for deploying contracts to testnet

---

## Questions?

Open a [GitHub Discussion](https://github.com/yourusername/stellaragent/discussions) or join our [Discord](https://discord.gg/stellaragent).
