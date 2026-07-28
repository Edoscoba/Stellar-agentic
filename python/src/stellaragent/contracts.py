"""Contract address resolution.

Python port of ``packages/core/src/contracts.ts``, with the same precedence
rules and the same environment variable names — so a single deployment
configures a TS agent and a Python agent identically.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, fields

from stellar_sdk import StrKey

from .types import Network

__all__ = [
    "ContractAddresses",
    "CONTRACT_KEYS",
    "UNCONFIGURED_CONTRACTS",
    "ContractsNotDeployedError",
    "is_deployed_address",
    "env_var_names",
    "resolve_contracts",
    "assert_deployed",
]


@dataclass(frozen=True)
class ContractAddresses:
    """The five contracts the SDK calls directly."""

    agent_wallet_factory: str
    payment_channel: str
    escrow: str
    rate_limiter: str
    circuit_breaker: str


#: In deployment order, matching the TS ``CONTRACT_KEYS``.
CONTRACT_KEYS: tuple[str, ...] = (
    "agent_wallet_factory",
    "payment_channel",
    "escrow",
    "rate_limiter",
    "circuit_breaker",
)

#: Placeholders meaning "nothing deployed here yet".
#:
#: Byte-for-byte the values the TS SDK once shipped as ``DEFAULT_CONTRACTS``.
#: They are not valid contract IDs — 60-61 characters where a Stellar contract
#: ID is exactly 56, with no valid checksum — and :func:`assert_deployed`
#: rejects every one of them.
UNCONFIGURED_CONTRACTS: dict[str, ContractAddresses] = {
    "testnet": ContractAddresses(
        agent_wallet_factory="CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        payment_channel="CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        escrow="CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
        rate_limiter="CDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD",
        circuit_breaker="CEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE",
    ),
    "mainnet": ContractAddresses("", "", "", "", ""),
    "local": ContractAddresses("", "", "", "", ""),
}


def is_deployed_address(address: str | None) -> bool:
    """Whether a string is a real, deployable Stellar contract ID.

    Uses strkey checksum validation rather than a pattern match against the
    known placeholders, so it also catches truncated addresses, addresses
    pasted from the wrong network, and single-character typos.
    """
    if not address:
        return False
    try:
        return bool(StrKey.is_valid_contract(address))
    except Exception:  # noqa: BLE001 — any parse failure means "not valid"
        return False


def env_var_names(network: Network, key: str) -> tuple[str, str]:
    """Environment variables consulted for a contract, most specific first.

    ``('STELLARAGENT_TESTNET_PAYMENT_CHANNEL', 'STELLARAGENT_PAYMENT_CHANNEL')``

    The network-scoped form lets one process talk to more than one network.
    Identical to the TS implementation's names, so one deployment's ``.env``
    block configures both SDKs.
    """
    suffix = key.upper()
    return (f"STELLARAGENT_{network.upper()}_{suffix}", f"STELLARAGENT_{suffix}")


def resolve_contracts(
    network: Network,
    overrides: dict[str, str] | None = None,
) -> ContractAddresses:
    """Resolve contract addresses for a network.

    Precedence, highest first: ``overrides``, the network-scoped environment
    variable, the unscoped environment variable, then the unconfigured
    sentinel. Never raises — use :func:`assert_deployed` to reject the result.
    """
    overrides = overrides or {}
    fallback = UNCONFIGURED_CONTRACTS[network]
    resolved: dict[str, str] = {}

    for key in CONTRACT_KEYS:
        override = overrides.get(key)
        if override:
            resolved[key] = override
            continue
        scoped, unscoped = env_var_names(network, key)
        resolved[key] = (
            os.environ.get(scoped)
            or os.environ.get(unscoped)
            or getattr(fallback, key)
        )

    return ContractAddresses(**resolved)


class ContractsNotDeployedError(RuntimeError):
    """Raised when an agent is created against contracts that are not deployed."""

    def __init__(self, network: Network, missing: list[str]) -> None:
        lines = [
            f'Contracts not deployed for network "{network}" — see docs/deployment.md',
            "",
            f"Unconfigured or invalid: {', '.join(missing)}",
            "",
            "Fix this by either:",
            f"  1. Deploying them:  pnpm deploy:contracts --network {network}",
            f"     (writes deployments/{network}.json and prints an .env block)",
            "  2. Passing known addresses explicitly:",
            "       StellarAgent.create(network=..., contracts={'payment_channel': 'C...'})",
            "  3. Setting environment variables:",
            *[f"       {env_var_names(network, key)[0]}=C..." for key in missing],
        ]
        super().__init__("\n".join(lines))
        self.network = network
        self.missing = missing


def assert_deployed(network: Network, contracts: ContractAddresses) -> None:
    """Raise unless every contract address is a real deployed contract ID.

    Called from :meth:`~stellaragent.agent.StellarAgent.create` so the failure
    names the actual problem, instead of surfacing later as a confusing RPC
    error from the middle of a payment.
    """
    missing = [
        f.name for f in fields(contracts) if not is_deployed_address(getattr(contracts, f.name))
    ]
    if missing:
        raise ContractsNotDeployedError(network, missing)
