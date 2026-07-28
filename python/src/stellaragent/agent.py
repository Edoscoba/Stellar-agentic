"""``StellarAgent`` — the Python counterpart of ``packages/core/src/index.ts``.

Tracks the TypeScript class's public API. Method names are ``snake_case``;
everything else — arguments, semantics, validation order, error text — is
deliberately the same, so an agent written against one SDK reads the same in
the other.

Current state
-------------
The contract-invoking methods are stubs that raise, exactly as their TS
counterparts do. They are pending the companion "real Soroban invocation"
work; porting them before the TS shape is settled would mean porting a design
that is about to change. What *is* implemented here is everything the TS class
implements today: identity, contract resolution and its fast-fail check,
friendbot funding, and :meth:`get_balance`.

The deterministic math in :mod:`stellaragent.fixed_point` and
:mod:`stellaragent.bid` is complete and verified byte-identical against the
TypeScript implementation — that is the part of this package with a hard
correctness requirement today.
"""

from __future__ import annotations

from typing import Any

from stellar_sdk import Keypair, Server

from .contracts import ContractAddresses, assert_deployed, resolve_contracts
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

__all__ = ["StellarAgent"]

FRIENDBOT_URL = "https://friendbot.stellar.org"


class StellarAgent:
    """Main SDK class for AI Agent payment operations on Stellar.

    Use :meth:`create` rather than the constructor.

    >>> agent = StellarAgent.create(  # doctest: +SKIP
    ...     network="testnet",
    ...     spend_limit=SpendLimit(amount="10", asset="USDC", period="hourly"),
    ... )
    >>> agent.pay_for_api(  # doctest: +SKIP
    ...     PayForAPIParams(
    ...         endpoint="https://api.example.com/inference",
    ...         amount="0.001",
    ...         asset="USDC",
    ...     )
    ... )
    """

    def __init__(
        self,
        keypair: Keypair,
        network_config: NetworkConfig,
        contracts: ContractAddresses,
    ) -> None:
        # Private by convention and by name-mangling: the secret should not be
        # something a caller reaches for casually. See the note on
        # :attr:`secret_key`.
        self.__keypair = keypair
        self._network_config = network_config
        self._contracts = contracts
        self._horizon = Server(horizon_url=network_config.horizon_url)
        self._active_channel_id: int | None = None

    # ── Factory methods ──────────────────────────────────────────────────────

    @classmethod
    def create(
        cls,
        network: Network = "testnet",
        secret_key: str | None = None,
        spend_limit: SpendLimit | None = None,
        contracts: dict[str, str] | None = None,
        allow_unconfigured_contracts: bool = False,
    ) -> StellarAgent:
        """Create a new agent. Generates a fresh keypair when no secret is given.

        Contract addresses resolve from ``contracts``, then from the
        ``STELLARAGENT_*`` environment variables, then from the per-network
        unconfigured sentinels — the same precedence as the TypeScript SDK,
        reading the same variable names.

        :raises ContractsNotDeployedError: when the resolved contracts are not
            real deployed contract IDs and ``allow_unconfigured_contracts`` is
            false. Pass ``True`` when you only need contract-free calls such
            as :meth:`get_balance`.
        """
        if network not in NETWORK_CONFIGS:
            raise ValueError(
                f"Unknown network {network!r}. Expected one of: "
                f"{', '.join(sorted(NETWORK_CONFIGS))}"
            )

        keypair = Keypair.from_secret(secret_key) if secret_key else Keypair.random()
        network_config = NETWORK_CONFIGS[network]
        resolved = resolve_contracts(network, contracts)

        if not allow_unconfigured_contracts:
            assert_deployed(network, resolved)

        agent = cls(keypair, network_config, resolved)
        agent._spend_limit = spend_limit  # type: ignore[attr-defined]

        # Only a freshly generated keypair gets funded, matching the TS rule.
        if network == "testnet" and not secret_key:
            agent._fund_from_friendbot()

        return agent

    @classmethod
    def from_secret(
        cls,
        secret_key: str,
        network: Network = "testnet",
        **options: Any,
    ) -> StellarAgent:
        """Restore an agent from an existing secret key.

        ``options`` forwards the rest of :meth:`create` — notably ``contracts``
        and ``allow_unconfigured_contracts``, without which a restored agent
        could only ever target contracts resolved from the environment.
        """
        return cls.create(network=network, secret_key=secret_key, **options)

    # ── Identity ─────────────────────────────────────────────────────────────

    @property
    def address(self) -> str:
        """The agent's Stellar public address."""
        return str(self.__keypair.public_key)

    @property
    def secret_key(self) -> str:
        """The agent's secret key — keep this safe.

        .. deprecated::
           The TypeScript SDK has moved signing behind a ``Signer`` interface
           so key material need not live in the agent process at all (see
           ``docs/signing.md``). This property is the pattern that abstraction
           exists to remove, and is kept only for parity with the TS class's
           current surface. A Python ``Signer`` equivalent should land before
           this package is used with real funds.
        """
        return str(self.__keypair.secret)

    @property
    def contracts(self) -> ContractAddresses:
        """The resolved contract addresses this agent will call."""
        return self._contracts

    @property
    def network_config(self) -> NetworkConfig:
        """RPC, Horizon and passphrase for the selected network."""
        return self._network_config

    # ── Payment channel ──────────────────────────────────────────────────────

    def open_channel(self, params: OpenChannelParams) -> int:
        """Open a payment channel and return its ID.

        :raises NotImplementedError: pending real Soroban invocation.
        """
        raise NotImplementedError(
            "Not yet implemented — contract invocation is pending. "
            "See contracts/payment_channel/src/lib.rs and docs/deployment.md"
        )

    def pay_for_api(self, params: PayForAPIParams) -> TxResult:
        """Pay for an API call, deducting from the active payment channel.

        Validation runs in the same order as the TS implementation, so both
        SDKs reject the same call with the same message.

        :raises NotImplementedError: pending real Soroban invocation.
        """
        if self._active_channel_id is None:
            raise RuntimeError("No active payment channel. Call open_channel() first.")

        if (params.dest_asset is not None) != (params.min_received is not None):
            raise ValueError("dest_asset and min_received must be set together")

        raise NotImplementedError(
            "Not yet implemented — see contracts/payment_channel/src/lib.rs"
        )

    # ── Agent-to-agent escrow ────────────────────────────────────────────────

    def request_work(self, params: RequestWorkParams) -> int:
        """Create an escrow job delegating work to another agent."""
        raise NotImplementedError("Not yet implemented — see contracts/escrow/src/lib.rs")

    def accept_job(self, job_id: int) -> TxResult:
        """Accept an open escrow job as a worker agent."""
        raise NotImplementedError("Not yet implemented")

    def submit_result(self, job_id: int, result: str) -> TxResult:
        """Submit a work result for an escrow job."""
        raise NotImplementedError("Not yet implemented")

    def release_payment(self, job_id: int) -> TxResult:
        """Release escrow payment to the worker."""
        raise NotImplementedError("Not yet implemented")

    # ── Rate limits ──────────────────────────────────────────────────────────

    def set_rate_limits(self, config: RateLimitConfig) -> TxResult:
        """Configure on-chain rate limits for this agent."""
        raise NotImplementedError("Not yet implemented")

    def check_rate_limit(self, amount: str) -> bool:
        """Whether a payment would be blocked by rate limits (read-only)."""
        raise NotImplementedError("Not yet implemented")

    # ── Queries ──────────────────────────────────────────────────────────────

    def get_balance(self) -> str:
        """Current native XLM balance, or ``"0"`` if the account is unknown.

        A Horizon query — it needs no contracts and no signing, which is why
        it works on an agent created with ``allow_unconfigured_contracts``.
        """
        try:
            account = self._horizon.accounts().account_id(self.address).call()
        except Exception:  # noqa: BLE001 — an unfunded account is a normal state
            return "0"

        for balance in account.get("balances", []):
            if balance.get("asset_type") == "native":
                return str(balance.get("balance", "0"))
        return "0"

    def get_spend_report(self) -> SpendReport:
        """Spend report for the current period."""
        raise NotImplementedError("Not yet implemented")

    def get_channel(self, channel_id: int) -> ChannelInfo:
        """Info about a payment channel."""
        raise NotImplementedError("Not yet implemented")

    def get_job(self, job_id: int) -> JobInfo:
        """Info about an escrow job."""
        raise NotImplementedError("Not yet implemented")

    def get_rate_limit_status(self) -> RateLimitStatus:
        """Current rate-limit usage alongside the configured limits."""
        raise NotImplementedError("Not yet implemented")

    # ── Internals ────────────────────────────────────────────────────────────

    def _fund_from_friendbot(self) -> None:
        """Best-effort testnet funding. An already-funded account is not an error."""
        try:
            import urllib.request

            with urllib.request.urlopen(
                f"{FRIENDBOT_URL}?addr={self.address}", timeout=30
            ) as response:
                if response.status != 200:
                    print("Friendbot funding failed — account may already exist")
        except Exception:  # noqa: BLE001 — funding is optional
            print("Could not reach friendbot")

    def __repr__(self) -> str:
        # Deliberately omits the secret: a repr lands in logs and tracebacks.
        return f"StellarAgent(address={self.address!r}, network={self._network_config.horizon_url!r})"
