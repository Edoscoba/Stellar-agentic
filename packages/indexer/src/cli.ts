#!/usr/bin/env node
import { createQueryServer } from "./api.js";
import { loadEnvironment } from "./config.js";
import { SorobanEventIndexer } from "./indexer.js";
import { EventStore } from "./store.js";

async function main(): Promise<void> {
  const command = process.argv[2] ?? "tail";
  if (command !== "catch-up" && command !== "tail") {
    throw new Error(
      "usage: stellaragent-indexer [catch-up|tail] [--from-ledger N]",
    );
  }
  const fromIndex = process.argv.indexOf("--from-ledger");
  const fromLedger =
    fromIndex === -1 ? undefined : Number(process.argv[fromIndex + 1]);
  if (
    fromLedger !== undefined &&
    (!Number.isSafeInteger(fromLedger) || fromLedger < 1)
  ) {
    throw new Error("--from-ledger must be a positive integer");
  }
  const config = loadEnvironment();
  const store = new EventStore(config.databasePath);
  const indexer = new SorobanEventIndexer({
    rpcUrl: config.rpcUrl,
    store,
    contracts: config.contracts,
    startLedger: config.startLedger,
    rollbackWindow: config.rollbackWindow,
    finalityLag: config.finalityLag,
    pollIntervalMs: config.pollIntervalMs,
  });

  const result = await indexer.catchUp(fromLedger);
  process.stdout.write(
    `Indexed ${result.eventCount} events through ledger ${result.throughLedger}\n`,
  );
  if (command === "catch-up") {
    store.close();
    return;
  }

  const server = createQueryServer(store);
  server.listen(config.port, () => {
    process.stdout.write(`Audit API listening on http://localhost:${config.port}\n`);
  });
  const controller = new AbortController();
  const shutdown = (): void => {
    controller.abort();
    indexer.stop();
    server.close(() => store.close());
  };
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
  await indexer.liveTail(controller.signal);
}

main().catch((error: unknown) => {
  process.stderr.write(
    `${error instanceof Error ? error.stack ?? error.message : String(error)}\n`,
  );
  process.exitCode = 1;
});
