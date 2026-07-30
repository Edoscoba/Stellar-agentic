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
 *
 * Consumers pass a secret key only to sign transactions they submit; trusted
 * node membership is never checked client-side.
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

import { KeypairSigner } from './signer.js';

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
    throw new Error('Secret keys must not be used where a public address is expected');
  }
  throw new Error('Expected a valid Stellar public address (G...)');
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
  /** Inject a Soroban RPC client (used by unit tests). */
  rpc?: SorobanRpc.Server;
}

function loadKeypair(secret: string): Keypair {
  try {
    return Keypair.fromSecret(secret);
  } catch {
    throw new Error('Invalid secret key format');
  }
}

export class CircuitBreaker {
  readonly contractId: string;
  private rpcServer: SorobanRpc.Server;
  private contract: Contract;
  private networkPassphrase: string;

  constructor(options: CircuitBreakerOptions) {
    this.contractId = options.contractId;
    this.rpcServer = options.rpc ?? new SorobanRpc.Server(options.rpcUrl);
    this.contract = new Contract(options.contractId);
    this.networkPassphrase = options.networkPassphrase ?? Networks.TESTNET;
  }

  /**
   * A trusted node records its approval to pause the system.
   * Whether `signerSecretKey` belongs to a trusted node is enforced on-chain.
   */
  async proposePause(signerSecretKey: string): Promise<void> {
    const keypair = loadKeypair(signerSecretKey);
    const nodeAddress = Address.fromString(keypair.publicKey()).toScVal();
    await this.invoke('propose_pause', [nodeAddress], keypair);
  }

  /** Execute the pause once enough on-chain proposals have been recorded. */
  async executePause(signerSecretKey: string): Promise<void> {
    await this.invoke('execute_pause', [], loadKeypair(signerSecretKey));
  }

  /** A trusted node records its approval to unpause the system. */
  async proposeUnpause(signerSecretKey: string): Promise<void> {
    const keypair = loadKeypair(signerSecretKey);
    const nodeAddress = Address.fromString(keypair.publicKey()).toScVal();
    await this.invoke('propose_unpause', [nodeAddress], keypair);
  }

  /** Lift the pause once enough on-chain unpause proposals have been recorded. */
  async unpause(signerSecretKey: string): Promise<void> {
    await this.invoke('unpause', [], loadKeypair(signerSecretKey));
  }

  /**
   * Query the contract to see if the system is currently paused.
   *
   * Simulation still requires a source account for fee bookkeeping — pass any
   * funded account's public key, or rely on the default throwaway keypair
   * address if your RPC accepts simulation-only reads without a live account.
   */
  async isPaused(sourcePublicKey?: string): Promise<boolean> {
    const publicKey = sourcePublicKey ?? Keypair.random().publicKey();
    const account = await this.rpcServer.getAccount(publicKey);

    const tx = new TransactionBuilder(account, {
      fee: BASE_FEE,
      networkPassphrase: this.networkPassphrase,
    })
      .addOperation(this.contract.call('is_paused'))
      .setTimeout(30)
      .build();

    const simulated = await this.rpcServer.simulateTransaction(tx);
    if (SorobanRpc.Api.isSimulationError(simulated)) {
      throw new Error(`is_paused simulation failed: ${simulated.error}`);
    }

    const retval = simulated.result?.retval;
    if (!retval) {
      return false;
    }
    return scValToNative(retval) === true;
  }

  /**
   * Build, simulate, sign auth entries + envelope, submit, and poll.
   * Matches the Soroban invocation pipeline used by {@link StellarAgent}.
   */
  private async invoke(functionName: string, args: xdr.ScVal[], signer: Keypair): Promise<void> {
    const keypairSigner = new KeypairSigner(signer);
    const account = await this.rpcServer.getAccount(signer.publicKey());

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
      throw new Error(`${functionName} simulation failed: ${simulated.error}`);
    }
    if (SorobanRpc.Api.isSimulationRestore(simulated)) {
      throw new Error(`${functionName} requires restoring expired ledger entries before invocation`);
    }

    const validUntilLedgerSeq = simulated.latestLedger + 100;
    const auth = await Promise.all((simulated.result?.auth ?? []).map(async (entry) => {
      if (entry.credentials().switch().name !== 'sorobanCredentialsAddress') {
        return entry;
      }
      const signedXdr = await keypairSigner.signAuthEntry(entry.toXDR('base64'), {
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
    const signedXdr = await keypairSigner.signTransaction(assembled.toXDR(), {
      networkPassphrase: this.networkPassphrase,
    });
    const signed = TransactionBuilder.fromXDR(signedXdr, this.networkPassphrase);

    const sendResult = await this.rpcServer.sendTransaction(signed);
    if (sendResult.status !== 'PENDING' && sendResult.status !== 'DUPLICATE') {
      throw new Error(
        `${functionName} submission failed (${sendResult.status}): ${
          sendResult.errorResult?.toXDR('base64') ?? 'unknown error'
        }`,
      );
    }

    await this.pollTransaction(sendResult.hash);
  }

  private async pollTransaction(hash: string): Promise<void> {
    const maxAttempts = 30;
    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      const result = await this.rpcServer.getTransaction(hash);
      if (result.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS) {
        return;
      }
      if (result.status === SorobanRpc.Api.GetTransactionStatus.FAILED) {
        throw new Error(`Transaction ${hash} failed`);
      }
      await new Promise((resolve) => setTimeout(resolve, 1000));
    }
    throw new Error(`Transaction ${hash} did not complete in time`);
  }
}
