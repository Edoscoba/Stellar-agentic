#!/usr/bin/env tsx
/**
 * Deploy, initialize and cross-wire every StellarAgent Soroban contract.
 *
 * ## Why this exists
 *
 * `CONTRIBUTING.md` used to document deployment as a single line:
 *
 * ```
 * stellar contract deploy --wasm target/.../agent_wallet_factory.wasm --network testnet
 * ```
 *
 * That is not a deployment. There are seven contracts, four of them need a
 * one-time `initialize` call, and three of them hold addresses of the others
 * that must be set *after* every contract exists. Doing this by hand in the
 * wrong order leaves a half-wired system whose failures surface much later,
 * as opaque RPC errors from inside a payment.
 *
 * ## What it does
 *
 *   1. Builds all WASMs (`cargo build --target wasm32-unknown-unknown --release`)
 *   2. Deploys all seven contracts
 *   3. Initializes the four that take an `initialize` entrypoint
 *   4. Cross-wires the references between them
 *   5. Writes `deployments/<network>.json` and prints a matching `.env` block
 *
 * ## Usage
 *
 * ```bash
 * pnpm deploy:contracts --network local  --source alice
 * pnpm deploy:contracts --network testnet --source alice --trusted-nodes G...,G...
 * pnpm deploy:contracts --network testnet --source alice --dry-run
 * ```
 *
 * `--source` is anything the Stellar CLI accepts for `--source-account`: a
 * saved identity name, a public key, or a secret key. Prefer an identity —
 * a secret on the command line lands in your shell history.
 */

import { execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync, existsSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

// ─── Repo layout ─────────────────────────────────────────────────────────────

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const CONTRACTS_DIR = resolve(REPO_ROOT, 'contracts');
const WASM_DIR = resolve(CONTRACTS_DIR, 'target/wasm32-unknown-unknown/release');
const DEPLOYMENTS_DIR = resolve(REPO_ROOT, 'deployments');

// ─── Contract inventory ──────────────────────────────────────────────────────

/**
 * Every deployable crate, in deployment order.
 *
 * `sdkKey` maps a crate onto the `ContractAddresses` field the SDK reads.
 * `price_oracle` and `amm_swap` have no SDK field — they are referenced only
 * by `payment_channel` — but they still have to be deployed and initialized,
 * which is exactly the step a single-contract runbook misses.
 */
const CONTRACTS = [
  { crate: 'agent_wallet_factory', sdkKey: 'agentWalletFactory' },
  { crate: 'payment_channel', sdkKey: 'paymentChannel' },
  { crate: 'escrow', sdkKey: 'escrow' },
  { crate: 'rate_limiter', sdkKey: 'rateLimiter' },
  { crate: 'circuit_breaker', sdkKey: 'circuitBreaker' },
  { crate: 'price_oracle', sdkKey: null },
  { crate: 'amm_swap', sdkKey: null },
] as const;

type Crate = (typeof CONTRACTS)[number]['crate'];
type Addresses = Record<Crate, string>;

/** Matches `QUORUM` in contracts/circuit_breaker/src/lib.rs. */
const CIRCUIT_BREAKER_QUORUM = 5;

/** Proposal validity window, in ledgers (~5s each on testnet ≈ 1 hour). */
const DEFAULT_PROPOSE_WINDOW_LEDGERS = 720;

// ─── Argument parsing ────────────────────────────────────────────────────────

interface Options {
  network: string;
  source: string;
  admin?: string;
  trustedNodes: string[];
  proposeWindow: number;
  out?: string;
  skipBuild: boolean;
  dryRun: boolean;
}

function parseArgs(argv: string[]): Options {
  const opts: Options = {
    network: 'local',
    source: '',
    trustedNodes: [],
    proposeWindow: DEFAULT_PROPOSE_WINDOW_LEDGERS,
    skipBuild: false,
    dryRun: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]!;
    const next = () => {
      const value = argv[++i];
      if (value === undefined) fail(`${arg} requires a value`);
      return value!;
    };
    switch (arg) {
      case '--network': opts.network = next(); break;
      case '--source': case '--source-account': opts.source = next(); break;
      case '--admin': opts.admin = next(); break;
      case '--trusted-nodes': opts.trustedNodes = next().split(',').map((s) => s.trim()).filter(Boolean); break;
      case '--propose-window': opts.proposeWindow = Number(next()); break;
      case '--out': opts.out = next(); break;
      case '--skip-build': opts.skipBuild = true; break;
      case '--dry-run': opts.dryRun = true; break;
      case '--help': case '-h': usage(); process.exit(0); break;
      default: fail(`Unknown argument: ${arg}`);
    }
  }

  if (!opts.source) fail('--source is required (a Stellar CLI identity, public key, or secret key)');
  if (!Number.isInteger(opts.proposeWindow) || opts.proposeWindow <= 0) {
    fail('--propose-window must be a positive integer number of ledgers');
  }
  return opts;
}

function usage(): void {
  console.log(`
Deploy and wire all StellarAgent Soroban contracts.

  --network <name>          Target network (default: local)
  --source <identity>       Stellar CLI identity / public key / secret key  [required]
  --admin <G...>            Admin address (default: resolved from --source)
  --trusted-nodes <G,...>   Circuit-breaker trusted nodes (comma-separated)
  --propose-window <n>      Circuit-breaker proposal window in ledgers (default: ${DEFAULT_PROPOSE_WINDOW_LEDGERS})
  --out <path>              Output file (default: deployments/<network>.json)
  --skip-build              Reuse existing WASM artifacts
  --dry-run                 Print every command without executing it
  -h, --help                Show this message
`.trim());
}

function fail(message: string): never {
  console.error(`\n  error: ${message}\n`);
  process.exit(1);
}

// ─── Shell helpers ───────────────────────────────────────────────────────────

let DRY_RUN = false;

/** Run a command, echoing it. Returns trimmed stdout. */
function run(cmd: string, args: string[], cwd = REPO_ROOT): string {
  console.log(`  $ ${cmd} ${args.join(' ')}`);
  if (DRY_RUN) return `<dry-run:${cmd}>`;
  try {
    return execFileSync(cmd, args, { cwd, encoding: 'utf8', stdio: ['inherit', 'pipe', 'inherit'] }).trim();
  } catch (err) {
    const e = err as { status?: number; message: string };
    fail(`command failed (exit ${e.status ?? '?'}): ${cmd} ${args.join(' ')}`);
  }
}

/**
 * Run a contract invocation that is safe to repeat.
 *
 * `initialize` panics with "already initialized" on a redeploy against
 * existing state. That is a successful outcome for this script's purposes —
 * the contract is in the state we want — so it is reported and swallowed
 * rather than aborting a nine-step deployment at step four.
 */
function runIdempotent(cmd: string, args: string[], label: string): boolean {
  console.log(`  $ ${cmd} ${args.join(' ')}`);
  if (DRY_RUN) return true;
  try {
    execFileSync(cmd, args, { cwd: REPO_ROOT, encoding: 'utf8', stdio: ['inherit', 'pipe', 'pipe'] });
    return true;
  } catch (err) {
    const e = err as { stderr?: string; stdout?: string; status?: number };
    const output = `${e.stderr ?? ''}${e.stdout ?? ''}`;
    if (/already initialized/i.test(output)) {
      console.log(`    → already initialized, skipping (${label})`);
      return false;
    }
    console.error(output);
    fail(`${label} failed (exit ${e.status ?? '?'})`);
  }
}

function heading(step: number, total: number, title: string): void {
  console.log(`\n[${step}/${total}] ${title}`);
}

// ─── Steps ───────────────────────────────────────────────────────────────────

function buildWasms(): void {
  run('cargo', ['build', '--target', 'wasm32-unknown-unknown', '--release'], CONTRACTS_DIR);
}

function deployAll(opts: Options): Addresses {
  const addresses = {} as Addresses;
  for (const { crate } of CONTRACTS) {
    const wasm = resolve(WASM_DIR, `${crate}.wasm`);
    if (!DRY_RUN && !existsSync(wasm)) {
      fail(`WASM not found: ${wasm}\n         Run without --skip-build, or build the contracts first.`);
    }
    const id = run('stellar', [
      'contract', 'deploy',
      '--wasm', wasm,
      '--source-account', opts.source,
      '--network', opts.network,
    ]);
    addresses[crate] = id;
    console.log(`    → ${crate} = ${id}`);
  }
  return addresses;
}

/** `stellar contract invoke --id X -- fn --arg value` */
function invoke(opts: Options, contractId: string, fn: string, args: string[], label: string): boolean {
  return runIdempotent('stellar', [
    'contract', 'invoke',
    '--id', contractId,
    '--source-account', opts.source,
    '--network', opts.network,
    '--', fn, ...args,
  ], label);
}

/**
 * One-time initialization.
 *
 * Order matters: `agent_wallet_factory.initialize(admin)` must land before
 * anything references the factory, and the circuit breaker must hold its
 * trusted-node set before `payment_channel`/`escrow` are pointed at it —
 * otherwise a pause could never reach quorum against a contract those two
 * already trust.
 */
function initializeAll(opts: Options, addr: Addresses, admin: string): void {
  invoke(opts, addr.agent_wallet_factory, 'initialize', ['--admin', admin], 'agent_wallet_factory.initialize');

  if (opts.trustedNodes.length > 0 && opts.trustedNodes.length < CIRCUIT_BREAKER_QUORUM) {
    console.warn(
      `    ! only ${opts.trustedNodes.length} trusted node(s) supplied, but the circuit\n` +
      `      breaker's QUORUM is ${CIRCUIT_BREAKER_QUORUM}. Pausing will be impossible until\n` +
      `      set_trusted_nodes is called with at least ${CIRCUIT_BREAKER_QUORUM} addresses.`,
    );
  }
  invoke(opts, addr.circuit_breaker, 'initialize', [
    '--admin', admin,
    '--trusted_nodes', JSON.stringify(opts.trustedNodes),
    '--propose_window_ledgers', String(opts.proposeWindow),
  ], 'circuit_breaker.initialize');

  invoke(opts, addr.price_oracle, 'initialize', ['--admin', admin], 'price_oracle.initialize');
  invoke(opts, addr.amm_swap, 'initialize', ['--admin', admin], 'amm_swap.initialize');
}

/**
 * Cross-wiring — only valid once every contract above exists.
 *
 * `payment_channel.pay_with_conversion` reads the price oracle and the AMM,
 * and both `payment_channel` and `escrow` consult the circuit breaker before
 * moving funds. Skipping any of these leaves a contract that silently
 * behaves as though the emergency pause does not exist.
 */
function wireAll(opts: Options, addr: Addresses, admin: string): void {
  invoke(opts, addr.payment_channel, 'set_circuit_breaker',
    ['--admin', admin, '--circuit_breaker', addr.circuit_breaker], 'payment_channel.set_circuit_breaker');
  invoke(opts, addr.payment_channel, 'set_price_oracle',
    ['--admin', admin, '--price_oracle', addr.price_oracle], 'payment_channel.set_price_oracle');
  invoke(opts, addr.payment_channel, 'set_amm',
    ['--admin', admin, '--amm', addr.amm_swap], 'payment_channel.set_amm');
  invoke(opts, addr.escrow, 'set_circuit_breaker',
    ['--admin', admin, '--circuit_breaker', addr.circuit_breaker], 'escrow.set_circuit_breaker');
}

// ─── Output ──────────────────────────────────────────────────────────────────

interface Deployment {
  network: string;
  admin: string;
  deployedAt: string;
  /** Addresses keyed by `ContractAddresses` field — what the SDK consumes. */
  contracts: Record<string, string>;
  /** Every deployed contract keyed by crate name, including the unmapped ones. */
  crates: Record<string, string>;
  circuitBreaker: { trustedNodes: string[]; proposeWindowLedgers: number };
}

function buildDeployment(opts: Options, addr: Addresses, admin: string): Deployment {
  const contracts: Record<string, string> = {};
  for (const { crate, sdkKey } of CONTRACTS) {
    if (sdkKey) contracts[sdkKey] = addr[crate];
  }
  return {
    network: opts.network,
    admin,
    deployedAt: new Date().toISOString(),
    contracts,
    crates: Object.fromEntries(CONTRACTS.map(({ crate }) => [crate, addr[crate]])),
    circuitBreaker: {
      trustedNodes: opts.trustedNodes,
      proposeWindowLedgers: opts.proposeWindow,
    },
  };
}

function writeDeployment(opts: Options, deployment: Deployment): string {
  const outPath = opts.out
    ? resolve(REPO_ROOT, opts.out)
    : resolve(DEPLOYMENTS_DIR, `${opts.network}.json`);
  if (DRY_RUN) {
    console.log(`  (dry run — would write ${outPath})`);
    return outPath;
  }
  mkdirSync(dirname(outPath), { recursive: true });
  writeFileSync(outPath, `${JSON.stringify(deployment, null, 2)}\n`);
  return outPath;
}

/** camelCase SDK key → the env var `packages/core/src/contracts.ts` reads. */
function envVarName(network: string, sdkKey: string): string {
  const suffix = sdkKey.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toUpperCase();
  return `STELLARAGENT_${network.toUpperCase()}_${suffix}`;
}

function printEnvBlock(deployment: Deployment): void {
  console.log('\n  Add these to your environment (or a .env file):\n');
  for (const [sdkKey, address] of Object.entries(deployment.contracts)) {
    console.log(`    ${envVarName(deployment.network, sdkKey)}=${address}`);
  }
}

// ─── Main ────────────────────────────────────────────────────────────────────

function main(): void {
  const opts = parseArgs(process.argv.slice(2));
  DRY_RUN = opts.dryRun;

  const totalSteps = opts.skipBuild ? 4 : 5;
  let step = 0;

  console.log(`\nDeploying StellarAgent contracts to "${opts.network}"`);
  if (DRY_RUN) console.log('(dry run — no commands will be executed)');

  if (!opts.skipBuild) {
    heading(++step, totalSteps, 'Building contract WASMs');
    buildWasms();
  } else {
    console.log('\n  (skipping build, reusing existing WASM artifacts)');
  }

  heading(++step, totalSteps, `Deploying ${CONTRACTS.length} contracts`);
  const addresses = deployAll(opts);

  // The admin defaults to whatever `--source` resolves to, so the account
  // paying for the deployment is the one that can administer the result.
  const admin = opts.admin
    ?? run('stellar', ['keys', 'public-key', opts.source]).split('\n').pop()!.trim();

  heading(++step, totalSteps, `Initializing contracts (admin: ${admin})`);
  initializeAll(opts, addresses, admin);

  heading(++step, totalSteps, 'Cross-wiring contract references');
  wireAll(opts, addresses, admin);

  heading(++step, totalSteps, 'Writing deployment config');
  const deployment = buildDeployment(opts, addresses, admin);
  const outPath = writeDeployment(opts, deployment);
  console.log(`  → ${outPath}`);
  printEnvBlock(deployment);

  console.log('\nDone.\n');
}

main();
