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
 * The trusted-node set itself lives on-chain in the contract (see
 * `set_trusted_nodes`, admin-only). This wrapper does **not** maintain its
 * own list of trusted signers — a client-side allow-list of secret keys is
 * exactly the anti-pattern this contract exists to avoid. Whether a given
 * signer is actually trusted is enforced by the contract at
 * `propose_pause` / `propose_unpause` time; the SDK's job is only to submit
 * the transaction and surface the contract's own errors.
 *
 * This wrapper provides a thin, promise‑based API that signs and submits
 * transactions using the `@stellar/stellar-sdk`. It keeps the core package
 * lightweight – the heavy cryptographic logic lives in the SDK dependency.
 * Consumers can inject a custom Soroban RPC server URL via the options.
 */

import {
  Address,
  Contract,
  Keypair,
  Networks,
  SorobanRpc,
  TransactionBuilder,
  BASE_FEE,
  scValToNative,
  xdr,
} from '@stellar/stellar-sdk';

export interface CircuitBreakerOptions {
  /**
   * Soroban RPC endpoint (e.g., https://soroban-testnet.stellar.org).
   */
  rpcUrl: string;
  /**
   * The contract ID (address) of the deployed CircuitBreaker contract.
   */
  contractId: string;
  /**
   * Network passphrase to sign transactions for. Defaults to testnet.
   */
  networkPassphrase?: string;
}

/**
 * Helper to load a Keypair from a secret key string.
 */
function loadKeypair(secret: string): Keypair {
  try {
    return Keypair.fromSecret(secret);
  } catch (e) {
    throw new Error('Invalid secret key format');
  }
}

export class CircuitBreaker {
  private rpcServer: SorobanRpc.Server;
  private contract: Contract;
  private networkPassphrase: string;

  constructor(options: CircuitBreakerOptions) {
    this.rpcServer = new SorobanRpc.Server(options.rpcUrl);
    this.contract = new Contract(options.contractId);
    this.networkPassphrase = options.networkPassphrase ?? Networks.TESTNET;
  }

  /**
   * A trusted node records its approval to pause the system.
   * Whether `signerSecretKey` actually belongs to a trusted node is
   * enforced on-chain by the contract — it will reject the call
   * (`"not a trusted node"`) if it isn't.
   *
   * @param signerSecretKey The secret key of the node submitting the proposal.
   */
  async proposePause(signerSecretKey: string): Promise<void> {
    const keypair = loadKeypair(signerSecretKey);
    const nodeAddress = Address.fromString(keypair.publicKey()).toScVal();
    await this.invoke('propose_pause', [nodeAddress], keypair);
  }

  /**
   * Attempt to execute the pause once enough approvals are recorded.
   * Callable by any funded account once quorum is reached on-chain — the
   * security comes from the trusted-node quorum, not from who submits
   * this transaction.
   */
  async executePause(signerSecretKey: string): Promise<void> {
    const keypair = loadKeypair(signerSecretKey);
    await this.invoke('execute_pause', [], keypair);
  }

  /**
   * A trusted node records its approval to unpause the system.
   */
  async proposeUnpause(signerSecretKey: string): Promise<void> {
    const keypair = loadKeypair(signerSecretKey);
    const nodeAddress = Address.fromString(keypair.publicKey()).toScVal();
    await this.invoke('propose_unpause', [nodeAddress], keypair);
  }

  /**
   * Attempt to lift the pause once enough unpause approvals are recorded.
   */
  async unpause(signerSecretKey: string): Promise<void> {
    const keypair = loadKeypair(signerSecretKey);
    await this.invoke('unpause', [], keypair);
  }

  /**
   * Query the contract to see if the system is currently paused.
   *
   * Simulating a Soroban call still requires a source account (for fee /
   * sequence-number bookkeeping) even though nothing is submitted or
   * signed — pass any funded account's public key. Defaults to a
   * throwaway keypair's address, which works for simulation-only reads on
   * most RPC providers; pass `sourcePublicKey` explicitly if yours
   * requires an account that exists on-chain.
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
   * Build, simulate, sign, and submit a contract invocation, then poll for
   * the result. Shared by all state-changing entry points.
   */
  private async invoke(functionName: string, args: xdr.ScVal[], signer: Keypair): Promise<void> {
    const account = await this.rpcServer.getAccount(signer.publicKey());

    const tx = new TransactionBuilder(account, {
      fee: BASE_FEE,
      networkPassphrase: this.networkPassphrase,
    })
      .addOperation(this.contract.call(functionName, ...args))
      .setTimeout(30)
      .build();

    const simulated = await this.rpcServer.simulateTransaction(tx);
    if (SorobanRpc.Api.isSimulationError(simulated)) {
      throw new Error(`${functionName} simulation failed: ${simulated.error}`);
    }

    const prepared = SorobanRpc.assembleTransaction(tx, simulated).build();
    prepared.sign(signer);

    const sendResult = await this.rpcServer.sendTransaction(prepared);
    if (sendResult.status === 'ERROR') {
      throw new Error(`${functionName} submission failed: ${JSON.stringify(sendResult.errorResult)}`);
    }

    await this.pollTransaction(sendResult.hash);
  }

  private async pollTransaction(hash: string): Promise<void> {
    const maxAttempts = 15;
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
