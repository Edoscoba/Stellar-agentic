/**
 * Real-RPC integration test.
 *
 * Prerequisites:
 *   stellar network start local
 *   pnpm deploy:contracts --network local --source alice
 *   STELLAR_LOCAL_INTEGRATION=1 pnpm --filter @stellaragent/indexer test
 */
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { SorobanRpc } from "@stellar/stellar-sdk";
import { describe, expect, it } from "vitest";
import { SorobanEventIndexer } from "../indexer.js";
import { EventStore } from "../store.js";
import type { ContractAddresses } from "../types.js";

const enabled = process.env.STELLAR_LOCAL_INTEGRATION === "1";
const describeLocal = enabled ? describe : describe.skip;

function stellar(args: string[]): string {
  return execFileSync("stellar", args, { encoding: "utf8" }).trim();
}

describeLocal("local Soroban standalone event indexing", () => {
  it("indexes and decodes a real factory lifecycle", async () => {
    const source = process.env.STELLAR_LOCAL_SOURCE ?? "alice";
    const rpcUrl =
      process.env.SOROBAN_RPC_URL ?? "http://localhost:8000/soroban/rpc";
    const deploymentPath =
      process.env.INDEXER_DEPLOYMENT_FILE ??
      resolve(process.cwd(), "../../deployments/local.json");
    const deployment = JSON.parse(readFileSync(deploymentPath, "utf8")) as {
      contracts: ContractAddresses;
    };
    const rpc = new SorobanRpc.Server(rpcUrl, { allowHttp: true });
    const startLedger = (await rpc.getLatestLedger()).sequence;
    const owner = stellar(["keys", "address", source]);
    const invoke = (fn: string, args: string[]) =>
      stellar([
        "contract",
        "invoke",
        "--id",
        deployment.contracts.agentWalletFactory,
        "--source-account",
        source,
        "--network",
        "local",
        "--",
        fn,
        ...args,
      ]);

    const name = `indexer-${Date.now()}`;
    const rawId = invoke("create_agent", [
      "--owner",
      owner,
      "--agent_address",
      owner,
      "--name",
      name,
    ]);
    const agentId = rawId.replace(/"/g, "").trim();
    invoke("deactivate_agent", ["--owner", owner, "--agent_id", agentId]);
    invoke("reactivate_agent", ["--owner", owner, "--agent_id", agentId]);

    const store = new EventStore(":memory:");
    const indexer = new SorobanEventIndexer({
      rpcUrl,
      store,
      contracts: deployment.contracts,
      startLedger,
      rollbackWindow: 0,
      finalityLag: 0,
      allowHttp: true,
    });
    await indexer.catchUp();

    const lifecycle = store
      .eventsForAgent(owner)
      .filter(
        (event) =>
          event.namespace === "factory" && event.entityId === agentId,
      );
    expect(lifecycle.map((event) => event.action)).toEqual([
      "created",
      "deactiv",
      "reactiv",
    ]);
    expect(lifecycle[0]?.payload).toMatchObject({
      agentId,
      agent: owner,
      owner,
    });

    const onChain = JSON.parse(
      invoke("get_agent", ["--agent_id", agentId]),
    ) as {
      active: boolean;
      address: string;
      created_at: number;
      name: string;
      owner: string;
      total_ops: number | string;
    };
    expect(onChain).toMatchObject({ active: true, address: owner, owner });
    expect(lifecycle.at(-1)?.action).toBe("reactiv");
    expect(store.agentInfoState(agentId)).toEqual({
      ...onChain,
      total_ops: String(onChain.total_ops),
    });
    store.close();
  }, 120_000);
});
