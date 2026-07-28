# StellarAgent — Python SDK

> **AI Agent Payment Rails on Stellar.**
> The Python counterpart of [`@stellaragent/core`](../packages/core).

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Python](https://img.shields.io/badge/python-3.9%2B-blue)](https://www.python.org)

---

## Install

```bash
pip install stellaragent
```

From a checkout:

```bash
cd python
pip install -e ".[dev]"
```

---

## Quick Start

Mirrors the [TypeScript Quick Start](../README.md#quick-start).

```python
from stellaragent import StellarAgent, SpendLimit, PayForAPIParams, RequestWorkParams

agent = StellarAgent.create(
    network="testnet",
    spend_limit=SpendLimit(amount="10", asset="USDC", period="hourly"),
)

# Pay for an API call
agent.pay_for_api(
    PayForAPIParams(
        endpoint="https://api.example.com/inference",
        amount="0.001",
        asset="USDC",
    )
)

# Agent-to-agent escrow job
job = agent.request_work(
    RequestWorkParams(
        worker_agent="G...AGENT_ADDRESS",
        task="Summarize this document",
        escrow_amount="0.05",
        asset="USDC",
    )
)
```

> **Note.** The contract-invoking methods (`pay_for_api`, `request_work`,
> `open_channel`, …) raise `NotImplementedError` today — exactly as their
> TypeScript counterparts do. They are pending the companion "real Soroban
> invocation" work. What is complete and load-bearing right now is the
> deterministic math below.

### Pointing at deployed contracts

Same environment variables as the TypeScript SDK, so one deployment
configures both:

```bash
export STELLARAGENT_TESTNET_AGENT_WALLET_FACTORY=C...
export STELLARAGENT_TESTNET_PAYMENT_CHANNEL=C...
export STELLARAGENT_TESTNET_ESCROW=C...
export STELLARAGENT_TESTNET_RATE_LIMITER=C...
export STELLARAGENT_TESTNET_CIRCUIT_BREAKER=C...
```

Or explicitly:

```python
agent = StellarAgent.create(
    network="testnet",
    contracts={"payment_channel": "C..."},
)
```

Creating an agent against undeployed contracts raises
`ContractsNotDeployedError` immediately rather than failing later inside an
RPC call. Pass `allow_unconfigured_contracts=True` for read-only use such as
`get_balance()`. See [docs/deployment.md](../docs/deployment.md).

---

## Deterministic math

This is the reason the package exists in the form it does.

`packages/core/src/math/fixed-point.ts` was written because IEEE-754 doubles
round differently on x86 (SSE2) and ARM (NEON), so the same bid-score
expression could produce different results on different machines and break
hash agreement on-chain. A Python port that reached for `float` — or for
`Decimal` with mismatched context settings — would reintroduce exactly that
divergence for a mixed TS/Python agent ecosystem, only now the two halves
would disagree on *every* machine rather than some of them.

So `stellaragent.fixed_point` and `stellaragent.bid` are **semantic ports**,
not rewrites. Every function produces the byte-identical string its TypeScript
counterpart produces.

```python
from stellaragent import fmt, div, to_stroops, rank_bids, AgentBid

fmt("8.2399999", 2)        # '8.23'  — truncates, never rounds up
str(div("2", "3"))         # '0.666666666666666666'  — ROUND_DOWN at 18 dp
to_stroops("1.5000001")    # 15000001

best = rank_bids([
    AgentBid("GALPHA", price="1", reputation="90",
             estimated_latency_seconds="3", success_rate="0.9"),
    AgentBid("GBRAVO", price="2", reputation="95",
             estimated_latency_seconds="1", success_rate="0.99"),
])[0]
best.score                 # '61.2500'
```

### How parity is enforced

[`fixtures/determinism.json`](../fixtures/determinism.json) holds **643 cases**
— arithmetic, formatting, stroop conversions, bid scores, rankings, spend
limits, and the inputs both implementations must reject. It is generated from
the *TypeScript* implementation, which is the reference:

```bash
pnpm fixtures:generate     # regenerate from packages/core
pnpm fixtures:check        # fail if the committed file is stale
```

Both suites assert against that same file:

| Suite | File |
|-------|------|
| vitest | [`packages/core/src/math/__tests__/determinism-fixtures.test.ts`](../packages/core/src/math/__tests__/determinism-fixtures.test.ts) |
| pytest | [`tests/test_determinism.py`](tests/test_determinism.py) |

If both pass, the two implementations are byte-identical for every case. If a
change to either makes them diverge, one of the suites fails. Both run as
required CI checks.

The comparison is **string equality**, not numeric closeness — `pytest.approx`
would defeat the entire point.

### Matching `bignumber.js`

Three details make a naive translation wrong, and all three are handled in
[`fixed_point.py`](src/stellaragent/fixed_point.py):

| `bignumber.js` | Python `Decimal` | Resolution |
|----------------|------------------|------------|
| `DECIMAL_PLACES: 18` applies to **division only**; `+ - *` are exact and unbounded | `Context.prec` applies to **every** operation | A large working precision keeps add/sub/mul exact; only division is quantized to 18 dp |
| `DECIMAL_PLACES` counts **decimal places** | `prec` counts **significant digits** | Explicit `quantize(Decimal('1e-18'))` rather than a precision setting |
| `BigNumber('-0')` normalises to `0`, but truncating `-0.5` yields `'-0'` | `Decimal('-0')` keeps its sign | `bn()` normalises zero at construction; `quantize` reproduces the second case naturally |

A dedicated `Context` object is used rather than `getcontext()`, so a caller
changing the thread-local decimal context cannot silently alter results here.

### One intentional API divergence

`bn()` **rejects `float`**. The TypeScript signature accepts `number`, but a
Python `float` cannot round-trip a decimal string — `Decimal(0.1)` is
`0.1000000000000000055511151231257827` — so accepting one would reintroduce
divergence in precisely the place this module exists to prevent it. Pass a
`str`, `int`, or `Decimal`.

---

## Development

```bash
cd python
python -m venv .venv && source .venv/bin/activate
pip install -e ".[dev]"

pytest                  # full suite, including cross-language parity
pytest --cov            # with coverage
ruff check .            # lint
mypy                    # type check
```

### Why `python/` and not `packages/python/`

`packages/*` is a **pnpm workspace glob**. Anything placed there is treated as
a Node package by pnpm and Turborepo — it would need a `package.json` it has
no use for, and would show up in `turbo run build` graphs it cannot
participate in. A top-level `python/` keeps the two toolchains cleanly
separated: pnpm owns `packages/` and `dashboard/`, pip owns `python/`, and
`fixtures/` is shared by both.

---

## Layout

```
python/
├── pyproject.toml
├── src/stellaragent/
│   ├── fixed_point.py   # port of math/fixed-point.ts
│   ├── bid.py           # port of math/bid.ts
│   ├── contracts.py     # port of contracts.ts
│   ├── types.py         # port of types/index.ts
│   └── agent.py         # port of index.ts (StellarAgent)
└── tests/
    ├── test_determinism.py   # shared-fixture parity suite
    ├── test_fixed_point.py
    ├── test_bid.py
    └── test_agent.py
```

---

## License

MIT © StellarAgent Contributors
