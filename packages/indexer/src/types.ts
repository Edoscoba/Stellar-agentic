import type { SorobanRpc, xdr } from "@stellar/stellar-sdk";

export type ContractKind =
  | "paymentChannel"
  | "escrow"
  | "rateLimiter"
  | "agentWalletFactory";

export interface ContractAddresses {
  paymentChannel: string;
  escrow: string;
  rateLimiter: string;
  agentWalletFactory: string;
}

export interface EventMetadata {
  eventId: string;
  contractKind: ContractKind;
  contractAddress: string;
  ledger: number;
  ledgerClosedAt: string;
  txHash: string;
  pagingToken: string;
  namespace: "channel" | "escrow" | "rl" | "factory" | "state";
  action: string;
  rawTopicXdr: string[];
  rawValueXdr: string;
}

export type DecodedEvent =
  | (EventMetadata & {
      namespace: "channel";
      action: "opened";
      channelId: string;
      agent: string;
      owner: string;
      deposit: string;
    })
  | (EventMetadata & {
      namespace: "channel";
      action: "paid";
      channelId: string;
      agent: string;
      recipient: string;
      amount: string;
      memo: string;
    })
  | (EventMetadata & {
      namespace: "channel";
      action: "convpaid";
      channelId: string;
      agent: string;
      recipient: string;
      amount: string;
      destinationToken: string;
      received: string;
      memo: string;
    })
  | (EventMetadata & {
      namespace: "channel";
      action: "topup";
      channelId: string;
      owner: string;
      amount: string;
    })
  | (EventMetadata & {
      namespace: "channel";
      action: "closed";
      channelId: string;
      owner: string;
    })
  | (EventMetadata & {
      namespace: "escrow";
      action: "created";
      jobId: string;
      requester: string;
      amount: string;
    })
  | (EventMetadata & {
      namespace: "escrow";
      action: "accepted" | "result";
      jobId: string;
      worker: string;
    })
  | (EventMetadata & {
      namespace: "escrow";
      action: "released";
      jobId: string;
      worker: string;
      amount: string;
    })
  | (EventMetadata & {
      namespace: "escrow";
      action: "refunded";
      jobId: string;
      requester: string;
      amount: string;
    })
  | (EventMetadata & {
      namespace: "escrow";
      action: "disputed";
      jobId: string;
      requester: string;
    })
  | (EventMetadata & {
      namespace: "rl";
      action: "recorded";
      agent: string;
      amount: string;
    })
  | (EventMetadata & {
      namespace: "rl";
      action: "killed";
      agent: string;
    })
  | (EventMetadata & {
      namespace: "factory";
      action: "created";
      agentId: string;
      agent: string;
      owner: string;
    })
  | (EventMetadata & {
      namespace: "factory";
      action: "deactiv" | "reactiv";
      agentId: string;
      owner: string;
    })
  | (EventMetadata & {
      namespace: "state";
      action: "channel";
      channelId: string;
      state: JsonValue;
    })
  | (EventMetadata & {
      namespace: "state";
      action: "job";
      jobId: string;
      state: JsonValue;
    })
  | (EventMetadata & {
      namespace: "state";
      action: "limit";
      agent: string;
      state: JsonValue;
    })
  | (EventMetadata & {
      namespace: "state";
      action: "agent";
      agentId: string;
      state: JsonValue;
    });

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export interface StoredEvent {
  eventId: string;
  contractKind: ContractKind;
  contractAddress: string;
  ledger: number;
  ledgerClosedAt: string;
  txHash: string;
  pagingToken: string;
  namespace: DecodedEvent["namespace"];
  action: string;
  entityType: "channel" | "job" | "agent" | null;
  entityId: string | null;
  payload: DecodedEvent;
}

export interface EventSource {
  getEvents(
    request: SorobanRpc.Server.GetEventsRequest,
  ): Promise<SorobanRpc.Api.GetEventsResponse>;
}

export interface RawContractEvent
  extends Omit<SorobanRpc.Api.EventResponse, "contractId" | "topic" | "value"> {
  contractId?: { toString(): string };
  topic: xdr.ScVal[];
  value: xdr.ScVal;
}
