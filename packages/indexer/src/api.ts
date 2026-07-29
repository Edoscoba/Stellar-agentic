import { createServer, type Server } from "node:http";
import { URL } from "node:url";
import type { EventStore } from "./store.js";

export function createQueryServer(store: EventStore): Server {
  return createServer((request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://localhost");
      response.setHeader("content-type", "application/json; charset=utf-8");

      let result: unknown;
      let match = url.pathname.match(/^\/agents\/([^/]+)\/events$/);
      if (request.method === "GET" && match) {
        result = store.eventsForAgent(decodeURIComponent(match[1]));
      } else if (
        request.method === "GET" &&
        (match = url.pathname.match(/^\/channels\/([^/]+)\/spend$/))
      ) {
        result = store.spendHistory(decodeURIComponent(match[1]));
      } else if (
        request.method === "GET" &&
        (match = url.pathname.match(/^\/jobs\/([^/]+)\/lifecycle$/))
      ) {
        result = store.jobLifecycle(decodeURIComponent(match[1]));
      } else if (
        request.method === "GET" &&
        (match = url.pathname.match(/^\/channels\/([^/]+)\/state$/))
      ) {
        result = store.channelState(decodeURIComponent(match[1])) ?? null;
      } else if (
        request.method === "GET" &&
        (match = url.pathname.match(/^\/jobs\/([^/]+)\/state$/))
      ) {
        result = store.jobState(decodeURIComponent(match[1])) ?? null;
      } else if (
        request.method === "GET" &&
        (match = url.pathname.match(/^\/rate-limits\/([^/]+)\/state$/))
      ) {
        result = store.rateLimitState(decodeURIComponent(match[1])) ?? null;
      } else if (
        request.method === "GET" &&
        (match = url.pathname.match(/^\/agent-info\/([^/]+)\/state$/))
      ) {
        result = store.agentInfoState(decodeURIComponent(match[1])) ?? null;
      } else if (request.method === "GET" && url.pathname === "/events") {
        const limit = Math.min(Number(url.searchParams.get("limit") ?? 100), 1_000);
        const offset = Number(url.searchParams.get("offset") ?? 0);
        result = store.allEvents(limit, offset);
      } else if (request.method === "GET" && url.pathname === "/health") {
        result = { ok: true, nextLedger: store.checkpoint() ?? null };
      } else {
        response.statusCode = 404;
        result = { error: "not found" };
      }
      response.end(JSON.stringify(result));
    } catch (error) {
      response.statusCode = 500;
      response.end(
        JSON.stringify({
          error: error instanceof Error ? error.message : String(error),
        }),
      );
    }
  });
}
