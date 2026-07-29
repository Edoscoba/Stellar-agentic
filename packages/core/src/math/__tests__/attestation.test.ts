import { describe, it, expect } from 'vitest';
import { Keypair } from '@stellar/stellar-sdk';

import {
  attestRankBids,
  verifyBidAttestation,
  type ScorerKeyDirectory,
} from '../attestation.js';
import { rankBids, DEFAULT_BID_WEIGHTS, type AgentBid } from '../bid.js';

// ─── Fixtures ────────────────────────────────────────────────────────────────

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
    makeBid({ agentAddress: 'GWORST', price: '10', reputation: '0', estimatedLatencySeconds: '10', successRate: '0' }),
  ];
}

const FIXED_NOW = 1_700_000_000_000; // ms

function directoryFor(keypair: Keypair, epoch = 0): ScorerKeyDirectory {
  return [{ epoch, publicKey: keypair.publicKey() }];
}

// ─── attestRankBids — input validation ───────────────────────────────────────

describe('attestRankBids — validation', () => {
  it('throws when the scorer keypair has no secret', () => {
    const pubOnly = Keypair.fromPublicKey(Keypair.random().publicKey());
    expect(() =>
      attestRankBids(makeBids(), DEFAULT_BID_WEIGHTS, pubOnly, { keyEpoch: 0 }),
    ).toThrow(RangeError);
  });

  it('throws on a negative or non-integer keyEpoch', () => {
    const scorer = Keypair.random();
    expect(() => attestRankBids(makeBids(), DEFAULT_BID_WEIGHTS, scorer, { keyEpoch: -1 })).toThrow(RangeError);
    expect(() => attestRankBids(makeBids(), DEFAULT_BID_WEIGHTS, scorer, { keyEpoch: 1.5 })).toThrow(RangeError);
  });

  it('throws on a non-positive ttlSeconds', () => {
    const scorer = Keypair.random();
    expect(() =>
      attestRankBids(makeBids(), DEFAULT_BID_WEIGHTS, scorer, { keyEpoch: 0, ttlSeconds: 0 }),
    ).toThrow(RangeError);
  });

  it('propagates rankBids weight-validation errors', () => {
    const scorer = Keypair.random();
    expect(() =>
      attestRankBids(makeBids(), { price: '1', reputation: '1', latency: '1', reliability: '1' }, scorer, {
        keyEpoch: 0,
      }),
    ).toThrow(RangeError);
  });
});

// ─── attestRankBids — output shape ───────────────────────────────────────────

describe('attestRankBids — output', () => {
  it('produces the same result as a plain rankBids call', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, { keyEpoch: 0, now: () => FIXED_NOW });
    expect(result).toEqual(rankBids(bids, DEFAULT_BID_WEIGHTS));
  });

  it('stamps issuedAt/expiresAt from the injected clock and ttl', () => {
    const scorer = Keypair.random();
    const { attestation } = attestRankBids(makeBids(), DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 3,
      ttlSeconds: 60,
      now: () => FIXED_NOW,
    });
    expect(attestation.issuedAt).toBe(Math.floor(FIXED_NOW / 1000));
    expect(attestation.expiresAt).toBe(Math.floor(FIXED_NOW / 1000) + 60);
    expect(attestation.keyEpoch).toBe(3);
    expect(attestation.scorerPublicKey).toBe(scorer.publicKey());
  });
});

// ─── Verification — happy path ────────────────────────────────────────────────

describe('verifyBidAttestation — valid attestation', () => {
  it('verifies a genuine attestation using only exported functions', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const verdict = verifyBidAttestation(bids, result, attestation, directoryFor(scorer), {
      now: () => FIXED_NOW + 1000, // still within the 300s default ttl
    });

    expect(verdict.valid).toBe(true);
    if (verdict.valid) {
      expect(verdict.recomputed).toEqual(result);
    }
  });

  it('is independent of the order bids are supplied to the verifier in', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const shuffled = [...bids].reverse();
    const verdict = verifyBidAttestation(shuffled, result, attestation, directoryFor(scorer), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(true);
  });

  it('rejects a genuine attestation once expired', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      ttlSeconds: 60,
      now: () => FIXED_NOW,
    });

    const verdict = verifyBidAttestation(bids, result, attestation, directoryFor(scorer), {
      now: () => FIXED_NOW + 61_000,
    });
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/expired/);
  });
});

// ─── Verification — tampered output ──────────────────────────────────────────

describe('verifyBidAttestation — tampered output', () => {
  it('rejects when the reported winner differs from what was actually computed', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    // The scorer (or a MITM) swaps in a fabricated result — e.g. claiming the
    // worst bid actually won.
    const fabricated = [...result].reverse();

    const verdict = verifyBidAttestation(bids, fabricated, attestation, directoryFor(scorer), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(false);
  });

  it('rejects a single mutated score even when array order is preserved', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const tampered = result.map((r, i) => (i === 0 ? { ...r, score: '999.9999' } : r));

    const verdict = verifyBidAttestation(bids, tampered, attestation, directoryFor(scorer), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(false);
  });
});

// ─── Verification — tampered input ───────────────────────────────────────────

describe('verifyBidAttestation — tampered input', () => {
  it('rejects when a bid price is altered after the fact', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const tamperedBids = bids.map((b) => (b.agentAddress === 'GBEST' ? { ...b, price: '9' } : b));

    const verdict = verifyBidAttestation(tamperedBids, result, attestation, directoryFor(scorer), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(false);
  });

  it('rejects when a bid is added to the set after the fact', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const extraBid = [...bids, makeBid({ agentAddress: 'GINTRUDER' })];

    const verdict = verifyBidAttestation(extraBid, result, attestation, directoryFor(scorer), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(false);
  });
});

// ─── Verification — untrusted / mismatched keys ──────────────────────────────

describe('verifyBidAttestation — untrusted keys', () => {
  it('rejects a signature from a key not in the trusted directory', () => {
    const scorer = Keypair.random();
    const impostor = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const verdict = verifyBidAttestation(bids, result, attestation, directoryFor(impostor, 99), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/no trusted key/);
  });

  it('rejects an attestation forged with an untrusted keypair claiming a trusted epoch', () => {
    const trusted = Keypair.random();
    const attacker = Keypair.random();
    const bids = makeBids();

    // Attacker signs with their own key but the attestation's keyEpoch
    // matches a real, trusted epoch — the pubkey it carries won't match the
    // directory's pubkey for that epoch.
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, attacker, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const verdict = verifyBidAttestation(bids, result, attestation, directoryFor(trusted, 0), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/does not match the trusted key/);
  });

  it('rejects an attestation with a tampered signature', () => {
    const scorer = Keypair.random();
    const bids = makeBids();
    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, scorer, {
      keyEpoch: 0,
      now: () => FIXED_NOW,
    });

    const tampered = { ...attestation, signature: Buffer.from('not-a-real-signature').toString('base64') };

    const verdict = verifyBidAttestation(bids, result, tampered, directoryFor(scorer), {
      now: () => FIXED_NOW,
    });
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/signature is invalid/);
  });
});

// ─── Key rotation ─────────────────────────────────────────────────────────────

describe('verifyBidAttestation — key rotation', () => {
  it('accepts an attestation from a still-current epoch alongside a retired one', () => {
    const oldKey = Keypair.random();
    const newKey = Keypair.random();
    const bids = makeBids();

    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, newKey, {
      keyEpoch: 1,
      now: () => FIXED_NOW,
    });

    const directory: ScorerKeyDirectory = [
      { epoch: 0, publicKey: oldKey.publicKey(), validUntil: Math.floor(FIXED_NOW / 1000) - 1 },
      { epoch: 1, publicKey: newKey.publicKey() },
    ];

    const verdict = verifyBidAttestation(bids, result, attestation, directory, { now: () => FIXED_NOW });
    expect(verdict.valid).toBe(true);
  });

  it('rejects a new attestation issued under a retired epoch', () => {
    const oldKey = Keypair.random();
    const bids = makeBids();
    const retiredAt = Math.floor(FIXED_NOW / 1000) - 10;

    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, oldKey, {
      keyEpoch: 0,
      now: () => FIXED_NOW, // issued after retirement
    });

    const directory: ScorerKeyDirectory = [{ epoch: 0, publicKey: oldKey.publicKey(), validUntil: retiredAt }];

    const verdict = verifyBidAttestation(bids, result, attestation, directory, { now: () => FIXED_NOW });
    expect(verdict.valid).toBe(false);
    if (!verdict.valid) expect(verdict.reason).toMatch(/retired/);
  });

  it('still honors an attestation issued before its key was retired', () => {
    const oldKey = Keypair.random();
    const bids = makeBids();
    const issuedAtMs = FIXED_NOW;
    const retiredAtSeconds = Math.floor(FIXED_NOW / 1000) + 3600; // retired an hour later

    const { result, attestation } = attestRankBids(bids, DEFAULT_BID_WEIGHTS, oldKey, {
      keyEpoch: 0,
      ttlSeconds: 7200,
      now: () => issuedAtMs,
    });

    const directory: ScorerKeyDirectory = [
      { epoch: 0, publicKey: oldKey.publicKey(), validUntil: retiredAtSeconds },
    ];

    // Verified well after retirement, but the attestation was issued before it.
    const verdict = verifyBidAttestation(bids, result, attestation, directory, {
      now: () => (retiredAtSeconds + 60) * 1000,
    });
    expect(verdict.valid).toBe(true);
  });
});
