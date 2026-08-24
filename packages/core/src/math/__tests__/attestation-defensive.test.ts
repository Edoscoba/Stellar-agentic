/**
 * The rejection paths `verifyBidAttestation` keeps for attestations that no
 * honest scorer would produce: a forged or corrupted header, and a ranking
 * that cannot be reproduced from the bids it claims to cover.
 *
 * These are exactly the checks that only ever fire against a hostile or
 * broken counterparty, so nothing in the ordinary attest → verify round trip
 * reaches them — `rankBids` is stubbed here (passing through to the real
 * implementation by default) to drive the recomputation disagreements a
 * cheating scorer would cause.
 */
import { describe, it, expect, vi } from 'vitest';
import { Keypair } from '@stellar/stellar-sdk';

vi.mock('../bid.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../bid.js')>();
  return { ...actual, rankBids: vi.fn(actual.rankBids) };
});

import {
  attestRankBids,
  verifyBidAttestation,
  type BidAttestation,
  type ScorerKeyDirectory,
} from '../attestation.js';
import { rankBids, DEFAULT_BID_WEIGHTS, type AgentBid, type ScoredBid } from '../bid.js';

const FIXED_NOW = 1_700_000_000_000; // ms
const FIXED_NOW_SECONDS = Math.floor(FIXED_NOW / 1000);

function makeBid(overrides: Partial<AgentBid> = {}): AgentBid {
  return {
    agentAddress: 'GAAA',
    price: '1',
    reputation: '50',
    estimatedLatencySeconds: '10',
    successRate: '0.5',
    ...overrides,
  };
}

function makeBids(): AgentBid[] {
  return [
    makeBid({ agentAddress: 'GMEDIOCRE', price: '5', reputation: '50', estimatedLatencySeconds: '5', successRate: '0.5' }),
    makeBid({ agentAddress: 'GBEST', price: '0', reputation: '100', estimatedLatencySeconds: '0', successRate: '1' }),
  ];
}

/** A genuine attestation over `bids`, stamped with the fixed clock. */
function attest(scorer: Keypair, bids: AgentBid[] = makeBids()) {
  return attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
    keyEpoch: 0,
    now: () => FIXED_NOW,
  });
}

function directoryFor(keypair: Keypair, extra: Partial<ScorerKeyDirectory[number]> = {}): ScorerKeyDirectory {
  return [{ epoch: 0, publicKey: keypair.publicKey(), ...extra }];
}

function verify(
  bids: AgentBid[],
  result: ScoredBid[],
  attestation: BidAttestation,
  directory: ScorerKeyDirectory,
) {
  return verifyBidAttestation(bids, result, attestation, directory, { now: () => FIXED_NOW });
}

describe('verifyBidAttestation — malformed attestation header', () => {
  it('rejects an attestation from a schema version it does not understand', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    const verdict = verify(
      bids,
      result,
      { ...attestation, version: 2 } as unknown as BidAttestation,
      directoryFor(scorer),
    );
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/unsupported attestation version: 2/);
  });

  it('rejects an attestation issued before its key epoch became valid', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    const verdict = verify(
      bids,
      result,
      attestation,
      directoryFor(scorer, { validFrom: FIXED_NOW_SECONDS + 60 }),
    );
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/not yet valid/);
  });

  it('accepts an attestation issued after its key epoch became valid', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    const verdict = verify(
      bids,
      result,
      attestation,
      directoryFor(scorer, { validFrom: FIXED_NOW_SECONDS - 60 }),
    );
    expect(verdict.valid).toBe(true);
  });

  it('rejects a scorerPublicKey that is not a Stellar address at all', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    // Trusted directory agrees with the (garbage) key, so the mismatch check
    // above passes and decoding it is what fails.
    const verdict = verify(
      bids,
      result,
      { ...attestation, scorerPublicKey: 'not-a-stellar-key' },
      [{ epoch: 0, publicKey: 'not-a-stellar-key' }],
    );
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/not a valid Stellar public key/);
  });

  it('rejects a signature field that is not decodable as base64', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    // Deserialized JSON can carry anything in this field, whatever the type says.
    const verdict = verify(
      bids,
      result,
      { ...attestation, signature: undefined as unknown as string },
      directoryFor(scorer),
    );
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/not valid base64/);
  });

  it('treats a signature check that throws as a failed signature, not a crash', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    const verifySpy = vi.spyOn(Keypair.prototype, 'verify').mockImplementation(() => {
      throw new Error('ed25519 backend exploded');
    });
    try {
      const verdict = verify(bids, result, attestation, directoryFor(scorer));
      expect(verdict.valid).toBe(false);
      if (!verdict.valid) expect(verdict.reason).toMatch(/signature is invalid/);
    } finally {
      verifySpy.mockRestore();
    }
  });
});

describe('verifyBidAttestation — unreproducible ranking', () => {
  it('rejects when recomputing the ranking throws', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    vi.mocked(rankBids).mockImplementationOnce(() => {
      throw new RangeError('BidScorer: weights must sum to 1.0, got 0.9000');
    });

    const verdict = verify(bids, result, attestation, directoryFor(scorer));
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/recomputation failed: BidScorer/);
  });

  it('rejects when the recomputed ranking disagrees with the attested result', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    // Same bids, same length, different winner — what a scorer that scored one
    // way and reported another would look like from the verifier's side.
    vi.mocked(rankBids).mockImplementationOnce(() => [...result].reverse());

    const verdict = verify(bids, result, attestation, directoryFor(scorer));
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/recomputed ranking does not match/);
  });

  it('rejects when the recomputed ranking drops a bid entirely', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attest(scorer, bids);

    vi.mocked(rankBids).mockImplementationOnce(() => result.slice(0, 1));

    const verdict = verify(bids, result, attestation, directoryFor(scorer));
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/recomputed ranking does not match/);
  });
});

describe('attestation clock and digest edge cases', () => {
  it('falls back to the system clock when no clock is injected', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const before = Math.floor(Date.now() / 1000);

    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
    });
    expect(attestation.issuedAt).toBeGreaterThanOrEqual(before);
    expect(attestation.expiresAt).toBeGreaterThan(attestation.issuedAt);

    // Verified with the default clock too — inside the default ttl.
    const verdict = verifyBidAttestation(bids, result, attestation, directoryFor(scorer));
    expect(verdict.valid).toBe(true);
  });

  it('digests a bid set containing repeated agent addresses', () => {
    const scorer = Keypair.random();
    // Two entries sharing an address exercise the "equal" case of the
    // digest's address sort, where neither bid orders before the other.
    const bids = [
      makeBid({ agentAddress: 'GSAME', price: '1' }),
      makeBid({ agentAddress: 'GSAME', price: '2' }),
      makeBid({ agentAddress: 'GOTHER', price: '3' }),
    ];
    const { result, attestation } = attest(scorer, bids);

    expect(verify(bids, result, attestation, directoryFor(scorer)).valid).toBe(true);
  });
});
