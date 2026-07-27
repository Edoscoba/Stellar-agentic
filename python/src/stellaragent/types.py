"""Type definitions mirroring ``packages/core/src/types/index.ts``.

Field names are ``snake_case`` per Python convention; the TS ``camelCase``
equivalent is noted where it is not a mechanical transformation.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal

__all__ = [
    "Network",
    "NetworkConfig",
    "NETWORK_CONFIGS",
    "SpendPeriod",
    "SpendLimit",
    "AgentInfo",
    "OpenChannelParams",
    "PayForAPIParams",
    "ChannelInfo",
    "SpendReport",
    "JobStatus",
    "RequestWorkParams",
    "JobInfo",
    "RateLimitConfig",
    "RateLimitStatus",
    "TxResult",
]

Network = Literal["mainnet", "testnet", "local"]


@dataclass(frozen=True)
class NetworkConfig:
    rpc_url: str
    network_passphrase: str
    horizon_url: str


NETWORK_CONFIGS: dict[str, NetworkConfig] = {
    "mainnet": NetworkConfig(
        rpc_url="https://soroban-rpc.stellar.org",
        network_passphrase="Public Global Stellar Network ; September 2015",
        horizon_url="https://horizon.stellar.org",
    ),
    "testnet": NetworkConfig(
        rpc_url="https://soroban-rpc.testnet.stellar.gateway.fm",
        network_passphrase="Test SDF Network ; September 2015",
        horizon_url="https://horizon-testnet.stellar.org",
    ),
    "local": NetworkConfig(
        rpc_url="http://localhost:8000/soroban/rpc",
        network_passphrase="Standalone Network ; February 2017",
        horizon_url="http://localhost:8000",
    ),
}

SpendPeriod = Literal["per_ledger", "hourly", "daily"]


@dataclass(frozen=True)
class SpendLimit:
    """Maximum spend per period, enforced on-chain."""

    amount: str
    asset: str
    period: SpendPeriod


@dataclass(frozen=True)
class AgentInfo:
    id: int
    address: str
    name: str
    owner: str
    active: bool
    created_at: int
    total_ops: int


@dataclass(frozen=True)
class OpenChannelParams:
    """Parameters for opening a payment channel.

    ``limit_per_period`` is always denominated in ``token``, the channel's
    single funding/settlement asset — even for cross-asset payments made via
    :class:`PayForAPIParams`'s ``dest_asset``.
    """

    deposit: str
    limit_per_period: str
    period: SpendPeriod
    token: str | None = None


@dataclass(frozen=True)
class PayForAPIParams:
    """Parameters for paying for an API call.

    Setting ``dest_asset`` routes through ``PaymentChannel.pay_with_conversion``
    so the recipient is settled in a different asset than the channel holds.
    It requires ``min_received`` — a slippage floor in ``dest_asset`` units —
    to be set as well. The spend limit is still enforced in the channel's own
    settlement asset either way.
    """

    endpoint: str
    amount: str
    asset: str | None = None
    channel_id: int | None = None
    dest_asset: str | None = None
    min_received: str | None = None


@dataclass(frozen=True)
class ChannelInfo:
    id: int
    agent: str
    owner: str
    token: str
    limit_per_period: int
    spent_this_period: int
    total_spent: int
    active: bool


@dataclass(frozen=True)
class SpendReport:
    spent_this_period: str
    remaining_this_period: str
    total_lifetime: str


JobStatus = Literal[
    "open", "in_progress", "pending_release", "completed", "refunded", "disputed"
]


@dataclass(frozen=True)
class RequestWorkParams:
    worker_agent: str
    task: str
    escrow_amount: str
    asset: str | None = None
    deadline_ledgers: int | None = None
    arbiter: str | None = None


@dataclass(frozen=True)
class JobInfo:
    id: int
    requester: str
    worker: str | None
    arbiter: str | None
    token: str
    amount: int
    task_description: str
    result: str | None
    deadline_ledger: int
    status: JobStatus
    created_at: int


@dataclass(frozen=True)
class RateLimitConfig:
    max_per_tx: str
    max_per_hour: str
    max_per_day: str
    max_txs_per_hour: int


@dataclass(frozen=True)
class RateLimitStatus:
    max_per_tx: str
    max_per_hour: str
    max_per_day: str
    max_txs_per_hour: int
    spent_this_hour: str
    spent_today: str
    txs_this_hour: int


@dataclass(frozen=True)
class TxResult:
    hash: str
    success: bool
    ledger: int | None = field(default=None)
