"""StellarAgent — AI Agent Payment Rails on Stellar (Python SDK).

Mirrors the TypeScript ``@stellaragent/core`` package. The deterministic math
modules are a strict semantic port: every function produces byte-identical
strings to its TS counterpart, verified by a shared fixture suite
(``fixtures/determinism.json``) that both test suites consume.

>>> from stellaragent import StellarAgent
>>> agent = await StellarAgent.create(network="testnet")  # doctest: +SKIP
"""

from __future__ import annotations

from .agent import StellarAgent
from .bid import (
    DEFAULT_BID_WEIGHTS,
    AgentBid,
    BidWeights,
    ScoreBreakdown,
    ScoredBid,
    is_within_spend_limit,
    rank_bids,
    remaining_budget,
    score_bid,
    select_best_bid,
)
from .contracts import (
    CONTRACT_KEYS,
    UNCONFIGURED_CONTRACTS,
    ContractAddresses,
    ContractsNotDeployedError,
    assert_deployed,
    env_var_names,
    is_deployed_address,
    resolve_contracts,
)
from .fixed_point import (
    BPS_SCALE,
    DECIMAL_PLACES,
    STROOP_SCALE,
    FixedPointError,
    add,
    bn,
    clamp,
    div,
    eq,
    fmt,
    from_stroops,
    gt,
    gte,
    is_positive,
    is_zero,
    lt,
    lte,
    mul,
    pct,
    sub,
    sum_strings,
    to_str,
    to_stroops,
)
from .types import (
    NETWORK_CONFIGS,
    ChannelInfo,
    JobInfo,
    Network,
    NetworkConfig,
    OpenChannelParams,
    PayForAPIParams,
    RateLimitConfig,
    RateLimitStatus,
    RequestWorkParams,
    SpendLimit,
    SpendReport,
    TxResult,
)

__version__ = "0.1.0"

__all__ = [
    "__version__",
    # agent
    "StellarAgent",
    # fixed point
    "BPS_SCALE",
    "DECIMAL_PLACES",
    "STROOP_SCALE",
    "FixedPointError",
    "add",
    "bn",
    "clamp",
    "div",
    "eq",
    "fmt",
    "from_stroops",
    "gt",
    "gte",
    "is_positive",
    "is_zero",
    "lt",
    "lte",
    "mul",
    "pct",
    "sub",
    "sum_strings",
    "to_str",
    "to_stroops",
    # bidding
    "AgentBid",
    "BidWeights",
    "DEFAULT_BID_WEIGHTS",
    "ScoreBreakdown",
    "ScoredBid",
    "is_within_spend_limit",
    "rank_bids",
    "remaining_budget",
    "score_bid",
    "select_best_bid",
    # contracts
    "CONTRACT_KEYS",
    "UNCONFIGURED_CONTRACTS",
    "ContractAddresses",
    "ContractsNotDeployedError",
    "assert_deployed",
    "env_var_names",
    "is_deployed_address",
    "resolve_contracts",
    # types
    "NETWORK_CONFIGS",
    "ChannelInfo",
    "JobInfo",
    "Network",
    "NetworkConfig",
    "OpenChannelParams",
    "PayForAPIParams",
    "RateLimitConfig",
    "RateLimitStatus",
    "RequestWorkParams",
    "SpendLimit",
    "SpendReport",
    "TxResult",
]
