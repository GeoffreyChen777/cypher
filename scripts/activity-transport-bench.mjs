#!/usr/bin/env node
/**
 * Measure what the viewport's activity heartbeat costs on each transport.
 *
 * Both regimes do the *same work*: keep the `target:{chatId}` lease alive so
 * the edge suppresses a push for a chat that is already on screen. They differ
 * only in how the report travels.
 *
 *   before  one HTTP POST /notifications/activity per beat   (bills 1:1)
 *   after   the report rides the presence beat already sent  (bills 20:1)
 *
 * Billing, from the Durable Object pricing model:
 *   billable = http requests + alarms + (inbound websocket messages / 20)
 *
 * Usage: node scripts/activity-transport-bench.mjs [--port N] [--beats N]
 */
const arg = (k, d) => {
  const i = process.argv.indexOf(k);
  return i > 0 ? process.argv[i + 1] : d;
};
const PORT = Number(arg("--port", 27672));
const BEATS = Number(arg("--beats", 40));
const BASE = `http://127.0.0.1:${PORT}`;
const ORG = "org1", USER = "alice", CHAT = "chat-bench";

const sql = async q => {
  const r = await fetch(`${BASE}/cdn-cgi/local/explorer/api/local/observability/query`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ sql: q })
  });
  return (await r.json()).result.rows;
};
const mark = () => Date.now();
/** Billable Durable Object requests recorded since `since`.
 *
 * Only Durable Object requests bill on this meter, and one client HTTP call
 * produces two spans: a root span for the Worker and a child span for the
 * object it forwarded to. Counting both would double the "before" figure, so
 * restrict to child spans. Websocket and alarm spans are object-side already.
 */
const billed = async since => {
  const rows = await sql(
    `SELECT name, COUNT(*) n FROM spans WHERE start_ms >= ${since} ` +
    `AND parent_id IS NOT NULL ` +
    `AND name IN ('GET','POST','PUT','DELETE','hibernatableWebSocket','alarm') GROUP BY name`);
  const c = Object.fromEntries(rows.map(([n, v]) => [n, v]));
  const http = (c.GET ?? 0) + (c.POST ?? 0) + (c.PUT ?? 0) + (c.DELETE ?? 0);
  const ws = c.hibernatableWebSocket ?? 0, alarm = c.alarm ?? 0;
  return { http, ws, alarm, billable: http + alarm + ws / 20 };
};

const report = (seq, chatId) => ({
  clientId: "bench-client", sequence: seq, platform: "desktop",
  foreground: true, interactionAgeMs: 0, chatId
});

async function joined(device) {
  const ws = new WebSocket(
    `ws://127.0.0.1:${PORT}/registry/${ORG}/ws?token=${USER}@${ORG}&device=${device}`);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  ws.send(JSON.stringify({ t: "hello", cursor: null, device }));
  await new Promise(r => setTimeout(r, 500));   // let the room answer hello
  return ws;
}

async function main() {
  console.log(`Simulating ${BEATS} activity beats per regime (one client, one chat).\n`);

  // ── BEFORE: presence beat on the socket AND a separate HTTP report ───────
  const wsA = await joined("bench-before");
  await new Promise(r => setTimeout(r, 300));
  const m1 = mark();
  for (let i = 0; i < BEATS; i++) {
    wsA.send(JSON.stringify({ t: "presence", at: Date.now() }));
    await fetch(`${BASE}/registry/${ORG}/notifications/activity`, {
      method: "POST",
      headers: { authorization: `Bearer ${USER}@${ORG}`, "content-type": "application/json" },
      body: JSON.stringify(report(1000 + i, CHAT))
    });
    await new Promise(r => setTimeout(r, 25));
  }
  await new Promise(r => setTimeout(r, 1200));
  const before = await billed(m1);
  wsA.close();
  await new Promise(r => setTimeout(r, 600));

  // ── AFTER: the same report rides the presence beat ───────────────────────
  const wsB = await joined("bench-after");
  await new Promise(r => setTimeout(r, 300));
  const m2 = mark();
  for (let i = 0; i < BEATS; i++) {
    wsB.send(JSON.stringify({
      t: "presence", at: Date.now(), activity: report(2000 + i, CHAT)
    }));
    await new Promise(r => setTimeout(r, 25));
  }
  await new Promise(r => setTimeout(r, 1200));
  const after = await billed(m2);
  wsB.close();

  const row = (k, v) => `  ${k.padEnd(26)} ${String(v.http).padStart(5)} ${String(v.ws).padStart(6)} ` +
    `${String(v.alarm).padStart(6)} ${v.billable.toFixed(2).padStart(9)}`;
  console.log("  regime                       http     ws  alarm  billable");
  console.log(row("before (HTTP heartbeat)", before));
  console.log(row("after  (rides presence)", after));

  const cut = before.billable / Math.max(after.billable, 0.0001);
  console.log(`\n  per-beat billable: ${(before.billable / BEATS).toFixed(3)} -> ` +
    `${(after.billable / BEATS).toFixed(3)}`);
  console.log(`  reduction on this path: ${cut.toFixed(1)}x ` +
    `(${(100 * (1 - after.billable / before.billable)).toFixed(1)}% fewer)`);

  // The saving is only real because the work still happens: that both
  // transports store byte-identical activity state is proven separately, in
  // edge/test/workerd/activity-presence.workerd.test.ts.
  process.exit(0);
}
main().catch(e => { console.error(e); process.exit(1); });
