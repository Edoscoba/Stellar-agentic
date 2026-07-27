# [SDK] Test suites + CI, contract deployment, Signer abstraction, Python SDK

Closes four issues: the zero-test gap, the hardcoded placeholder contract
addresses, in-memory secret keys, and the missing Python SDK.

They are one PR because they are not separable. The deployment work needs the
test infrastructure to be verifiable; the Signer refactor touches the same
constructor the deployment fast-fail check added; and the Python SDK's whole
correctness argument rests on a fixture suite that runs under the vitest config
introduced by the test work. Four independent branches would have conflicted in
`packages/core/src/index.ts` and `.github/workflows/ci.yml`.

Reviewable commit-by-commit — each commit is one issue, in dependency order.

---

## Problem

Four separate gaps, plus several things that were already broken:

- **No tests.** `packages/core` ran `vitest run --passWithNoTests` against zero
  test files, despite `math/fixed-point.ts` and `math/bid.ts` being the
  correctness-critical core of the SDK. `packages/cli` had no `test` script;
  `dashboard`'s was `echo "No tests yet…" && exit 0`.
- **Fake contract addresses.** `DEFAULT_CONTRACTS` shipped
  `CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA` for testnet and
  empty strings for mainnet/local. Every call would have failed deep inside an
  RPC round-trip.
- **Raw secrets in a long-lived process.** `StellarAgent` was built entirely
  around `Keypair.fromSecret(config.secretKey)` and exposed `get secretKey()`.
- **No Python SDK**, despite the roadmap listing one — and porting the
  deterministic math naively would have reintroduced the exact cross-platform
  divergence `fixed-point.ts` exists to prevent.

---

## Summary

### Tests and CI

- **1069 TypeScript tests** and **891 Python tests**, replacing zero.
  `packages/core` alone: 96 for `fixed-point.ts`, 72 for `bid.ts`, 694
  fixture-driven, 60 for `StellarAgent`, 50 for `Signer`, 40 for contract
  resolution, 32 for signer↔agent integration.
- **Playwright** for the dashboard (closes the long-referenced issue #63) —
  21 tests covering all six routes, run against a production `vite preview`
  build so CI exercises the bundle that ships.
- **Coverage gate** on `packages/core/src/math`, pinned at 100%
  lines/branches/functions/statements in its own CI job.
- `packages/cli` gains a `test` script and packaging smoke tests.

### Contract deployment

- **`scripts/deploy.ts`** builds all **seven** contract WASMs (not four — see
  below), deploys them, runs the four `initialize` entrypoints in the required
  order, cross-wires the references, and writes `deployments/<network>.json`
  plus a matching `.env` block. `--dry-run` prints the whole sequence without
  touching a network.
- **`packages/core/src/contracts.ts`** resolves addresses from
  `config.contracts` → `STELLARAGENT_<NETWORK>_<CONTRACT>` →
  `STELLARAGENT_<CONTRACT>` → a per-network sentinel. The old placeholders are
  kept but renamed `UNCONFIGURED_CONTRACTS` — labelled as what they are rather
  than posing as deployment output.
- **`StellarAgent.create()` fails fast** with `ContractsNotDeployedError`,
  naming every unconfigured contract, the network, the deploy command, and the
  exact env vars to set.
- **`docs/deployment.md`** — full runbook, ordering constraints, troubleshooting,
  and the by-hand sequence for when the script can't be used.
- **`.github/workflows/deploy-testnet.yml`** — manual dispatch only (rationale
  below).

### Signer abstraction

- **`Signer`** interface — `getPublicKey` / `signTransaction` / `signAuthEntry`,
  base64 XDR both ways. Key material never crosses it.
- **`KeypairSigner`** preserves the existing behaviour; the secret moves to a
  `#private` field so it no longer appears in `JSON.stringify`, `Object.keys`,
  or an error reporter walking enumerable properties.
- **`RemoteSigner`** — HTTP signing service with a bearer token, per-request
  timeout, and an optional `expectedPublicKey` pin.
- **`SignerAdapter`** wraps anything already speaking SEP-43.
- `create({ signer })` is new; `create({ secretKey })` is unchanged.
  **Fully backward compatible.**

### Python SDK

- **`python/`** — `stellaragent`, with `fixed_point.py` and `bid.py` as strict
  semantic ports, plus `contracts.py`, `types.py`, and `agent.py`.
- **`fixtures/determinism.json`** — **643 cases** generated from the TypeScript
  implementation, consumed by *both* test suites, asserting byte-identical
  string output.
- Packaging metadata, a README mirroring the TS Quick Start, and a CI matrix
  across Python 3.10–3.13.

---

## Design decisions worth reviewing

### Why a signing service and not Ledger

The issue offered either a Ledger integration or a remote-signing protocol.

**A Ledger requires a physical button press per signature.** That is excellent
for a human treasury and fatal here: the premise of this SDK is an autonomous
agent paying $0.001 per API call with no human in the loop. A hardware wallet
cannot serve an unattended process at that cadence — the first payment would
block forever.

A signing service fits the actual threat model ("the agent process is
compromised, or its memory is dumped"): the agent holds a URL and a token,
policy lives at the boundary where it is enforced even when the caller is
compromised, and rotation is a token swap rather than migrating funds off every
account. Hardware signing remains right for admin keys — that is what
`SignerAdapter` is for. Full reasoning in `docs/signing.md`.

The protocol returns **signed XDR rather than a raw signature** deliberately: a
service that signed an opaque hash could not inspect what it signs, and
inspection (spend ceilings, destination allow-lists, audit log) is most of the
value of moving the key behind a boundary.

### Seven contracts, not four

The deployment issue says "all four contract WASMs". There are seven crates.
`price_oracle` and `amm_swap` have no `ContractAddresses` field because the SDK
never calls them directly — but `payment_channel.pay_with_conversion` does, so
they must still be deployed and initialized. That is precisely the step a
hand-written runbook misses.

Ordering is load-bearing: `agent_wallet_factory.initialize(admin)` must land
before anything references the factory, and `circuit_breaker` must hold its
trusted-node set before `payment_channel`/`escrow` are pointed at it —
otherwise those two are wired to a breaker that can never reach quorum.

### Env vars rather than the SDK reading `deployments/*.json`

`@stellaragent/core` is bundled for browsers (the dashboard imports it), so it
cannot `fs.readFile` at runtime. Env vars are the portable channel; callers who
want the file import the JSON and pass it as `config.contracts`, which outranks
everything.

### Address validation is a checksum check, not a pattern match

`isDeployedAddress` uses strkey validation rather than matching the known
placeholders, so it also catches truncated addresses, addresses pasted from the
wrong network, and single-character typos — all of which otherwise surface as
the same opaque RPC failure.

### `allowUnconfiguredContracts`

`getBalance()` touches no contract. Making it impossible on an undeployed
network would have been a regression, so there is an explicit opt-out. Contract
calls on such an agent still fail.

### Testnet redeploys are manual dispatch, not push-to-main

Every deploy mints new contract IDs and orphans all existing testnet state.
Running that on each merge would silently break anything pointed at the previous
deployment.

### Matching `bignumber.js` from Python

Three things make a naive `Decimal` translation wrong. All three are handled and
documented in `fixed_point.py`:

| `bignumber.js` | Python `Decimal` | Resolution |
|---|---|---|
| `DECIMAL_PLACES: 18` applies to **division only**; `+ - *` are exact and unbounded | `Context.prec` applies to **every** operation | Large working precision keeps add/sub/mul exact; only division is quantized |
| `DECIMAL_PLACES` counts **decimal places** | `prec` counts **significant digits** | Explicit `quantize(Decimal('1e-18'))` rather than a precision setting |
| `BigNumber('-0')` normalises to `0`, but truncating `-0.5` gives `'-0'` | `Decimal('-0')` keeps its sign | `bn()` normalises zero at construction; `quantize` handles the second case |

A dedicated `Context` object is used rather than `getcontext()`, so a caller
changing the thread-local decimal context elsewhere in the process cannot
silently alter results — that would be the same class of non-determinism the
module exists to prevent.

**One intentional API divergence:** Python's `bn()` rejects `float`. The TS
signature accepts `number`, but `Decimal(0.1)` is
`0.1000000000000000055511151231257827`, so accepting one would reintroduce
divergence exactly where this module prevents it.

### `python/`, not `packages/python/`

`packages/*` is a pnpm workspace glob — anything there is treated as a Node
package by pnpm and Turborepo, needing a `package.json` it has no use for. A
top-level `python/` keeps the toolchains separate, with `fixtures/` shared.

---

## Pre-existing bugs found and fixed

None of these were in the issues; all of them blocked the work.

1. **`pnpm test` at the repo root had never worked.** `turbo.json` used the
   1.x `pipeline` key under the declared turbo `^2.0.0`. This is the literal
   acceptance criterion of the testing issue.
2. **CI's SDK job was a no-op.** It ran in `working-directory: sdk` (does not
   exist) and filtered `@stellaragent/sdk` (not the package name — it is
   `@stellaragent/core`). Every step was a silent no-op or an error.
3. **`StellarAgent.create({ network: 'local' })` threw.** `Horizon.Server`
   rejects `http://` without `allowHttp`, and the `local` config is
   `http://localhost:8000`. The SDK was unusable on the local network — exactly
   what the integration tests and deploy script need. Now exempted for loopback
   hosts only, so a plaintext non-local endpoint still fails loudly.
4. **`packages/cli` could not build.** No `@types/node`, so `console` was
   undefined.
5. **Contracts built to undeployable WASM.** `cargo build --target
   wasm32-unknown-unknown` under Rust ≥1.82 emits the post-MVP `reference-types`
   feature, which soroban-sdk 22's VM rejects at upload:

   ```
   HostError: Error(WasmVm, InvalidAction)
   "reference-types not enabled: zero byte expected"
   ```

   **`cargo build` succeeds** — the artifact is simply undeployable, so nothing
   fails until an upload is simulated. CI built that target and went green while
   producing wasm that could never reach a network. Fixed by targeting
   `wasm32v1-none`.

   Two things made this easy to miss: `agent_wallet_factory` uploaded *fine* and
   only `payment_channel` failed, so a single-contract smoke test would also
   have passed. And the commonly-cited
   `RUSTFLAGS="-C target-feature=-reference-types"` workaround **does not work**
   — byte-identical output, because the feature comes from the precompiled
   `core`/`std` for that target, not from the crate's own codegen.

---

## Testing

### Verified against a live network, not just mocked

The deployment path was run end-to-end against `stellar/quickstart` on a local
standalone network:

- all seven contracts deployed, initialized, and cross-wired
- `factory.admin()` returns the deployer and `circuit_breaker.is_paused()`
  returns `false` — proving the `initialize` calls actually landed on-chain
- the generated `deployments/local.json` is accepted by `assertDeployed()` and
  `StellarAgent.create()` in **both** SDKs, via explicit config *and* via the
  printed `STELLARAGENT_LOCAL_*` env block
- `getBalance()` / `get_balance()` succeed against the live local Horizon

### The tests were verified to actually fail

Green suites prove nothing on their own, so each guard was checked against a
deliberate break:

| Guard | Sabotage | Result |
|---|---|---|
| Coverage gate | delete `bid.test.ts` | exits **1** (0 when met) |
| Determinism suite | `ROUND_HALF_UP` instead of `ROUND_DOWN` | **140 failures** |
| Determinism suite | `Decimal` precision 18 (the significant-digits trap) | **248 failures** |
| Fixture staleness | tamper with one expected value | all **3** CI steps fail |
| Secret containment | six planted leaks (field, array, `Map`, class instance, cycle, throwing getter) | walker detects each |

### Cross-language determinism

`fixtures/determinism.json` holds 643 cases — 388 fixed-point, 210 `scoreBid`,
24 `rankBids`, 16 spend-limit, 5 invalid-weight — generated from the TypeScript
implementation, which is the reference (it is what on-chain bid scores were
computed against). Both suites assert against that same file, by **string
equality**; `pytest.approx` would defeat the entire point.

`pnpm fixtures:check` fails if the committed file drifts from the TS
implementation — otherwise a TS change could silently make the fixtures wrong
and both suites would keep agreeing with each other about stale behaviour.

### CI

New jobs: `SDK (Python 3.10–3.13)`, `Determinism (TS ↔ Python)`, and
`Coverage gate (core/math)`. The determinism job runs the staleness check and
both language suites **in one job deliberately** — split across two, a branch
protection rule could be satisfied by one passing while the other was never
required.

---

## Out of scope / follow-ups

Called out honestly rather than papered over. All of these depend on the
companion **"implement real Soroban invocation"** issue, which is not in this
PR — `openChannel`, `payForAPI`, `requestWork` and the query methods are still
stubs that throw.

- **Local-network lifecycle integration tests.**
  `packages/core/src/__tests__/integration.local.test.ts` is scaffolded and gated
  behind `STELLAR_LOCAL_INTEGRATION=1`. Its connectivity block runs; the nine
  lifecycle assertions are `it.todo` rather than false passes. They are the
  acceptance criteria of the invocation work.
- **The deployment issue's AC is half-closed.** "Produces a working config" is
  verified above. "…that the full lifecycle integration test can consume" cannot
  be, because that test does not exist yet.
- **The Signer issue's AC likewise.** `create({ signer })` works and never
  touches a secret, and parity tests drive every method on both a keypair-backed
  and a mock-remote-signer-backed agent asserting identical behaviour — but a
  "full payment lifecycle" cannot complete while `payForAPI` is a stub.
- **The Python SDK has no `Signer` equivalent yet.** `agent.secret_key` is
  marked deprecated in the docstring pointing at `docs/signing.md`. It should
  land before the package is used with real funds.
- **Python floor is 3.10, not 3.9.** mypy no longer supports targeting 3.9 and
  it could not be verified locally, so claiming support would have been
  untested.
- **`price_oracle` / `amm_swap` are deployed and initialized but not
  configured.** No prices or rates are set — `set_price` / `set_rate` are
  deployment-specific and left to the operator.

---

## Files changed

48 files, +20,783 / −111.

| Area | What |
|---|---|
| `packages/core/src/` | `contracts.ts`, `signer.ts` (new); `index.ts`, `types/index.ts` (modified) |
| `packages/core/src/**/__tests__/` | 7 new suites |
| `packages/core/vitest.config.ts` | coverage gate |
| `python/` | full package + 4 test modules |
| `fixtures/determinism.json` | 643 shared cases |
| `scripts/` | `deploy.ts`, `generate-fixtures.ts` |
| `dashboard/` | Playwright config, e2e specs, e2e tsconfig |
| `.github/workflows/` | `ci.yml` rebuilt; `deploy-testnet.yml` new |
| `docs/` | `deployment.md`, `signing.md` |
| root | `turbo.json`, `package.json`, `README.md`, `CONTRIBUTING.md`, `.gitignore` |
