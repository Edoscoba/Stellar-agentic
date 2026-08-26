# [SDK] Rust SDK — a third first-class implementation

Closes the `sdk/rust` epic.

## Problem

Agent infrastructure written in Rust — and anything already embedding the
Soroban tooling in this repo — had to shell out to the TypeScript SDK or
re-implement the protocol. This removes that.

The second reason is the more interesting one. The determinism guarantee is
this project's headline claim, and it was proven across exactly **two**
implementations. Two implementations can agree by sharing an assumption
neither of them ever wrote down. A third, written from the spec and from
observed `bignumber.js` behaviour rather than by transliterating either
existing one, is the real test of whether `fixtures/determinism.json` actually
pins the semantics.

**It does.** All 643 fixtures — 388 fixed-point cases, 210 bid scores, 24
rankings, 16 spend-limit checks, plus the throwing and invalid-weight cases —
matched byte-for-byte on the first run, with no adjustment to the fixtures and
no change to the TypeScript or Python implementations.

## Summary

Adds `sdk/rust`, a publish-ready `stellaragent` crate: the Soroban RPC client,
the deterministic math modules, and fixture parity with TypeScript and Python
enforced by the same required CI job.

~6,500 lines of source and tests across six phase-separated commits, each of
which builds and tests green in isolation — verified by checking out every
commit in a scratch worktree, so `git bisect` stays useful.

## What landed, by phase

Commits are ordered so the risky part is first, as the issue asked.

| Commit | Phase | Contents |
| --- | --- | --- |
| `5910772` | 3 | Deterministic math: `decimal`, `fixed_point`, `bid`, `predict`, `ledger_time` |
| `49435ea` | 4 | Fixture parity suite; determinism CI job now covers three languages |
| `5902d52` | 1 | Client core: `rpc`, `scval`, `signer`, `contracts`, `error` |
| `148d322` | 2 | Operations: channels, payments, escrow, rate limits, queries |
| `9b46344` | 5 | Mocked-RPC suite and the local-network integration suite |
| `1d67919` | 6 | Crate metadata, `sdk-rust` CI job, release workflow, README |

---

## Phase 3 — deterministic math

`math::decimal` implements arbitrary-precision decimal arithmetic directly
rather than wrapping an existing crate. That was not a preference; three
specifics of `bignumber.js`'s contract are not reproduced by a general-purpose
decimal type, and each one is observable in the fixtures:

1. **`DECIMAL_PLACES` bounds division only.** `plus`/`minus`/`times` are exact
   and unbounded. A type whose precision is expressed in *significant digits*
   truncates
   `123456789012345678901234567890 / 987654321098765432109876543210`
   somewhere other than the 18th decimal place and diverges on the last digit.
   Here the truncation point is always a decimal place, and add/sub/mul never
   round at all.

2. **Rounding is `ROUND_DOWN` — toward zero, never half-even.** Rounding a
   spend calculation up by one stroop is the difference between a payment the
   chain accepts and one it rejects. `BigInt` division truncates toward zero,
   which is the same direction for negative operands as well as positive.

3. **Negative zero has two different behaviours, and both are load-bearing.**
   `bignumber.js` decides `toFixed`'s sign from the coefficient *before*
   rounding:

   ```js
   BigNumber('-0.00001').toFixed(4, ROUND_DOWN)                    // '-0.0000'
   BigNumber('-0.00001').decimalPlaces(4, ROUND_DOWN).toFixed(4)   // '0.0000'
   BigNumber('-0').toFixed(2)                                       // '0.00'
   ```

   `Decimal::to_fixed` takes the sign from the receiver; `Decimal::truncate`
   returns a value that has genuinely lost it. `bid.rs` uses the second form
   (`decimalPlaces(4).toFixed(4)`) and `fixed_point::fmt` uses the first, so
   collapsing them into one function would have produced a wrong digit in one
   of the two places.

**No `impl IntoDecimal for f64`, deliberately.** A float reaching a monetary or
score calculation reintroduces exactly the cross-platform divergence this
module exists to prevent — `0.1f64` is not `0.1`. The Python SDK enforces this
at runtime; here the type system refuses it, so it is a compile error rather
than a comment asking nicely. The single exception is `math::ledger_time`,
which returns `f64` because it produces wall-clock *estimates* for display
("resets in ~4 minutes") and never feeds a payment decision — documented
inline as such.

`math::predict` replicates the contracts' reset-then-check window semantics,
including the boundary that is easy to get backwards: every amount comparison
is strict `>` (a payment landing exactly on the limit is allowed) **except**
the hourly transaction-count check, which is `>=`. Each comparison cites its
source line in `contracts/rate_limiter/src/lib.rs`. It also faithfully
reproduces the quirk that `RateLimiter::check` never reads `active`, with the
reasoning written down — its contract is "agrees with `check`", not "agrees
with what `check` probably should do".

---

## Phase 4 — fixture parity

`sdk/rust/tests/determinism.rs` reads the identical
`fixtures/determinism.json` the other two suites read and asserts **string
equality**, never numeric closeness. A tolerance comparison would defeat the
entire point of the module under test: "close enough" is precisely what makes
x86 and ARM disagree about a bid score.

The `throws: true` cases matter as much as the value cases. Agreeing on
outputs while disagreeing about which *inputs* are legal is still a divergence
— one SDK would ship a payment the other refuses to build — so the suite
asserts Rust rejects exactly what TypeScript rejects.

The Rust half runs inside the existing `determinism` job rather than as a
fourth job, for the reason already documented there: split across separate
jobs, a branch-protection rule could be satisfied by one passing while another
was never required. The job is renamed `Determinism (TS ↔ Python ↔ Rust)`.

> **Note for whoever updates branch protection:** the required check's *name*
> changed. The rule needs re-pointing, or it will silently stop being enforced.

---

## Phase 1 — client core

**`scval`** covers every type the contracts use, and documents the two
conventions that catch people out:

- A `#[contracttype]` struct is *not* a vector of fields in declaration order
  — it is a map, and the host requires its keys sorted. `scval::map` sorts them
  rather than trusting the caller's insertion order, which is exactly the kind
  of thing that differs between two SDKs building the same argument.
- A fieldless enum variant is *not* a bare symbol — it is a one-element vector
  holding one. Passing a bare symbol is the most common route to an
  `UnexpectedType` from a contract that expected an enum, so
  `enum_variant("Hourly")` exists separately from `symbol("Hourly")`.

**`rpc`** reads account state from `getLedgerEntries` rather than Horizon, so
the sequence number and the ledger it is valid against come from the same
server the transaction will be submitted to (the two can disagree during a
sync, and requiring Horizon would stop an agent running against a bare RPC
deployment). It refuses plaintext HTTP outside loopback — **including LAN
addresses** — so a misconfigured endpoint fails loudly instead of transmitting
signed envelopes in the clear.

**`signer`** ports the existing TypeScript abstraction: base64 XDR in, signed
base64 XDR out, deliberately SEP-43-shaped so a wallet or HSM can be adapted
with a thin wrapper. `sign_auth_entry` is not decoration — Soroban
authorisation entries are signed separately from the envelope carrying them, so
a signer implementing only `sign_transaction` cannot authorise a contract
invocation at all. The classic-account signature is a vector of
`{public_key, signature}` maps rather than the 64 raw bytes one would guess,
because a Stellar account can have several signers and the contract has to know
which one signed.

`RemoteSigner` rather than a hardware backend, with the reasoning in the module
docs: a button press per signature is right for admin keys and fatal for an
agent paying a fraction of a cent per inference call with no human in the loop.

**`error`** maps contract panic text onto typed codes in one place. Soroban only
surfaces prose, so the pattern matching is unavoidable — but it can be
concentrated, and it uses the same patterns as `packages/core/src/index.ts` so
all three SDKs classify a given panic identically.

---

## Phase 2 — operations

One pipeline for every contract call: build → simulate → sign authorisations →
assemble → sign envelope → submit → poll. Read-only calls stop after
simulation, which is why `channel()` and friends work against an agent that has
never submitted anything and holds no funds.

Two steps are load-bearing and easy to omit:

- The simulated footprint and resource fee must be attached to the real
  transaction. Skipping it is the classic route to `txSorobanInvalid` — the
  network will not run an invocation whose footprint it was not told about.
- Auth entries with `Address` credentials are signed separately, bound to a
  nonce, an expiry ledger and one specific invocation tree, so a leaked signed
  entry is not indefinitely replayable.

Transactions carry time bounds rather than `Preconditions::None`: an envelope
with no upper bound stays submittable forever, so one that never made it into a
ledger could be replayed much later.

`XLM` resolves to the native Stellar Asset Contract by **deriving** the ID from
the network passphrase rather than looking up a constant, so it is correct on
any network including a standalone one, with no configuration. A test pins the
derivation against the published testnet SAC ID
(`CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC`), so a rewrite of
the preimage is caught rather than a change in taste.

### Two deliberate divergences from the TypeScript SDK

Both flagged here because they are API differences a reviewer should agree
with, not accidents:

1. **Friendbot funding is opt-in** (`fund_new_testnet_accounts`) and only ever
   applies to a freshly generated keypair. The TypeScript SDK funds implicitly.
   A network side effect that happens without being asked for is a surprise in
   a library.
2. **`create()` is a builder.** A ten-argument constructor where eight
   arguments are optional reads badly in Rust, and the builder lets
   "signer XOR secret_key" be a build-time error rather than a documented
   convention.

---

## Phase 5 — tests

**`tests/mock_rpc.rs`** runs the whole invocation pipeline over real HTTP
against a hand-rolled JSON-RPC stub, rather than mocking out the client's
internals. That distinction is the point: a test that stubs `invoke` proves
nothing about whether the envelope this SDK produces is one a server would
accept, and the pipeline is where the interesting mistakes live — a missing
footprint, an unsigned auth entry, a sequence number off by one.

It covers a read-only call submitting nothing, a mutation polling through
`NOT_FOUND` to `SUCCESS`, `DUPLICATE` treated as accepted, a spend-limit panic
classified onto `SPEND_LIMIT_EXCEEDED`, a rejected submission carrying its
hash, an archived-entry simulation, a struct missing a field naming that field,
and an unknown enum variant reported rather than guessed at.

The stub is ~130 lines of `tokio::net` rather than a mocking crate. Adding a
dependency to make it marginally shorter is not a good trade in a crate whose
value proposition includes an auditable dependency set.

**`tests/integration_local.rs`** is the definition-of-done suite: the full
payment lifecycle against a real local network, plus a test asserting the
off-chain predictor agrees with the chain about a limit breach. Gated twice —
the `integration` feature *and* `#[ignore]` — and it **fails loudly rather than
skipping** when its environment is only half-configured. A skipped integration
test is indistinguishable from a passing one in a CI summary, which is worse
than not running it at all.

Doc tests on every public item; `cargo doc` runs with `-D warnings` in CI, so a
broken intra-doc link fails the build.

---

## Phase 6 — packaging and docs

- Publish-ready crate metadata, `README.md` with the same worked example the
  other two SDKs use.
- New `sdk-rust` CI job: `fmt`, `clippy --all-targets --all-features -D
  warnings`, `test --all-features`, `doc -D warnings`, `package --no-verify`.
  Separate from the `contracts` job on purpose — that one builds for
  `wasm32v1-none` against `soroban-sdk` and resolves from
  `contracts/Cargo.lock`, while this builds for the host and needs `std`.
  Sharing a job would mean sharing a dependency resolution, which is exactly
  what that lockfile exists to pin away.
- `release-rust-sdk.yml`, triggered by a `sdk-rust-v*` tag rather than a push
  to main, so cutting a release is deliberate; the prefix is namespaced because
  this repo also ships npm packages and a Python distribution. It verifies the
  tag matches the manifest version before doing anything, and re-runs the
  determinism fixtures even though CI already ran them on the same commit —
  crates.io publishes are irreversible, and a version can be yanked but never
  replaced.
- `Cargo.lock` committed, with the same reasoning as `contracts/Cargo.lock`: CI
  should test one pinned dependency set rather than whatever crates.io resolved
  to this morning. A dependent's own resolution is unaffected — cargo ignores a
  library's lockfile when it is used as a dependency.

---

## Verification

Run locally, all green:

```
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features        # 121 unit + 9 determinism + 18 mocked-RPC + 42 doc
RUSTDOCFLAGS=-D warnings cargo doc --no-deps --all-features
cargo package --no-verify
```

Each of the six commits was checked out in a scratch worktree and built
independently — no bisect point is broken.

---

## Definition of done

| Criterion | Status |
| --- | --- |
| Agrees byte-for-byte with TS and Python on every determinism fixture | ✅ all 643, first run |
| The determinism CI job covers three languages | ✅ `Determinism (TS ↔ Python ↔ Rust)` |
| The Rust SDK performs the full payment lifecycle against a local network | ⚠️ see below |

The lifecycle suite is written and compiles
(`tests/integration_local.rs::the_full_payment_lifecycle_runs_end_to_end`), and
the pipeline it drives is covered end-to-end over HTTP by `tests/mock_rpc.rs`.
It has **not** been executed against a live standalone network from this branch
— that needs a running quickstart container and the five contracts deployed,
which this environment does not have. Someone with a local network should run:

```bash
export STELLARAGENT_LOCAL_SECRET=S...
export STELLARAGENT_LOCAL_PAYMENT_CHANNEL=C...   # and the other four
cargo test --features integration -- --ignored --test-threads=1
```

Flagging it rather than quietly claiming the box: the suite is the check, and
the check has not been run.

---

## Notes for review

- **Branch protection needs re-pointing** at the renamed determinism check.
- **The design was not agreed in the issue thread first**, as the issue asked.
  If any of it is the wrong shape, the phase-separated commits mean a rewrite
  can be scoped to one of them rather than to all 6,500 lines. The two API
  divergences from the TypeScript SDK (opt-in funding, builder over
  constructor) are the most likely candidates for disagreement.
- **Attestations are not ported.** `packages/core/src/math/attestation.ts` has
  no Rust counterpart yet: it is not in the phase list, it has no fixtures, and
  matching `JSON.stringify`'s exact canonicalisation byte-for-byte is its own
  determinism problem that deserves its own fixtures rather than being
  smuggled in here.
- **Two pre-existing TS ↔ Python divergences were found** while deriving the
  semantics, both in cases the fixtures do not cover:
  `mul("-1", "0")` and `div("-1", "1e30")` give `0.000…` in TypeScript and
  `-0.000…` in Python. The Rust port follows TypeScript, since the fixtures are
  generated from it. Worth a follow-up issue to add fixtures for the
  sign-of-zero cases and reconcile Python — not fixed here, because changing
  the Python SDK is outside this epic's scope.
