import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import { ChatRoom } from "../../src/chat-room";
import { RegistryRoom } from "../../src/registry-room";
import { Notifications } from "../../src/notifications";
import { encodeFrame, decodeFrame, FRAME } from "../../src/chat-frames";
import { AUTH_USER_HEADER, type Env } from "../../src/env";
import type { Row } from "../../src/registry-core";

declare global { namespace Cloudflare { interface Env { TEST_LOG: DurableObjectNamespace } } }

declare const __ROWS_FIXTURE__: {kind: string; textBytes: number; steps: {
  at: number; update: number[]; tail: unknown; checkpoint: number[] | null;
}[]; optimized: {at:number; update:number[]}[]};
const fixture = __ROWS_FIXTURE__;

function meter(state: DurableObjectState, sockets: WebSocket[] = []) {
  let cursors: { category: string; cursor: SqlStorageCursor<Record<string, SqlStorageValue>> }[] = [];
  // `setAlarm`/`deleteAlarm` each bill one row written and never reach
  // `storage.sql`, so the SQL proxy below cannot see them — the measurement gap
  // docs/rows-written-baseline.md records as "alarm API calls are not in the
  // totals". Without this, a per-push `setAlarm` is invisible.
  let alarms: string[] = [];
  let pending: Promise<unknown>[] = [];
  const sql = new Proxy(state.storage.sql, {get(target, key) {
    if (key === "exec") return (query: string, ...params: SqlStorageValue[]) => {
      const cursor = target.exec(query, ...params);
      const verb = query.trim().split(/\s/)[0].toUpperCase();
      const table = query.match(/(?:INTO|UPDATE|FROM)\s+(\w+)/i)?.[1] ?? "schema";
      const field = table === "meta" || table === "notify_kv" ? `:${String(params[0] ?? "literal").split(":")[0]}` : "";
      cursors.push({category:`${verb}:${table}${field}`, cursor});
      return cursor;
    };
    const value = Reflect.get(target, key, target);
    return typeof value === "function" ? value.bind(target) : value;
  }});
  const storage = new Proxy(state.storage, {get(target, key) {
    if (key === "sql") return sql;
    if (key === "setAlarm" || key === "deleteAlarm") {
      const category = key === "setAlarm" ? "ALARM:set" : "ALARM:delete";
      const fn = Reflect.get(target, key, target) as (...args: unknown[]) => Promise<void>;
      return (...args: unknown[]) => { alarms.push(category); return fn.apply(target, args); };
    }
    const value = Reflect.get(target, key, target);
    return typeof value === "function" ? value.bind(target) : value;
  }});
  const context = new Proxy(state, {get(target, key) {
    if (key === "storage") return storage;
    if (key === "getWebSockets") return () => sockets;
    // Alarm scheduling runs off `waitUntil` (registry-room.ts) or a floating
    // `.then` (chat-room.ts). Collect both so `settle()` can attribute their
    // writes to the phase that caused them instead of a later one.
    if (key === "waitUntil") return (promise: Promise<unknown>) => { pending.push(promise); };
    const value = Reflect.get(target, key, target);
    return typeof value === "function" ? value.bind(target) : value;
  }});
  /** Drain deferred alarm work so `take()` sees it.
   *
   * Microtasks only: the fixture mocks `Date.now()` to a fixed past instant, so
   * `markBackupDirty`'s `now + DAY_MS` alarm is already overdue in real time.
   * Yielding to the macrotask queue lets workerd deliver that alarm mid-run,
   * which measures the mock rather than the code. `waitUntil` chains (the
   * registry's `scheduleAlarm`) settle on microtasks alone. ChatRoom's floating
   * `getAlarm().then(...)` is therefore still attributed to a later phase — one
   * `ALARM:set` for the whole run, unchanged from before this meter existed. */
  const settle = async () => {
    while (pending.length) { const batch = pending; pending = []; await Promise.allSettled(batch); }
  };
  const take = () => {
    const counters: Record<string, {calls: number; written: number; read: number}> = {};
    // Read after consumers have drained their SELECT cursors, not at exec time.
    for (const {category, cursor} of cursors) {
      const c = counters[category] ??= {calls:0,written:0,read:0};
      c.calls++; c.written += cursor.rowsWritten; c.read += cursor.rowsRead;
    }
    for (const category of alarms) {
      const c = counters[category] ??= {calls:0,written:0,read:0};
      c.calls++; c.written += 1; // one billed row per alarm API call
    }
    cursors = []; alarms = [];
    return {written:Object.values(counters).reduce((a,b)=>a+b.written,0),
      read:Object.values(counters).reduce((a,b)=>a+b.read,0), sql:counters};
  };
  return { context, take, settle };
}

function peer(device: string) {
  let attachment: unknown = {userId:"p0-user",device,ready:true};
  const frames: Uint8Array[] = [];
  const socket = {deserializeAttachment:()=>attachment, serializeAttachment:(v:unknown)=>{attachment=v;},
    send:(b:ArrayBuffer)=>frames.push(new Uint8Array(b)),close:()=>{}} as unknown as WebSocket;
  return {socket,frames};
}
const request = (path: string, method = "GET", body?: BodyInit) => new Request(`https://test${path}`, {
  method, headers:{[AUTH_USER_HEADER]:"p0-user","content-type":"application/json"}, body
});
const total = (phases: Record<string,{written:number}>) => Object.values(phases).reduce((a,b)=>a+b.written,0);

it("P0: identical real Loro history with one and three viewers", async ({task}) => {
  const results = [];
  for (const watchers of [1,3]) {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName(`p0-chat-${watchers}`));
    results.push(await runInDurableObject(stub, async (_, state) => {
      const host = peer("host"), readers = Array.from({length:watchers},(_,i)=>peer(`viewer-${i}`));
      const m = meter(state, [host.socket,...readers.map(x=>x.socket)]);
      const room = new ChatRoom(m.context, {} as Env);
      // Seed ownership without a fabricated checkpoint; host WS registration
      // itself is not simulated. Report setup separately from the push path.
      state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('owner','p0-user')");
      const setup = m.take();
      const phases: Record<string, ReturnType<typeof m.take>> = {};
      let seq = 0, tailAt = 1000;
      const clock = vi.spyOn(Date,"now");
      const add = async (name: string) => {
        await m.settle(); // deferred alarm writes belong to the phase that caused them
        const v = m.take(), old = phases[name];
        if (!old) {phases[name]=v; return;}
        old.written+=v.written;old.read+=v.read;
        for (const [key,c] of Object.entries(v.sql)) {
          const o=old.sql[key]??={calls:0,written:0,read:0};o.calls+=c.calls;o.written+=c.written;o.read+=c.read;
        }
      };
      try {
        for (const [index, step] of fixture.steps.entries()) {
          clock.mockReturnValue(1788998400000+step.at);
          await room.webSocketMessage(host.socket, encodeFrame(FRAME.push,{batchId:`b${index}`},new Uint8Array(step.update)).buffer as ArrayBuffer);
          const ack = decodeFrame(host.frames.at(-1)!)!;
          expect(ack.type).toBe(FRAME.ack);expect(ack.header.seq).toBe(++seq);
          await add("push");
          if (step.at >= tailAt || index === fixture.steps.length-1) {
            expect((await room.fetch(request("/tail","PUT",JSON.stringify(step.tail)))).status).toBe(200);
            await add("tail");tailAt=step.at+1000;
          }
          if (step.checkpoint) {
            expect((await room.fetch(request(`/checkpoint?seqCovered=${seq}`,"POST",new Uint8Array(step.checkpoint)))).status).toBe(200);
            await add("checkpoint");
          }
        }
        const wsOutboundFrames=host.frames.length+readers.reduce((n,r)=>n+r.frames.length,0);
        const wsOutboundBytes=[host,...readers].reduce((n,r)=>n+r.frames.reduce((a,b)=>a+b.byteLength,0),0);
        // Current WS duplicate path avoids all SQL writes.
        const last=fixture.steps.at(-1)!;
        // Final checkpoint pruned the last batch; use a fresh row for dedup proof.
        await room.webSocketMessage(host.socket,encodeFrame(FRAME.push,{batchId:"dedup"},new Uint8Array(last.update)).buffer as ArrayBuffer);
        m.take();
        await room.webSocketMessage(host.socket,encodeFrame(FRAME.push,{batchId:"dedup"},new Uint8Array(last.update)).buffer as ArrayBuffer);
        const duplicate=m.take();expect(duplicate.written).toBe(0);
        expect((await room.fetch(request("/rows?device=host&batchId=dedup","POST",new Uint8Array(last.update)))).status).toBe(200);
        const httpDuplicate=m.take();expect(httpDuplicate.written).toBe(0);
        for(const reader of readers) expect(reader.frames.length).toBe(fixture.steps.length+1);
        return {watchers,fixture:fixture.kind,textBytes:fixture.textBytes,batches:fixture.steps.length,
          setup,phases,totalWritten:total(phases),duplicate,httpDuplicate,
          wsOutboundFrames,wsOutboundBytes};
      } finally {clock.mockRestore();await state.storage.deleteAlarm();}
    }));
  }
  expect(results[0].totalWritten).toBe(results[1].totalWritten);
  Object.assign(task.meta, {baseline:{ROWS_BASELINE_CHAT:results}});
});

it("P0: SQL engine counts index writes, not merely INSERT statements", async ({task}) => {
  const counts = await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("p0-index")), (_,state)=>{
    const sql=state.storage.sql;
    sql.exec("CREATE TABLE unindexed (id INTEGER PRIMARY KEY, value TEXT)");
    sql.exec("CREATE TABLE indexed (id INTEGER PRIMARY KEY, value TEXT UNIQUE)");
    const plain=sql.exec("INSERT INTO unindexed VALUES(1,'a')").rowsWritten;
    const indexed=sql.exec("INSERT INTO indexed VALUES(1,'a')").rowsWritten;
    expect(indexed).toBeGreaterThan(plain);
    return {plain,indexed};
  });
  Object.assign(task.meta, {baseline:{ROWS_BASELINE_INDEX:counts}});
});

it("P0: registry heartbeat and notification event/activity writes", async ({task}) => {
  const registry = await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("p0-registry")), async (_,state)=>{
    const m=meter(state), room=new RegistryRoom(m.context,{NOTIFICATIONS_ENABLED:"false"} as Env);
    m.take();
    const registry=[];
    for(let i=0;i<4;i++) {
      const op={kind:"sessions",id:"chat",op:"upsert",hlc:`1788998400000-${String(i).padStart(6,"0")}-host`,
        set:{chatId:"chat",deviceId:"host",status:"working",updatedAt:i}};
      const r=await room.fetch(request("/push?device=host","POST",JSON.stringify({batch:`b${i}`,ops:[op]})));
      expect(r.status).toBe(200);await m.settle();registry.push(m.take());
    }
    await state.storage.deleteAlarm();
    return registry;
  });
  const notifications = await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("p0-notifications")), async (_,state)=>{
    const m=meter(state), start=Date.now();
    const rows=new Map<string,Row>([
      ["chats/chat",{kind:"chats",id:"chat",seq:1,deleted:false,clocks:{},fields:{deviceId:"host",spaceId:"space"}}],
      ["spaces/space",{kind:"spaces",id:"space",seq:1,deleted:false,clocks:{},fields:{}}]
    ]);
    const service=new Notifications(m.context,{NOTIFICATIONS_ENABLED:"true",APNS_TEAM_ID:"TEAM123456",
      APNS_KEY_ID:"TESTKEY001",APNS_PRIVATE_KEY:"test",PUSH_DEVICES:{} as DurableObjectNamespace} as Env,(kind,id)=>rows.get(`${kind}/${id}`),()=>{});
    m.take();const events=[],activity=[];
    for(let i=0;i<4;i++) {
      const r=await service.fetch(request("/event","POST",JSON.stringify({chatId:"chat",deviceId:"host",status:"working",updatedAt:start+i,startedAt:start})),"event");
      expect(r.status).toBe(200);events.push(m.take());
      const a=await service.fetch(request("/activity","POST",JSON.stringify({clientId:"desktop",platform:"desktop",foreground:false,chatId:null,sequence:i+1,interactionAgeMs:0})),"activity");
      expect(a.status).toBe(200);expect((await a.json() as {available:boolean}).available).toBe(true);activity.push(m.take());
    }
    return {events,activity};
  });
  Object.assign(task.meta, {baseline:{ROWS_BASELINE_REGISTRY:registry,ROWS_BASELINE_NOTIFICATIONS:notifications}});
});

it("P2 experiment: cumulative Loro export every 2s against the same ChatRoom", async ({task}) => {
  const result = await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("p2-cumulative-chat")), async (_, state) => {
    const host = peer("p2-host"), m = meter(state, [host.socket]);
    const room = new ChatRoom(m.context, {} as Env);
    state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('owner','p0-user')");
    const setup = m.take();
    let seq = 0; const phases: Record<string, ReturnType<typeof m.take>> = {};
    const add = async (name: string) => { await m.settle(); const value = m.take(); const old = phases[name]; if (!old) { phases[name] = value; return; }
      old.written += value.written; old.read += value.read; for (const [k,c] of Object.entries(value.sql)) { const o = old.sql[k] ??= {calls:0,written:0,read:0}; o.calls += c.calls; o.written += c.written; o.read += c.read; } };
    let optimizedIndex = 0, tailAt = 1000;
    for (const step of fixture.steps) {
      while (optimizedIndex < fixture.optimized.length && fixture.optimized[optimizedIndex]!.at <= step.at) {
        const update = fixture.optimized[optimizedIndex++]!.update;
        await room.webSocketMessage(host.socket, encodeFrame(FRAME.push, { batchId: `p2-${++seq}` }, new Uint8Array(update)).buffer as ArrayBuffer);
        expect(decodeFrame(host.frames.at(-1)!)?.type).toBe(FRAME.ack); await add("push");
      }
      if (step.at >= tailAt || step === fixture.steps.at(-1)) {
        expect((await room.fetch(request("/tail", "PUT", JSON.stringify(step.tail)))).status).toBe(200); await add("tail"); tailAt = step.at + 1000;
      }
      if (step.checkpoint) {
        const checkpointRequest = new Request(`https://test/checkpoint?seqCovered=${seq}`, { method:"POST", headers:{[AUTH_USER_HEADER]:"p0-user","content-type":"application/octet-stream","x-chat2-frontier":""}, body:new Uint8Array(step.checkpoint) });
        expect((await room.fetch(checkpointRequest)).status).toBe(200); await add("checkpoint");
      }
    }
    return { fixture:"text-240x120ms-cumulative-2s", watchers:1, batches:seq, textBytes:fixture.textBytes, setup, phases, totalWritten:total(phases), pushPayloadBytes:host.frames.filter(f=>decodeFrame(f)?.type===FRAME.ack).map(f=>f.byteLength).reduce((a,b)=>a+b,0) };
  });
  Object.assign(task.meta, {baseline:{ROWS_BASELINE_CHAT_CUMULATIVE:result}});
});
