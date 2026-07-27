/**
 * Contract address resolution.
 *
 * ## Why this module exists
 *
 * `DEFAULT_CONTRACTS` used to hard-code addresses like
 * `CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA` for testnet
 * and empty strings for mainnet/local. Those are not real contract IDs — they
 * are 60-61 characters long where a Stellar contract ID is exactly 56, and
 * none of them carry a valid strkey checksum. Any call made against them
 * would have failed deep inside a Soroban RPC round-trip with an opaque
 * error, long after the real mistake (never having deployed) was made.
 *
 * This module replaces that with three things:
 *
 * 1. An explicit, named set of **unconfigured sentinels** — the same fake
 *    values, but now labelled as what they are rather than posing as
 *    deployment output.
 * 2. **Resolution** from environment variables, so a real deployment can be
 *    supplied without recompiling.
 * 3. A **fast-fail check** that rejects an unconfigured or malformed address
 *    at `StellarAgent.create()` time with a message pointing at the runbook.
 *
 * ## Why environment variables rather than reading the generated file
 *
 * `scripts/deploy.ts` writes `deployments/<network>.json`. This package,
 * however, is bundled for browsers as well as Node — `@stellaragent/dashboard`
 * imports it directly — so it cannot `fs.readFile` a path at runtime without
 * breaking that build. Environment variables are the portable channel (the
 * issue sanctions either), and the deploy script prints a ready-to-paste
 * `.env` block alongside the JSON so the two stay in step. Callers who do
 * want the file can import the JSON and pass it as `config.contracts`, which
 * takes precedence over everything here.
 *
 * @module contracts
 */

import { StrKey } from '@stellar/stellar-sdk';

import type { ContractAddresses, Network } from './types/index.js';

/** Every key of `ContractAddresses`, in deployment order. */
export const CONTRACT_KEYS = [
  'agentWalletFactory',
  'paymentChannel',
  'escrow',
  'rateLimiter',
  'circuitBreaker',
] as const satisfies readonly (keyof ContractAddresses)[];

export type ContractKey = (typeof CONTRACT_KEYS)[number];

/**
 * Placeholder addresses standing in for "nothing has been deployed to this
 * network yet".
 *
 * These are deliberately kept — removing them would make an unconfigured
 * agent fail with `undefined` rather than a message — but they are no longer
 * called "defaults", because nothing about them is usable. `assertDeployed`
 * rejects every one of them.
 */
export const UNCONFIGURED_CONTRACTS: Record<Network, ContractAddresses> = {
  testnet: {
    agentWalletFactory: 'CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    paymentChannel: 'CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB',
    escrow: 'CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC',
    rateLimiter: 'CDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD',
    circuitBreaker: 'CEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE',
  },
  mainnet: {
    agentWalletFactory: '',
    paymentChannel: '',
    escrow: '',
    rateLimiter: '',
    circuitBreaker: '',
  },
  local: {
    agentWalletFactory: '',
    paymentChannel: '',
    escrow: '',
    rateLimiter: '',
    circuitBreaker: '',
  },
};

/**
 * Whether a string is a real, deployable Stellar contract ID.
 *
 * Uses strkey validation rather than a pattern match against the known
 * placeholders: that also catches truncated addresses, addresses pasted from
 * the wrong network's output, and single-character typos — all of which
 * otherwise surface as the same opaque RPC failure.
 */
export function isDeployedAddress(address: string | undefined): boolean {
  if (!address) return false;
  try {
    return StrKey.isValidContract(address);
  } catch {
    return false;
  }
}

/** `agentWalletFactory` → `AGENT_WALLET_FACTORY` */
function envSuffix(key: ContractKey): string {
  return key.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toUpperCase();
}

/**
 * Environment variable names consulted for a contract, most specific first:
 * `STELLARAGENT_TESTNET_PAYMENT_CHANNEL` then `STELLARAGENT_PAYMENT_CHANNEL`.
 * The network-scoped form lets one process talk to more than one network.
 */
export function envVarNames(network: Network, key: ContractKey): [string, string] {
  const suffix = envSuffix(key);
  return [
    `STELLARAGENT_${network.toUpperCase()}_${suffix}`,
    `STELLARAGENT_${suffix}`,
  ];
}

/** Read `process.env` if it exists — this package also runs in browsers. */
function readEnv(name: string): string | undefined {
  const env = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process
    ?.env;
  const value = env?.[name];
  return value && value.length > 0 ? value : undefined;
}

/**
 * Resolve the contract addresses for a network.
 *
 * Precedence, highest first:
 * 1. `overrides` — what the caller passed as `config.contracts`
 * 2. `STELLARAGENT_<NETWORK>_<CONTRACT>` environment variable
 * 3. `STELLARAGENT_<CONTRACT>` environment variable
 * 4. the unconfigured sentinel for that network
 *
 * Resolution never throws — it reports what it found. Use `assertDeployed`
 * to reject the result if it is still unconfigured.
 */
export function resolveContracts(
  network: Network,
  overrides?: Partial<ContractAddresses>,
): ContractAddresses {
  const resolved = {} as ContractAddresses;
  for (const key of CONTRACT_KEYS) {
    const override = overrides?.[key];
    if (override) {
      resolved[key] = override;
      continue;
    }
    const [scoped, global] = envVarNames(network, key);
    resolved[key] = readEnv(scoped) ?? readEnv(global) ?? UNCONFIGURED_CONTRACTS[network][key];
  }
  return resolved;
}

/** Thrown when an agent is created against contracts that are not deployed. */
export class ContractsNotDeployedError extends Error {
  readonly network: Network;
  /** The contract keys that failed validation, in `CONTRACT_KEYS` order. */
  readonly missing: ContractKey[];

  constructor(network: Network, missing: ContractKey[]) {
    const lines = [
      `Contracts not deployed for network "${network}" — see docs/deployment.md`,
      '',
      `Unconfigured or invalid: ${missing.join(', ')}`,
      '',
      'Fix this by either:',
      `  1. Deploying them:  pnpm deploy:contracts --network ${network}`,
      '     (writes deployments/' + network + '.json and prints an .env block)',
      '  2. Passing known addresses explicitly:',
      "       StellarAgent.create({ network, contracts: { paymentChannel: 'C...' } })",
      '  3. Setting environment variables:',
      ...missing.map((key) => `       ${envVarNames(network, key)[0]}=C...`),
    ];
    super(lines.join('\n'));
    this.name = 'ContractsNotDeployedError';
    this.network = network;
    this.missing = missing;
  }
}

/**
 * Throw unless every contract address is a real deployed contract ID.
 *
 * This runs at `StellarAgent.create()` time so the failure names the actual
 * problem, instead of surfacing later as a confusing RPC error from the
 * middle of a payment.
 */
export function assertDeployed(network: Network, contracts: ContractAddresses): void {
  const missing = CONTRACT_KEYS.filter((key) => !isDeployedAddress(contracts[key]));
  if (missing.length > 0) {
    throw new ContractsNotDeployedError(network, missing);
  }
}
