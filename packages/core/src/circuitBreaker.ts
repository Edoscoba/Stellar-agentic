// packages/core/src/circuitBreaker.ts
/**
 * CircuitBreaker SDK wrapper for the Soroban multi‑sig pause contract.
 *
 * The contract lives in `contracts/circuit_breaker` and exposes:
 *   - `propose_pause(node)`   – a trusted node records its approval to pause.
 *   - `execute_pause()`       – flips `is_paused` to true once >=5 distinct
 *                               trusted nodes have called `propose_pause`
 *                               within the contract's validity window.
 *   - `propose_unpause(node)` – a trusted node records its approval to unpause.
 *   - `unpause()`             – flips `is_paused` back to false once quorum
 *                               is reached on unpause proposals.
 *   - `is_paused()`           – view function returning the current pause state.
 *
 * The trusted-node set lives on-chain in the contract (see `set_trusted_nodes`,
 * admin-only). This wrapper does **not** maintain a client-side allow-list of
 * signers — a hardcoded list of secret keys is exactly the anti-pattern this
 * contract exists to avoid. Whether a signer is trusted is enforced by the
 * contract at `propose_pause` / `propose_unpause` time via `require_auth()`.
 */

import {
  Address,
  Contract,
  Keypair,
  Networks,
  Operation,
  SorobanRpc,
  StrKey,
  TransactionBuilder,
  BASE_FEE,
  scValToNative,
  xdr,
} from '@stellar/stellar-sdk';

import { isDeployedAddress } from './contracts.js';
import { StellarAgentError } from './errors.js';
import type { StellarAgentErrorCode } from './errors.js';
import { KeypairSigner, SigningError } from './signer.js';
import type { Signer } from './signer.js';
import type { TxResult } from './types/index.js';

/** Stellar account public key (`G...`). Secret keys (`S...`) are rejected. */
export type PublicAddress = string & { readonly __brand: unique symbol };

/**
 * Parse and validate a Stellar public address. Rejects secret keys so a
 * trusted-node list can never accidentally hold signing material.
 */
export function asPublicAddress(value: string): PublicAddress {
  if (StrKey.isValidEd25519PublicKey(value)) {
    return value as PublicAddress;
  }
  if (value.startsWith('S')) {
    throw new StellarAgentError(
      'INVALID_ARGUMENT',
      'Secret keys must not be used where a public address is expected',
    );
  }
  throw new StellarAgentError(
    'INVALID_ARGUMENT',
    'Expected a valid Stellar public address (G...)',
  );
}

export interface CircuitBreakerOptions {
  /**
   * Soroban RPC endpoint (e.g., https://soroban-testnet.stellar.org).
   * Ignored when `rpc` is provided.
   */
  rpcUrl: string;
  /** The contract ID (address) of the deployed CircuitBreaker contract. */
  contractId: string;
  /** Network passphrase to sign transactions for. Defaults to testnet. */
  networkPassphrase?: string;
  /**
   * Default signer for write methods. When set, callers can omit passing a
   * signer/secret on each call. Prefer {@link Signer} (remote/HSM) over raw
   * secret keys in production.
   */
  signer?: Signer;
  /** Inject a Soroban RPC client (used by unit tests). */
  rpc?: SorobanRpc.Server;
}

function loadKeypair(secret: string): Keypair {
  try {
    return Keypair.fromSecret(secret);
  } catch (error) {
    throw new StellarAgentError('INVALID_ARGUMENT', 'Invalid secret key format', { cause: error });
  }
}

function resolveSigner(input: string | Signer): Signer {
  return typeof input === 'string' ? new KeypairSigner(loadKeypair(input)) : input;
}

function diagnosticText(events: xdr.DiagnosticEvent[] | undefined): string {
  if (!events?.length) return '';
  try {
    return events.map((diagnostic) => {
      const event = diagnostic.event();
      return JSON.stringify({
        topics: event.body().v0().topics().map((topic) => scValToNative(topic)),
        data: scValToNative(event.body().v0().data()),
      }, (_key, value) => typeof value === 'bigint' ? value.toString() : value);
    }).join('; ');
  } catch {
    return events.map((event) => event.toXDR('base64')).join('; ');
  }
}

function contractError(
  fallback: StellarAgentErrorCode,
  message: string,
  transactionHash?: string,
): StellarAgentError {
  const mappings: Array<[RegExp, StellarAgentErrorCode]> = [
    [/not a trusted node/i, 'NOT_AUTHORIZED'],
    [/quorum not reached/i, 'CONTRACT_ERROR'],
    [/not the admin/i, 'NOT_AUTHORIZED'],
    [/not initialized|already initialized/i, 'CONTRACT_ERROR'],
  ];
  const code = mappings.find(([pattern]) => pattern.test(message))?.[1] ?? fallback;
  return new StellarAgentError(code, message, { transactionHash });
}

export class CircuitBreaker {
  readonly contractId: string;
  private readonly defaultSigner?: Signer;
  private rpcServer: SorobanRpc.Server;
  private contract: Contract;
  private networkPassphrase: string;

  constructor(options: CircuitBreakerOptions) {
    if (!isDeployedAddress(options.contractId)) {
      throw new StellarAgentError(
        'INVALID_ARGUMENT',
        `Invalid circuit breaker contract ID: ${options.contractId}`,
      );
    }

    this.contractId = options.contractId;
    this.defaultSigner = options.signer;
    this.rpcServer = options.rpc ?? new SorobanRpc.Server(options.rpcUrl);
    this.contract = new Contract(options.contractId);
    this.networkPassphrase = options.networkPassphrase ?? Networks.TESTNET;
  }

  /** Whether the system is currently paused. */
  async isPaused(sourcePublicKey?: string): Promise<boolean> {
    const value = await this.simulateRead('is_paused', [], sourcePublicKey);
    return value === true;
  }

  /** Distinct trusted-node pause proposals still within the validity window. */
  async pauseQuorumCount(sourcePublicKey?: string): Promise<number> {
    const value = await this.simulateRead('pause_quorum_count', [], sourcePublicKey);
    return Number(value ?? 0);
  }

  /** Distinct trusted-node unpause proposals still within the validity window. */
  async unpauseQuorumCount(sourcePublicKey?: string): Promise<number> {
    const value = await this.simulateRead('unpause_quorum_count', [], sourcePublicKey);
    return Number(value ?? 0);
  }

  /** A trusted node records its approval to pause the system. */
  async proposePause(signer?: string | Signer): Promise<TxResult> {
    const resolved = await this.requireSigner(signer);
    const nodeAddress = Address.fromString(await resolved.getPublicKey()).toScVal();
    return this.invoke('propose_pause', [nodeAddress], resolved);
  }

  /** Execute the pause once enough on-chain proposals have been recorded. */
  async executePause(signer?: string | Signer): Promise<TxResult> {
    return this.invoke('execute_pause', [], await this.requireSigner(signer));
  }

  /** A trusted node records its approval to unpause the system. */
  async proposeUnpause(signer?: string | Signer): Promise<TxResult> {
    const resolved = await this.requireSigner(signer);
    const nodeAddress = Address.fromString(await resolved.getPublicKey()).toScVal();
    return this.invoke('propose_unpause', [nodeAddress], resolved);
  }

  /** Lift the pause once enough on-chain unpause proposals have been recorded. */
  async unpause(signer?: string | Signer): Promise<TxResult> {
    return this.invoke('unpause', [], await this.requireSigner(signer));
  }

  private async requireSigner(signer?: string | Signer): Promise<Signer> {
    if (signer !== undefined) {
      return resolveSigner(signer);
    }
    if (this.defaultSigner) {
      return this.defaultSigner;
    }
    throw new StellarAgentError(
      'INVALID_ARGUMENT',
      'A signer or secret key is required — pass one to the method or set options.signer',
    );
  }

  private async simulateRead(
    method: string,
    args: xdr.ScVal[],
    sourcePublicKey?: string,
  ): Promise<unknown> {
    try {
      const publicKey = sourcePublicKey
        ? asPublicAddress(sourcePublicKey)
        : Keypair.random().publicKey();
      const account = await this.rpcServer.getAccount(publicKey);

      const tx = new TransactionBuilder(account, {
        fee: BASE_FEE,
        networkPassphrase: this.networkPassphrase,
      })
        .addOperation(this.contract.call(method, ...args))
        .setTimeout(30)
        .build();

      const simulated = await this.rpcServer.simulateTransaction(tx);
      if (SorobanRpc.Api.isSimulationError(simulated)) {
        throw contractError(
          'SIMULATION_FAILED',
          `${method} simulation failed: ${simulated.error}`,
        );
      }
      if (SorobanRpc.Api.isSimulationRestore(simulated)) {
        throw new StellarAgentError(
          'SIMULATION_FAILED',
          `${method} requires restoring expired ledger entries before invocation`,
        );
      }

      return simulated.result?.retval ? scValToNative(simulated.result.retval) : undefined;
    } catch (error) {
      if (error instanceof StellarAgentError) throw error;
      throw new StellarAgentError(
        'NETWORK_ERROR',
        `${method} failed while communicating with Soroban RPC: ${
          error instanceof Error ? error.message : String(error)
        }`,
        { cause: error },
      );
    }
  }

  private async invoke(
    functionName: string,
    args: xdr.ScVal[],
    signer: Signer,
  ): Promise<TxResult> {
    try {
      const publicKey = await signer.getPublicKey();
      const account = await this.rpcServer.getAccount(publicKey);

      const operation = this.contract.call(functionName, ...args);
      const tx = new TransactionBuilder(account, {
        fee: BASE_FEE,
        networkPassphrase: this.networkPassphrase,
      })
        .addOperation(operation)
        .setTimeout(30)
        .build();

      const simulated = await this.rpcServer.simulateTransaction(tx);
      if (SorobanRpc.Api.isSimulationError(simulated)) {
        throw contractError(
          'SIMULATION_FAILED',
          `${functionName} simulation failed: ${simulated.error}`,
        );
      }
      if (SorobanRpc.Api.isSimulationRestore(simulated)) {
        throw new StellarAgentError(
          'SIMULATION_FAILED',
          `${functionName} requires restoring expired ledger entries before invocation`,
        );
      }

      const validUntilLedgerSeq = simulated.latestLedger + 100;
      const auth = await Promise.all((simulated.result?.auth ?? []).map(async (entry) => {
        if (entry.credentials().switch().name !== 'sorobanCredentialsAddress') {
          return entry;
        }
        const signedXdr = await signer.signAuthEntry(entry.toXDR('base64'), {
          networkPassphrase: this.networkPassphrase,
          validUntilLedgerSeq,
        });
        return xdr.SorobanAuthorizationEntry.fromXDR(signedXdr, 'base64');
      }));

      const hostFunction = operation.body().invokeHostFunctionOp().hostFunction();
      const authorizedOperation = Operation.invokeHostFunction({ func: hostFunction, auth });
      const authorizedTx = TransactionBuilder.cloneFrom(tx)
        .clearOperations()
        .addOperation(authorizedOperation)
        .build();

      const assembled = SorobanRpc.assembleTransaction(authorizedTx, simulated).build();
      const signedXdr = await signer.signTransaction(assembled.toXDR(), {
        networkPassphrase: this.networkPassphrase,
      });
      const signed = TransactionBuilder.fromXDR(signedXdr, this.networkPassphrase);

      const submitted = await this.rpcServer.sendTransaction(signed);
      if (submitted.status !== 'PENDING' && submitted.status !== 'DUPLICATE') {
        const diagnostics = diagnosticText(submitted.diagnosticEvents);
        throw contractError(
          'SUBMISSION_FAILED',
          `${functionName} submission failed (${submitted.status}): ${
            diagnostics || submitted.errorResult?.toXDR('base64') || 'unknown error'
          }`,
        );
      }

      return await this.pollTransaction(submitted.hash, functionName);
    } catch (error) {
      if (error instanceof StellarAgentError || error instanceof SigningError) throw error;
      throw new StellarAgentError(
        'NETWORK_ERROR',
        `${functionName} failed while communicating with Soroban RPC: ${
          error instanceof Error ? error.message : String(error)
        }`,
        { cause: error },
      );
    }
  }

  private async pollTransaction(hash: string, functionName: string): Promise<TxResult> {
    for (let attempt = 0; attempt < 30; attempt++) {
      const result = await this.rpcServer.getTransaction(hash);
      if (result.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS) {
        return { hash, success: true, ledger: result.ledger };
      }
      if (result.status === SorobanRpc.Api.GetTransactionStatus.FAILED) {
        const diagnostics = diagnosticText(result.diagnosticEventsXdr);
        throw contractError(
          'TRANSACTION_FAILED',
          `${functionName} transaction failed${diagnostics ? `: ${diagnostics}` : ''}`,
          hash,
        );
      }
      await new Promise((resolve) => setTimeout(resolve, 1000));
    }
    throw new StellarAgentError(
      'TRANSACTION_TIMEOUT',
      `${functionName} transaction did not complete in time`,
      { transactionHash: hash },
    );
  }
}
