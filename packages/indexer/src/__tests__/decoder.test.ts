import {
  Address,
  Keypair,
  nativeToScVal,
  xdr,
  type SorobanRpc,
} from "@stellar/stellar-sdk";
import { describe, expect, it } from "vitest";
import { decodeEvent } from "../decoder.js";
import type { RawContractEvent } from "../types.js";

function address(): string {
  return Keypair.random().publicKey();
}

function raw(
  namespace: string,
  action: string,
  values: unknown[],
): RawContractEvent {
  return {
    id: "0000000000000042-0000000001",
    type: "contract",
    ledger: 42,
    ledgerClosedAt: "2026-01-01T00:00:00Z",
    pagingToken: "42-1",
    inSuccessfulContractCall: true,
    txHash: "abc",
    contractId: { toString: () => "CPAYMENT" },
    topic: [xdr.ScVal.scvSymbol(namespace), xdr.ScVal.scvSymbol(action)],
    value: xdr.ScVal.scvVec(
      values.map((value) =>
        value instanceof Address ? value.toScVal() : nativeToScVal(value),
      ),
    ),
  } as RawContractEvent & SorobanRpc.Api.EventResponse;
}

describe("decodeEvent", () => {
  it("decodes the exact payment tuple and preserves raw XDR", () => {
    const agent = address();
    const recipient = address();
    const event = decodeEvent(
      raw("channel", "paid", [
        7n,
        new Address(agent),
        new Address(recipient),
        250n,
        Buffer.from("invoice-7"),
      ]),
      "paymentChannel",
      "CPAYMENT",
    );

    expect(event).toMatchObject({
      namespace: "channel",
      action: "paid",
      channelId: "7",
      agent,
      recipient,
      amount: "250",
      memo: Buffer.from("invoice-7").toString("base64"),
    });
    expect(event.rawTopicXdr).toHaveLength(2);
    expect(event.rawValueXdr).not.toBe("");
  });

  it("decodes a scalar killed payload as a one-value tuple", () => {
    const agent = address();
    const event = raw("rl", "killed", []);
    expect(
      decodeEvent(
        { ...event, value: new Address(agent).toScVal() },
        "rateLimiter",
        "CRATE",
      ),
    ).toMatchObject({ action: "killed", agent });
  });

  it("rejects malformed known and unknown events", () => {
    expect(() =>
      decodeEvent(raw("escrow", "released", [1n]), "escrow", "CESCROW"),
    ).toThrow("expected 3 values");
    expect(() =>
      decodeEvent(raw("escrow", "surprise", []), "escrow", "CESCROW"),
    ).toThrow("unknown StellarAgent event");
  });

  it.each([
    ["channel", "opened", [1n, "GAGENT", "GOWNER", 100n]],
    ["channel", "topup", [1n, "GOWNER", 50n]],
    ["channel", "closed", [1n, "GOWNER"]],
    ["channel", "convpaid", [1n, "GAGENT", "GTO", 10n, "CTOKEN", 9n, Buffer.alloc(0)]],
    ["escrow", "created", [2n, "GREQUESTER", 200n]],
    ["escrow", "accepted", [2n, "GWORKER"]],
    ["escrow", "result", [2n, "GWORKER"]],
    ["escrow", "released", [2n, "GWORKER", 200n]],
    ["escrow", "refunded", [2n, "GREQUESTER", 200n]],
    ["escrow", "disputed", [2n, "GREQUESTER"]],
    ["rl", "recorded", ["GAGENT", 10n]],
    ["factory", "created", [3n, "GAGENT", "GOWNER"]],
    ["factory", "deactiv", [3n, "GOWNER"]],
    ["factory", "reactiv", [3n, "GOWNER"]],
    ["state", "channel", [1n, { active: true, total_spent: 25n }]],
    ["state", "job", [2n, { amount: 200n, status: ["Completed"] }]],
    ["state", "limit", ["GAGENT", { active: true, daily_spend: 10n }]],
    ["state", "agent", [3n, { active: true, name: "buyer" }]],
  ])("decodes %s/%s", (namespace, action, values) => {
    expect(
      decodeEvent(
        raw(namespace, action, values),
        namespace === "channel"
          ? "paymentChannel"
          : namespace === "rl"
            ? "rateLimiter"
          : namespace === "factory" || (namespace === "state" && action === "agent")
              ? "agentWalletFactory"
              : namespace === "state" && action === "limit"
                ? "rateLimiter"
                : namespace === "state" && action === "channel"
                  ? "paymentChannel"
              : "escrow",
        "CCONTRACT",
      ),
    ).toMatchObject({ namespace, action });
  });
});
