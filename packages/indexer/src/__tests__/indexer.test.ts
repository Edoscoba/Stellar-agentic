import { nativeToScVal, xdr, type SorobanRpc } from "@stellar/stellar-sdk";
import { describe, expect, it } from "vitest";
import { SorobanEventIndexer } from "../indexer.js";
import { EventStore } from "../store.js";
import type { EventSource } from "../types.js";

const contracts = {
  paymentChannel: "CPAYMENT",
  escrow: "CESCROW",
  rateLimiter: "CRATE",
  agentWalletFactory: "CFACTORY",
};

function rpcEvent(id: string, ledger: number, amount: bigint) {
  return {
    id,
    type: "contract" as const,
    ledger,
    ledgerClosedAt: "2026-01-01T00:00:00Z",
    pagingToken: `${ledger}-${id}`,
    inSuccessfulContractCall: true,
    txHash: `tx-${id}`,
    contractId: { toString: () => contracts.rateLimiter },
    topic: [xdr.ScVal.scvSymbol("rl"), xdr.ScVal.scvSymbol("recorded")],
    value: xdr.ScVal.scvVec([
      nativeToScVal("GAGENT"),
      nativeToScVal(amount),
    ]),
  } as SorobanRpc.Api.EventResponse;
}

describe("SorobanEventIndexer", () => {
  it("replays its rollback window and removes orphaned RPC events", async () => {
    let canonical = [rpcEvent("old", 99, 10n)];
    const starts: number[] = [];
    const source: EventSource = {
      async getEvents(request) {
        starts.push(request.startLedger!);
        return { latestLedger: 100, events: canonical };
      },
    };
    const store = new EventStore(":memory:");
    const indexer = new SorobanEventIndexer({
      source,
      store,
      contracts,
      startLedger: 90,
      rollbackWindow: 5,
      finalityLag: 0,
    });

    await indexer.runOnce();
    canonical = [rpcEvent("replacement", 99, 20n)];
    await indexer.runOnce();

    expect(starts).toEqual([90, 96]);
    expect(store.allEvents().map((event) => event.eventId)).toEqual(["replacement"]);
    expect(store.checkpoint()).toBe(101);
    store.close();
  });

  it("follows paging tokens and honors the finality lag", async () => {
    const requests: SorobanRpc.Server.GetEventsRequest[] = [];
    const source: EventSource = {
      async getEvents(request) {
        requests.push(request);
        if (!request.cursor) {
          return { latestLedger: 50, events: [rpcEvent("a", 48, 1n)] };
        }
        if (request.cursor === "48-a") {
          return { latestLedger: 51, events: [rpcEvent("b", 50, 2n)] };
        }
        return { latestLedger: 51, events: [] };
      },
    };
    const store = new EventStore(":memory:");
    const indexer = new SorobanEventIndexer({
      source,
      store,
      contracts,
      startLedger: 40,
      finalityLag: 1,
      pageSize: 1,
    });

    const result = await indexer.runOnce();
    expect(requests[1]).toMatchObject({ cursor: "48-a" });
    expect(requests[1].startLedger).toBeUndefined();
    expect(result).toEqual({ fromLedger: 40, throughLedger: 49, eventCount: 1 });
    expect(store.allEvents().map((event) => event.eventId)).toEqual(["a"]);
    store.close();
  });
});
