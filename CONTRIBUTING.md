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
| `sdk/` | TypeScript | NPM SDK for developers |
| `dashboard/` | React + TypeScript + Tailwind | Business monitoring dashboard |
| `docs/` | Markdown | Documentation |

---

## Development Setup

### Prerequisites

- [Rust](https://rustup.rs/) + `wasm32-unknown-unknown` target
- [Stellar CLI](https://developers.stellar.org/docs/tools/stellar-cli)
- Node.js 18+
- Git

### Setup

```bash
# Clone the repo
git clone https://github.com/yourusername/stellaragent.git
cd stellaragent

# Install Rust wasm target
rustup target add wasm32-unknown-unknown

# Install SDK dependencies
cd sdk && npm install

# Install dashboard dependencies
cd ../dashboard && npm install

# Run testnet locally (optional)
stellar network start local
```

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

## Good First Issues

If you're new to the project, start here:

- **Contracts**: Write unit tests for the `RateLimiter` contract
- **SDK**: Add JSDoc comments to all exported functions
- **Dashboard**: Improve mobile responsiveness of the agent table
- **Docs**: Add a tutorial for deploying contracts to testnet

---

## Questions?

Open a [GitHub Discussion](https://github.com/yourusername/stellaragent/discussions) or join our [Discord](https://discord.gg/stellaragent).
