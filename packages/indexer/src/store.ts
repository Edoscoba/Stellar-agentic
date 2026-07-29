import Database from "better-sqlite3";
import type { DecodedEvent, StoredEvent } from "./types.js";

export interface ChannelSpend {
  channelId: string;
  totalSpent: string;
  payments: StoredEvent[];
}

export interface JobLifecycle {
  jobId: string;
  status: "Open" | "InProgress" | "PendingRelease" | "Completed" | "Refunded" | "Disputed" | "Unknown";
  events: StoredEvent[];
}

function entity(event: DecodedEvent): {
  type: StoredEvent["entityType"];
  id: string | null;
} {
  if ("channelId" in event) return { type: "channel", id: event.channelId };
  if ("jobId" in event) return { type: "job", id: event.jobId };
  if ("agentId" in event) return { type: "agent", id: event.agentId };
  if ("agent" in event) return { type: "agent", id: event.agent };
  return { type: null, id: null };
}

function participants(event: DecodedEvent): Array<[string, string]> {
  const result: Array<[string, string]> = [];
  const fields = event as unknown as Record<string, unknown>;
  for (const role of ["agent", "owner", "recipient", "requester", "worker"] as const) {
    if (typeof fields[role] === "string") {
      result.push([fields[role], role]);
    }
  }
  if ("state" in event && event.state && !Array.isArray(event.state) && typeof event.state === "object") {
    for (const role of ["agent", "address", "owner", "recipient", "requester", "worker", "arbiter"]) {
      const value = event.state[role];
      if (typeof value === "string") result.push([value, role]);
    }
  }
  return result;
}

export class EventStore {
  private readonly db: Database.Database;

  constructor(filename = "stellaragent-events.sqlite") {
    this.db = new Database(filename);
    this.db.pragma("journal_mode = WAL");
    this.db.pragma("foreign_keys = ON");
    this.migrate();
  }

  private migrate(): void {
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS events (
        event_id TEXT PRIMARY KEY,
        contract_kind TEXT NOT NULL,
        contract_address TEXT NOT NULL,
        ledger INTEGER NOT NULL,
        ledger_closed_at TEXT NOT NULL,
        tx_hash TEXT NOT NULL,
        paging_token TEXT NOT NULL,
        namespace TEXT NOT NULL,
        action TEXT NOT NULL,
        entity_type TEXT,
        entity_id TEXT,
        payload_json TEXT NOT NULL,
        topic_xdr_json TEXT NOT NULL,
        value_xdr TEXT NOT NULL
      );
      CREATE INDEX IF NOT EXISTS events_ledger_idx
        ON events (ledger, paging_token);
      CREATE INDEX IF NOT EXISTS events_entity_idx
        ON events (entity_type, entity_id, ledger, paging_token);

      CREATE TABLE IF NOT EXISTS event_participants (
        event_id TEXT NOT NULL REFERENCES events(event_id) ON DELETE CASCADE,
        address TEXT NOT NULL,
        role TEXT NOT NULL,
        PRIMARY KEY (event_id, address, role)
      );
      CREATE INDEX IF NOT EXISTS event_participants_address_idx
        ON event_participants (address, event_id);

      CREATE TABLE IF NOT EXISTS checkpoints (
        stream TEXT PRIMARY KEY,
        next_ledger INTEGER NOT NULL,
        updated_at TEXT NOT NULL
      );
    `);
  }

  close(): void {
    this.db.close();
  }

  checkpoint(stream = "stellaragent"): number | undefined {
    const row = this.db
      .prepare("SELECT next_ledger AS nextLedger FROM checkpoints WHERE stream = ?")
      .get(stream) as { nextLedger: number } | undefined;
    return row?.nextLedger;
  }

  replaceRange(
    fromLedger: number,
    throughLedger: number,
    events: DecodedEvent[],
    stream = "stellaragent",
  ): void {
    if (throughLedger < fromLedger) return;

    const insertEvent = this.db.prepare(`
      INSERT INTO events (
        event_id, contract_kind, contract_address, ledger, ledger_closed_at,
        tx_hash, paging_token, namespace, action, entity_type, entity_id,
        payload_json, topic_xdr_json, value_xdr
      ) VALUES (
        @eventId, @contractKind, @contractAddress, @ledger, @ledgerClosedAt,
        @txHash, @pagingToken, @namespace, @action, @entityType, @entityId,
        @payloadJson, @topicXdrJson, @valueXdr
      )
      ON CONFLICT(event_id) DO UPDATE SET
        contract_kind = excluded.contract_kind,
        contract_address = excluded.contract_address,
        ledger = excluded.ledger,
        ledger_closed_at = excluded.ledger_closed_at,
        tx_hash = excluded.tx_hash,
        paging_token = excluded.paging_token,
        namespace = excluded.namespace,
        action = excluded.action,
        entity_type = excluded.entity_type,
        entity_id = excluded.entity_id,
        payload_json = excluded.payload_json,
        topic_xdr_json = excluded.topic_xdr_json,
        value_xdr = excluded.value_xdr
    `);
    const insertParticipant = this.db.prepare(`
      INSERT OR IGNORE INTO event_participants (event_id, address, role)
      VALUES (?, ?, ?)
    `);
    const update = this.db.transaction(() => {
      this.db
        .prepare("DELETE FROM events WHERE ledger BETWEEN ? AND ?")
        .run(fromLedger, throughLedger);
      for (const event of events) {
        if (event.ledger < fromLedger || event.ledger > throughLedger) continue;
        const target = entity(event);
        insertEvent.run({
          ...event,
          entityType: target.type,
          entityId: target.id,
          payloadJson: JSON.stringify(event),
          topicXdrJson: JSON.stringify(event.rawTopicXdr),
          valueXdr: event.rawValueXdr,
        });
        for (const [address, role] of participants(event)) {
          insertParticipant.run(event.eventId, address, role);
        }
      }
      this.db
        .prepare(`
          INSERT INTO checkpoints (stream, next_ledger, updated_at)
          VALUES (?, ?, ?)
          ON CONFLICT(stream) DO UPDATE SET
            next_ledger = excluded.next_ledger,
            updated_at = excluded.updated_at
        `)
        .run(stream, throughLedger + 1, new Date().toISOString());
    });
    update();
  }

  eventsForAgent(address: string): StoredEvent[] {
    return this.rows(`
      SELECT DISTINCT e.* FROM events e
      LEFT JOIN event_participants p ON p.event_id = e.event_id
      WHERE p.address = ?
        OR (
          e.entity_type = 'agent'
          AND e.entity_id IN (
            SELECT created.entity_id
            FROM events created
            JOIN event_participants creator
              ON creator.event_id = created.event_id
            WHERE created.namespace = 'factory'
              AND created.action = 'created'
              AND creator.role = 'agent'
              AND creator.address = ?
          )
        )
      ORDER BY e.ledger, e.paging_token
    `, address, address);
  }

  spendHistory(channelId: string): ChannelSpend {
    const payments = this.rows(`
      SELECT * FROM events
      WHERE entity_type = 'channel' AND entity_id = ?
        AND action IN ('paid', 'convpaid')
      ORDER BY ledger, paging_token
    `, channelId);
    const total = payments.reduce(
      (sum, event) => sum + BigInt((event.payload as { amount: string }).amount),
      0n,
    );
    return { channelId, totalSpent: total.toString(), payments };
  }

  jobLifecycle(jobId: string): JobLifecycle {
    const events = this.rows(`
      SELECT * FROM events
      WHERE entity_type = 'job' AND entity_id = ? AND namespace = 'escrow'
      ORDER BY ledger, paging_token
    `, jobId);
    let status: JobLifecycle["status"] = "Unknown";
    for (const event of events) {
      if (event.action === "created") status = "Open";
      else if (event.action === "accepted") status = "InProgress";
      else if (event.action === "result") status = "PendingRelease";
      else if (event.action === "released") status = "Completed";
      else if (event.action === "refunded") status = "Refunded";
      else if (event.action === "disputed") status = "Disputed";
    }
    return { jobId, status, events };
  }

  allEvents(limit = 100, offset = 0): StoredEvent[] {
    return this.rows(
      "SELECT * FROM events ORDER BY ledger, paging_token LIMIT ? OFFSET ?",
      limit,
      offset,
    );
  }

  channelState(channelId: string): unknown | undefined {
    return this.latestSnapshot("channel", "channel", channelId);
  }

  jobState(jobId: string): unknown | undefined {
    return this.latestSnapshot("job", "job", jobId);
  }

  rateLimitState(agent: string): unknown | undefined {
    return this.latestSnapshot("limit", "agent", agent);
  }

  agentInfoState(agentId: string): unknown | undefined {
    return this.latestSnapshot("agent", "agent", agentId);
  }

  private latestSnapshot(
    action: string,
    entityType: string,
    entityId: string,
  ): unknown | undefined {
    const event = this.rows(`
      SELECT * FROM events
      WHERE namespace = 'state' AND action = ?
        AND entity_type = ? AND entity_id = ?
      ORDER BY ledger DESC, paging_token DESC
      LIMIT 1
    `, action, entityType, entityId)[0];
    return event && "state" in event.payload ? event.payload.state : undefined;
  }

  private rows(sql: string, ...params: unknown[]): StoredEvent[] {
    const rows = this.db.prepare(sql).all(...params) as Array<
      Record<string, unknown> & { payload_json: string }
    >;
    return rows.map((row) => ({
      eventId: row.event_id as string,
      contractKind: row.contract_kind as StoredEvent["contractKind"],
      contractAddress: row.contract_address as string,
      ledger: row.ledger as number,
      ledgerClosedAt: row.ledger_closed_at as string,
      txHash: row.tx_hash as string,
      pagingToken: row.paging_token as string,
      namespace: row.namespace as StoredEvent["namespace"],
      action: row.action as string,
      entityType: row.entity_type as StoredEvent["entityType"],
      entityId: row.entity_id as string | null,
      payload: JSON.parse(row.payload_json) as DecodedEvent,
    }));
  }
}
