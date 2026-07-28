"""Unit tests for :mod:`stellaragent.agent` and :mod:`stellaragent.contracts`.

The contract-invoking methods are stubs pending the companion "real Soroban
invocation" work, so what is asserted here is the surface that *is* live:
identity, contract resolution and its fast-fail check, validation ordering,
and the read-only balance path. The stub methods are pinned to raise, so
implementing them is a deliberate, test-visible change rather than a silent
behaviour switch.
"""

from __future__ import annotations

import os
from collections.abc import Iterator
from typing import Any

import pytest
from stellar_sdk import Keypair
from stellar_sdk.exceptions import Ed25519SecretSeedInvalidError

from stellaragent import StellarAgent
from stellaragent.contracts import (
    CONTRACT_KEYS,
    UNCONFIGURED_CONTRACTS,
    ContractAddresses,
    ContractsNotDeployedError,
    assert_deployed,
    env_var_names,
    is_deployed_address,
    resolve_contracts,
)
from stellaragent.types import NETWORK_CONFIGS, PayForAPIParams

# Same deterministic test keypair the TypeScript suite uses, so both sides
# assert against the same address.
TEST_SECRET = "SADQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQP54X"
TEST_PUBLIC = "GDVEU3DD4KOFECV66VIHWEZOYX4ZKR3WV27L464SIIPOU2IUI3JCZA57"

# Structurally valid contract IDs — strkey-encoded from fixed payloads, so they
# carry real checksums. Nothing is deployed at them.
DEPLOYED: dict[str, str] = {
    "agent_wallet_factory": "CAAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQC526",
    "payment_channel": "CABAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAFNSZ",
    "escrow": "CABQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGCK3",
    "rate_limiter": "CACAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAINCW",
    "circuit_breaker": "CACQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQLC2U",
}


@pytest.fixture(autouse=True)
def _clean_env() -> Iterator[None]:
    """Keep STELLARAGENT_* out of the environment unless a test sets it."""
    saved = {k: v for k, v in os.environ.items() if k.startswith("STELLARAGENT_")}
    for key in saved:
        del os.environ[key]
    yield
    for key in [k for k in os.environ if k.startswith("STELLARAGENT_")]:
        del os.environ[key]
    os.environ.update(saved)


def make_agent(**overrides: Any) -> StellarAgent:
    kwargs: dict[str, Any] = {
        "network": "testnet",
        "secret_key": TEST_SECRET,
        "contracts": DEPLOYED,
    }
    kwargs.update(overrides)
    return StellarAgent.create(**kwargs)


# ─── Contract validation ─────────────────────────────────────────────────────


class TestIsDeployedAddress:
    def test_accepts_a_real_contract_id(self) -> None:
        assert is_deployed_address(DEPLOYED["payment_channel"])

    def test_rejects_every_testnet_placeholder(self) -> None:
        for key in CONTRACT_KEYS:
            assert not is_deployed_address(getattr(UNCONFIGURED_CONTRACTS["testnet"], key))

    @pytest.mark.parametrize(
        "value", ["", None, "not-a-contract", TEST_PUBLIC, "   ", DEPLOYED["escrow"].lower()]
    )
    def test_rejects_invalid_values(self, value: Any) -> None:
        assert not is_deployed_address(value)

    def test_rejects_a_single_character_typo(self) -> None:
        # Checksum validation, not pattern matching — this is why.
        typo = DEPLOYED["escrow"][:-1] + "A"
        assert len(typo) == 56
        assert not is_deployed_address(typo)

    def test_rejects_a_truncated_id(self) -> None:
        assert not is_deployed_address(DEPLOYED["escrow"][:55])

    def test_the_shipped_placeholders_are_not_even_the_right_length(self) -> None:
        # A real contract ID is exactly 56 characters; these are 60-61.
        for key in CONTRACT_KEYS:
            assert len(getattr(UNCONFIGURED_CONTRACTS["testnet"], key)) != 56


class TestEnvVarNames:
    def test_matches_the_typescript_names_exactly(self) -> None:
        # One deployment's .env block must configure both SDKs.
        assert env_var_names("testnet", "payment_channel") == (
            "STELLARAGENT_TESTNET_PAYMENT_CHANNEL",
            "STELLARAGENT_PAYMENT_CHANNEL",
        )
        assert env_var_names("local", "agent_wallet_factory")[0] == (
            "STELLARAGENT_LOCAL_AGENT_WALLET_FACTORY"
        )

    def test_every_pair_is_unique(self) -> None:
        names = [env_var_names(n, k)[0] for n in NETWORK_CONFIGS for k in CONTRACT_KEYS]
        assert len(set(names)) == len(names)


class TestResolveContracts:
    def test_falls_back_to_the_sentinels(self) -> None:
        assert resolve_contracts("testnet") == UNCONFIGURED_CONTRACTS["testnet"]

    def test_reads_a_network_scoped_variable(self) -> None:
        os.environ["STELLARAGENT_TESTNET_ESCROW"] = DEPLOYED["escrow"]
        assert resolve_contracts("testnet").escrow == DEPLOYED["escrow"]

    def test_falls_back_to_the_unscoped_variable(self) -> None:
        os.environ["STELLARAGENT_ESCROW"] = DEPLOYED["escrow"]
        assert resolve_contracts("local").escrow == DEPLOYED["escrow"]

    def test_scoped_beats_unscoped(self) -> None:
        os.environ["STELLARAGENT_ESCROW"] = DEPLOYED["payment_channel"]
        os.environ["STELLARAGENT_TESTNET_ESCROW"] = DEPLOYED["escrow"]
        assert resolve_contracts("testnet").escrow == DEPLOYED["escrow"]

    def test_explicit_override_beats_everything(self) -> None:
        os.environ["STELLARAGENT_TESTNET_ESCROW"] = DEPLOYED["payment_channel"]
        resolved = resolve_contracts("testnet", {"escrow": DEPLOYED["escrow"]})
        assert resolved.escrow == DEPLOYED["escrow"]

    def test_keeps_networks_separate(self) -> None:
        os.environ["STELLARAGENT_TESTNET_ESCROW"] = DEPLOYED["escrow"]
        os.environ["STELLARAGENT_LOCAL_ESCROW"] = DEPLOYED["payment_channel"]
        assert resolve_contracts("testnet").escrow == DEPLOYED["escrow"]
        assert resolve_contracts("local").escrow == DEPLOYED["payment_channel"]

    def test_never_raises(self) -> None:
        assert resolve_contracts("mainnet") is not None


class TestAssertDeployed:
    def test_passes_for_a_complete_set(self) -> None:
        assert_deployed("testnet", ContractAddresses(**DEPLOYED))

    def test_rejects_the_placeholders(self) -> None:
        with pytest.raises(ContractsNotDeployedError):
            assert_deployed("testnet", UNCONFIGURED_CONTRACTS["testnet"])

    def test_names_every_missing_contract(self) -> None:
        partial = ContractAddresses(**{**DEPLOYED, "escrow": "", "rate_limiter": ""})
        with pytest.raises(ContractsNotDeployedError) as exc:
            assert_deployed("local", partial)
        assert exc.value.missing == ["escrow", "rate_limiter"]

    def test_message_points_at_the_runbook(self) -> None:
        with pytest.raises(ContractsNotDeployedError, match="docs/deployment.md"):
            assert_deployed("testnet", UNCONFIGURED_CONTRACTS["testnet"])

    def test_message_lists_only_the_missing_env_vars(self) -> None:
        partial = ContractAddresses(**{**DEPLOYED, "escrow": ""})
        with pytest.raises(ContractsNotDeployedError) as exc:
            assert_deployed("testnet", partial)
        assert "STELLARAGENT_TESTNET_ESCROW=" in str(exc.value)
        assert "STELLARAGENT_TESTNET_PAYMENT_CHANNEL=" not in str(exc.value)


# ─── Agent identity ──────────────────────────────────────────────────────────


class TestIdentity:
    def test_derives_the_address_from_the_secret(self) -> None:
        assert make_agent().address == TEST_PUBLIC

    def test_matches_the_typescript_fixture_address(self) -> None:
        # Both SDKs derive the same address from the same seed.
        assert Keypair.from_secret(TEST_SECRET).public_key == TEST_PUBLIC

    def test_generates_a_fresh_keypair_when_no_secret_is_given(self) -> None:
        a = make_agent(network="local", secret_key=None)
        b = make_agent(network="local", secret_key=None)
        assert a.address != b.address
        assert a.address.startswith("G") and len(a.address) == 56

    def test_exposes_the_secret_it_was_given(self) -> None:
        assert make_agent().secret_key == TEST_SECRET

    def test_repr_does_not_leak_the_secret(self) -> None:
        # A repr lands in logs and tracebacks.
        assert TEST_SECRET not in repr(make_agent())
        assert TEST_PUBLIC in repr(make_agent())

    def test_rejects_a_malformed_secret(self) -> None:
        with pytest.raises(Ed25519SecretSeedInvalidError):
            make_agent(secret_key="not-a-secret")

    def test_rejects_an_unknown_network(self) -> None:
        with pytest.raises(ValueError, match="Unknown network"):
            make_agent(network="mars")

    def test_from_secret_restores_the_same_address(self) -> None:
        agent = StellarAgent.from_secret(TEST_SECRET, "local", contracts=DEPLOYED)
        assert agent.address == TEST_PUBLIC

    def test_from_secret_forwards_options(self) -> None:
        # Without the passthrough a restored agent could only reach contracts
        # resolved from the environment.
        agent = StellarAgent.from_secret(TEST_SECRET, "local", contracts=DEPLOYED)
        assert agent.contracts.escrow == DEPLOYED["escrow"]


class TestNetworkSelection:
    @pytest.mark.parametrize(
        ("network", "passphrase"),
        [
            ("testnet", "Test SDF Network ; September 2015"),
            ("mainnet", "Public Global Stellar Network ; September 2015"),
            ("local", "Standalone Network ; February 2017"),
        ],
    )
    def test_selects_the_right_passphrase(self, network: str, passphrase: str) -> None:
        assert make_agent(network=network).network_config.network_passphrase == passphrase

    def test_passphrases_match_the_typescript_config(self) -> None:
        # A mismatch here would make signatures from one SDK invalid for the
        # other's network.
        assert NETWORK_CONFIGS["testnet"].network_passphrase == "Test SDF Network ; September 2015"


# ─── Fast-fail on undeployed contracts ───────────────────────────────────────


class TestDeployedContractsCheck:
    def test_refuses_the_testnet_placeholders(self) -> None:
        with pytest.raises(ContractsNotDeployedError, match='network "testnet"'):
            StellarAgent.create(network="testnet", secret_key=TEST_SECRET)

    @pytest.mark.parametrize("network", ["mainnet", "local"])
    def test_refuses_an_unconfigured_network(self, network: str) -> None:
        with pytest.raises(ContractsNotDeployedError):
            StellarAgent.create(network=network, secret_key=TEST_SECRET)

    def test_rejects_a_partially_configured_set(self) -> None:
        with pytest.raises(ContractsNotDeployedError, match="escrow"):
            StellarAgent.create(
                network="local",
                secret_key=TEST_SECRET,
                contracts={**DEPLOYED, "escrow": ""},
            )

    def test_resolves_from_environment_variables(self) -> None:
        for key, value in DEPLOYED.items():
            os.environ[env_var_names("local", key)[0]] = value
        agent = StellarAgent.create(network="local", secret_key=TEST_SECRET)
        assert agent.contracts == ContractAddresses(**DEPLOYED)

    def test_can_be_bypassed_for_read_only_use(self) -> None:
        agent = StellarAgent.create(
            network="local", secret_key=TEST_SECRET, allow_unconfigured_contracts=True
        )
        assert agent.address == TEST_PUBLIC


# ─── Method behaviour ────────────────────────────────────────────────────────


class TestPayForAPI:
    def test_refuses_with_no_open_channel(self) -> None:
        with pytest.raises(RuntimeError, match="No active payment channel"):
            make_agent().pay_for_api(PayForAPIParams(endpoint="https://x", amount="0.001"))

    def test_checks_the_channel_before_validating_arguments(self) -> None:
        # Same ordering as the TypeScript implementation.
        with pytest.raises(RuntimeError, match="No active payment channel"):
            make_agent().pay_for_api(
                PayForAPIParams(endpoint="https://x", amount="0.001", dest_asset="XLM")
            )

    @pytest.mark.parametrize(
        "params",
        [
            {"dest_asset": "XLM"},
            {"min_received": "0.009"},
        ],
    )
    def test_rejects_a_half_specified_conversion(self, params: dict[str, str]) -> None:
        agent = make_agent()
        agent._active_channel_id = 1
        with pytest.raises(ValueError, match="dest_asset and min_received must be set together"):
            agent.pay_for_api(PayForAPIParams(endpoint="https://x", amount="0.001", **params))

    def test_accepts_both_together_and_falls_through_to_the_stub(self) -> None:
        agent = make_agent()
        agent._active_channel_id = 1
        with pytest.raises(NotImplementedError):
            agent.pay_for_api(
                PayForAPIParams(
                    endpoint="https://x", amount="0.001", dest_asset="XLM", min_received="0.009"
                )
            )


class TestGetBalance:
    def test_returns_the_native_balance(self, monkeypatch: pytest.MonkeyPatch) -> None:
        agent = make_agent()
        monkeypatch.setattr(
            agent,
            "_horizon",
            _FakeHorizon(
                {
                    "balances": [
                        {"asset_type": "credit_alphanum4", "balance": "250.0"},
                        {"asset_type": "native", "balance": "1234.5670000"},
                    ]
                }
            ),
        )
        assert agent.get_balance() == "1234.5670000"

    def test_returns_zero_with_no_native_entry(self, monkeypatch: pytest.MonkeyPatch) -> None:
        agent = make_agent()
        monkeypatch.setattr(
            agent, "_horizon", _FakeHorizon({"balances": [{"asset_type": "credit_alphanum4"}]})
        )
        assert agent.get_balance() == "0"

    def test_returns_zero_for_a_missing_account(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # An unfunded account is a normal state, not an error.
        agent = make_agent()
        monkeypatch.setattr(agent, "_horizon", _FakeHorizon(None, raises=True))
        assert agent.get_balance() == "0"

    def test_works_without_deployed_contracts(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # It touches no contract, so it must stay usable on an unconfigured
        # network — same property as the TypeScript SDK.
        agent = StellarAgent.create(
            network="local", secret_key=TEST_SECRET, allow_unconfigured_contracts=True
        )
        monkeypatch.setattr(
            agent, "_horizon", _FakeHorizon({"balances": [{"asset_type": "native", "balance": "7"}]})
        )
        assert agent.get_balance() == "7"


class TestUnimplementedSurface:
    """Pin the stubs to raise, so implementing them is a visible change."""

    @pytest.mark.parametrize(
        "call",
        [
            lambda a: a.request_work(None),
            lambda a: a.accept_job(1),
            lambda a: a.submit_result(1, "r"),
            lambda a: a.release_payment(1),
            lambda a: a.set_rate_limits(None),
            lambda a: a.check_rate_limit("1"),
            lambda a: a.get_spend_report(),
            lambda a: a.get_channel(1),
            lambda a: a.get_job(1),
            lambda a: a.get_rate_limit_status(),
        ],
    )
    def test_raises_not_implemented(self, call: Any) -> None:
        with pytest.raises(NotImplementedError):
            call(make_agent())

    def test_open_channel_points_at_the_contract(self) -> None:
        with pytest.raises(NotImplementedError, match="payment_channel"):
            make_agent().open_channel(None)


# ─── Helpers ─────────────────────────────────────────────────────────────────


class _FakeHorizon:
    """Minimal stand-in for ``stellar_sdk.Server``'s account query chain."""

    def __init__(self, payload: Any, raises: bool = False) -> None:
        self._payload = payload
        self._raises = raises

    def accounts(self) -> _FakeHorizon:
        return self

    def account_id(self, _address: str) -> _FakeHorizon:
        return self

    def call(self) -> Any:
        if self._raises:
            raise RuntimeError("404 Not Found")
        return self._payload
