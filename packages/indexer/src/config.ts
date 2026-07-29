import { readFileSync } from "node:fs";
import type { ContractAddresses } from "./types.js";

interface DeploymentFile {
  contracts?: Partial<ContractAddresses>;
}

function positiveInteger(value: string | undefined, name: string, fallback?: number): number {
  const parsed = value === undefined ? fallback : Number(value);
  if (parsed === undefined || !Number.isSafeInteger(parsed) || parsed < 0) {
    throw new Error(`${name} must be a non-negative integer`);
  }
  return parsed;
}

export interface EnvironmentConfig {
  rpcUrl: string;
  databasePath: string;
  contracts: ContractAddresses;
  startLedger: number;
  rollbackWindow: number;
  finalityLag: number;
  pollIntervalMs: number;
  port: number;
}

export function loadEnvironment(env = process.env): EnvironmentConfig {
  let deployed: DeploymentFile = {};
  if (env.INDEXER_DEPLOYMENT_FILE) {
    deployed = JSON.parse(readFileSync(env.INDEXER_DEPLOYMENT_FILE, "utf8")) as DeploymentFile;
  }
  const contracts = {
    paymentChannel:
      env.PAYMENT_CHANNEL_CONTRACT ?? deployed.contracts?.paymentChannel,
    escrow: env.ESCROW_CONTRACT ?? deployed.contracts?.escrow,
    rateLimiter: env.RATE_LIMITER_CONTRACT ?? deployed.contracts?.rateLimiter,
    agentWalletFactory:
      env.AGENT_WALLET_FACTORY_CONTRACT ??
      deployed.contracts?.agentWalletFactory,
  };
  for (const [name, address] of Object.entries(contracts)) {
    if (!address) throw new Error(`missing ${name} contract address`);
  }
  if (!env.SOROBAN_RPC_URL) throw new Error("missing SOROBAN_RPC_URL");

  return {
    rpcUrl: env.SOROBAN_RPC_URL,
    databasePath: env.INDEXER_DATABASE ?? "stellaragent-events.sqlite",
    contracts: contracts as ContractAddresses,
    startLedger: positiveInteger(env.INDEXER_START_LEDGER, "INDEXER_START_LEDGER"),
    rollbackWindow: positiveInteger(env.INDEXER_ROLLBACK_WINDOW, "INDEXER_ROLLBACK_WINDOW", 12),
    finalityLag: positiveInteger(env.INDEXER_FINALITY_LAG, "INDEXER_FINALITY_LAG", 1),
    pollIntervalMs: positiveInteger(env.INDEXER_POLL_INTERVAL_MS, "INDEXER_POLL_INTERVAL_MS", 5_000),
    port: positiveInteger(env.PORT, "PORT", 3_001),
  };
}
