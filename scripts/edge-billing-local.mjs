#!/usr/bin/env node
/**
 * Local billing probe for the edge Worker.
 *
 * `wrangler dev` captures every invocation as a queryable span, including the
 * ones that decide the Durable Object bill and are invisible everywhere else:
 * `durable_object_subrequest` (one billable DO request each),
 * `hibernatableWebSocket` (inbound WS events, billed 20:1),
 * `durable_object_storage_setAlarm` (one request AND one row written), and
 * every `durable_object_storage_exec` with its exact rows_written/rows_read.
 *
 * Why this and not the development Worker: the deployed dev Worker wraps every
 * operation in `DevelopmentGuard`, which spends its own requests and rows
 * (`edge/src/development-budget.ts` — `budget.events++; budget.rows++`), and
 * its room allowlist is capped at 16 for the lifetime of the guard data. It is
 * an integration target, not a measurement target. This is guard-free, exact,
 * unmetered and offline.
 *
 * Usage:
 *   node scripts/edge-billing-local.mjs --mark            # epoch ms to scope a run
 *   node scripts/edge-billing-local.mjs [--since MS] [--port N] [--json]
 *
 * Start the server first:
 *   cd edge && npx wrangler dev --port 27655 --var AUTH_MODE:dev --local
 */
const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] !== undefined ? args[i + 1] : fallback;
};
if (args.includes("--mark")) {
  console.log(String(Date.now()));
  process.exit(0);
}
const port = Number(flag("--port", 27655));
const since = Number(flag("--since", 0));
const asJson = args.includes("--json");

/** Cloudflare's published conversion: inbound WS application messages bill
 * 20:1, while HTTP requests, WS upgrades and alarm deliveries bill 1:1 and
 * outbound messages are free. */
const WS_MESSAGES_PER_REQUEST = 20;

const query = async (sql) => {
  const response = await fetch(
    `http://localhost:${port}/cdn-cgi/local/explorer/api/local/observability/query`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ sql })
    }
  );
  if (!response.ok) {
    throw new Error(
      `observability query failed (${response.status}). Is \`wrangler dev --port ${port}\` running?`
    );
  }
  const body = await response.json();
  if (!body.success) throw new Error(JSON.stringify(body.errors));
  const { columns, rows } = body.result;
  return rows.map((row) => Object.fromEntries(columns.map((c, i) => [c, row[i]])));
};

const where = `WHERE start_ms >= ${Number.isFinite(since) ? since : 0}`;
const spans = await query(
  `SELECT span_id, parent_id, name, start_ms, json(attributes) AS attributes FROM spans ${where}`
);
if (spans.length === 0) {
  console.error(`No spans since ${since}. Drive some traffic first.`);
  process.exit(1);
}

const attrs = (span) => {
  try {
    return JSON.parse(span.attributes) ?? {};
  } catch {
    return {};
  }
};
const byId = new Map(spans.map((s) => [s.span_id, s]));

/** The DO-bound request's path: the Worker rewrites it before forwarding, so
 * the nested http span carries the in-room path (`/rows`, `/notifications/activity`). */
const pathOf = (span) => {
  let cursor = span;
  for (let hop = 0; hop < 6 && cursor; hop++) {
    const url = attrs(cursor)["url.full"];
    if (url) {
      try {
        return `${attrs(cursor)["http.request.method"] ?? "?"} ${new URL(url).pathname}`;
      } catch {
        return url.split("?")[0];
      }
    }
    cursor = cursor.parent_id ? byId.get(cursor.parent_id) : undefined;
  }
  return "(unknown)";
};

const doRequests = spans.filter((s) => s.name === "durable_object_subrequest");
const wsEvents = spans.filter((s) => s.name === "hibernatableWebSocket");
const alarmSets = spans.filter((s) => s.name === "durable_object_storage_setAlarm");
const alarmDeletes = spans.filter((s) => s.name === "durable_object_storage_delete_alarm"
  || s.name === "durable_object_storage_deleteAlarm");
const execs = spans.filter((s) => s.name === "durable_object_storage_exec");
const workerRequests = spans.filter((s) => s.parent_id === null && attrs(s)["url.full"]);

let rowsWritten = 0;
let rowsRead = 0;
let emulatorRows = 0;
const byQuery = new Map();
for (const span of execs) {
  const a = attrs(span);
  const written = a["cloudflare.durable_object.response.rows_written"] ?? 0;
  const read = a["cloudflare.durable_object.response.rows_read"] ?? 0;
  const text = String(a["db.query.text"] ?? "");
  const verb = text.trim().split(/\s+/)[0]?.toUpperCase() ?? "?";
  const table = text.match(/(?:INTO|UPDATE|FROM)\s+(\w+)/i)?.[1] ?? "schema";
  // miniflare keeps its own id→name bookkeeping table that the real runtime
  // does not. Counting it would overstate rows written on every cold room.
  if (table.startsWith("__miniflare")) {
    emulatorRows += written;
    continue;
  }
  rowsWritten += written;
  rowsRead += read;
  const key = `${verb}:${table}`;
  const entry = byQuery.get(key) ?? { calls: 0, written: 0, read: 0 };
  entry.calls += 1;
  entry.written += written;
  entry.read += read;
  byQuery.set(key, entry);
}
// setAlarm/deleteAlarm bill one row written each and never touch storage.sql.
rowsWritten += alarmSets.length + alarmDeletes.length;

const pathCounts = new Map();
for (const span of doRequests) {
  const key = pathOf(span);
  pathCounts.set(key, (pathCounts.get(key) ?? 0) + 1);
}

const wsBillable = wsEvents.length / WS_MESSAGES_PER_REQUEST;
const alarmBillable = alarmSets.length + alarmDeletes.length;
const billable = doRequests.length + wsBillable + alarmBillable;

if (asJson) {
  console.log(JSON.stringify({
    since,
    workerRequests: workerRequests.length,
    doHttpRequests: doRequests.length,
    wsEvents: wsEvents.length,
    alarmCalls: alarmBillable,
    billableRequests: Number(billable.toFixed(2)),
    rowsWritten,
    rowsRead,
    emulatorRowsExcluded: emulatorRows,
    byPath: Object.fromEntries(pathCounts),
    byQuery: Object.fromEntries(byQuery)
  }, null, 2));
  process.exit(0);
}

const pad = (n) => String(n).padStart(7);
console.log(`\nspans since ${since || "(all)"} — ${spans.length} captured\n`);
console.log("BILLABLE DURABLE OBJECT REQUESTS");
console.log(`  DO HTTP (1:1)                 ${pad(doRequests.length)}`);
console.log(`  WS events (${WS_MESSAGES_PER_REQUEST}:1)  ${pad(wsEvents.length)} → ${wsBillable.toFixed(2)}`);
console.log(`  alarm set/delete (1:1)        ${pad(alarmBillable)}`);
console.log(`  ────────────────────────────────────────`);
console.log(`  TOTAL BILLABLE                ${pad(billable.toFixed(2))}`);
console.log(`\n  (Worker-level requests, billed separately: ${workerRequests.length})`);

console.log("\nDO REQUESTS BY PATH");
for (const [path, n] of [...pathCounts].sort((a, b) => b[1] - a[1])) {
  console.log(`  ${pad(n)}  ${path}`);
}

console.log(`\nROWS WRITTEN ${rowsWritten}   ROWS READ ${rowsRead}`);
if (emulatorRows) console.log(`  (excluded ${emulatorRows} miniflare-internal rows — not billed by the real runtime)`);
for (const [key, e] of [...byQuery].sort((a, b) => b[1].written - a[1].written)) {
  if (e.written) console.log(`  ${pad(e.written)}  ${key}  (${e.calls} calls)`);
}
if (alarmBillable) console.log(`  ${pad(alarmBillable)}  ALARM:set/delete`);
console.log();
