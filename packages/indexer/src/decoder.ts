import { scValToNative } from "@stellar/stellar-sdk";
import type { ContractKind, DecodedEvent, RawContractEvent } from "./types.js";

const expectedLengths: Record<string, number> = {
  "channel/opened": 4,
  "channel/paid": 5,
  "channel/convpaid": 7,
  "channel/topup": 3,
  "channel/closed": 2,
  "escrow/created": 3,
  "escrow/accepted": 2,
  "escrow/result": 2,
  "escrow/released": 3,
  "escrow/refunded": 3,
  "escrow/disputed": 2,
  "rl/recorded": 2,
  "rl/killed": 1,
  "factory/created": 3,
  "factory/deactiv": 2,
  "factory/reactiv": 2,
  "state/channel": 2,
  "state/job": 2,
  "state/limit": 2,
  "state/agent": 2,
};

function scalar(value: unknown): string {
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "string") return value;
  if (value instanceof Uint8Array) return Buffer.from(value).toString("base64");
  throw new Error(`unsupported event scalar: ${String(value)}`);
}

function tuple(value: unknown, length: number, key: string): unknown[] {
  const values = Array.isArray(value) ? value : [value];
  if (values.length !== length) {
    throw new Error(`${key} expected ${length} values, received ${values.length}`);
  }
  return values;
}

function jsonValue(value: unknown): import("./types.js").JsonValue {
  if (value === null || typeof value === "boolean" || typeof value === "string") {
    return value;
  }
  if (typeof value === "number") return value;
  if (typeof value === "bigint") return value.toString();
  if (value instanceof Uint8Array) return Buffer.from(value).toString("base64");
  if (Array.isArray(value)) return value.map(jsonValue);
  if (typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([key, nested]) => [
        key,
        jsonValue(nested),
      ]),
    );
  }
  throw new Error(`unsupported event value: ${String(value)}`);
}

export function decodeEvent(
  raw: RawContractEvent,
  contractKind: ContractKind,
  contractAddress: string,
): DecodedEvent {
  const namespace = scalar(scValToNative(raw.topic[0]));
  const action = scalar(scValToNative(raw.topic[1]));
  const key = `${namespace}/${action}`;
  const length = expectedLengths[key];
  if (length === undefined) throw new Error(`unknown StellarAgent event ${key}`);

  const nativeValues = tuple(scValToNative(raw.value), length, key);
  const values = nativeValues.map((value, index) =>
    namespace === "state" && index === 1 ? "" : scalar(value),
  );
  const base = {
    eventId: raw.id,
    contractKind,
    contractAddress,
    ledger: raw.ledger,
    ledgerClosedAt: raw.ledgerClosedAt,
    txHash: raw.txHash,
    pagingToken: raw.pagingToken,
    namespace,
    action,
    rawTopicXdr: raw.topic.map((value) => value.toXDR("base64")),
    rawValueXdr: raw.value.toXDR("base64"),
  };

  switch (key) {
    case "channel/opened":
      return { ...base, namespace: "channel", action: "opened", channelId: values[0], agent: values[1], owner: values[2], deposit: values[3] };
    case "channel/paid":
      return { ...base, namespace: "channel", action: "paid", channelId: values[0], agent: values[1], recipient: values[2], amount: values[3], memo: values[4] };
    case "channel/convpaid":
      return { ...base, namespace: "channel", action: "convpaid", channelId: values[0], agent: values[1], recipient: values[2], amount: values[3], destinationToken: values[4], received: values[5], memo: values[6] };
    case "channel/topup":
      return { ...base, namespace: "channel", action: "topup", channelId: values[0], owner: values[1], amount: values[2] };
    case "channel/closed":
      return { ...base, namespace: "channel", action: "closed", channelId: values[0], owner: values[1] };
    case "escrow/created":
      return { ...base, namespace: "escrow", action: "created", jobId: values[0], requester: values[1], amount: values[2] };
    case "escrow/accepted":
    case "escrow/result":
      return { ...base, namespace: "escrow", action, jobId: values[0], worker: values[1] } as DecodedEvent;
    case "escrow/released":
      return { ...base, namespace: "escrow", action: "released", jobId: values[0], worker: values[1], amount: values[2] };
    case "escrow/refunded":
      return { ...base, namespace: "escrow", action: "refunded", jobId: values[0], requester: values[1], amount: values[2] };
    case "escrow/disputed":
      return { ...base, namespace: "escrow", action: "disputed", jobId: values[0], requester: values[1] };
    case "rl/recorded":
      return { ...base, namespace: "rl", action: "recorded", agent: values[0], amount: values[1] };
    case "rl/killed":
      return { ...base, namespace: "rl", action: "killed", agent: values[0] };
    case "factory/created":
      return { ...base, namespace: "factory", action: "created", agentId: values[0], agent: values[1], owner: values[2] };
    case "factory/deactiv":
    case "factory/reactiv":
      return { ...base, namespace: "factory", action, agentId: values[0], owner: values[1] } as DecodedEvent;
    case "state/channel":
      return { ...base, namespace: "state", action: "channel", channelId: values[0], state: jsonValue(nativeValues[1]) };
    case "state/job":
      return { ...base, namespace: "state", action: "job", jobId: values[0], state: jsonValue(nativeValues[1]) };
    case "state/limit":
      return { ...base, namespace: "state", action: "limit", agent: values[0], state: jsonValue(nativeValues[1]) };
    case "state/agent":
      return { ...base, namespace: "state", action: "agent", agentId: values[0], state: jsonValue(nativeValues[1]) };
    default:
      throw new Error(`unreachable event ${key}`);
  }
}
