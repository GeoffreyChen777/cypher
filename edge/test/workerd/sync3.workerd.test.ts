import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import golden from "../../../fixtures/sync3/golden.json";
import invalid from "../../../fixtures/sync3/invalid.json";
import numbers from "../../../fixtures/sync3/numbers.json";
import runCommand from "../../../fixtures/sync3/run-command.json";
import lifecycle from "../../../fixtures/sync3/command-lifecycle.json";
import { Sync3Log } from "../../src/sync3-log";
import { validateOperation, type Operation, type Reply } from "../../src/sync3-protocol";

function inLog<T>(name: string, work: (log: Sync3Log) => T): Promise<T> {
  const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName(`sync3-${name}`));
  return runInDurableObject(stub, (_, state) => work(new Sync3Log(state.storage)));
}
const ops = () => golden.operations.map(validateOperation);
const queued = (id: string, text = "hello"): Operation => validateOperation({
  id, actor: "phone", ownerEpoch: 1,
  event: { type: "commandQueued", commandId: id, command: { ...runCommand, id, payload: {
    ...runCommand.payload, request: { ...runCommand.payload.request, prompt: text },
  } } },
});
describe("sync3 real SQLite commit/receipt boundary", () => {
  it.each(lifecycle)("persists sparse command lifecycle and rejects poison rows: $name", async scenario => {
    await inLog(`lifecycle-${scenario.name}`, log => {
      log.initialize("account", "host"); log.append([ops()[0]]);
      scenario.steps.forEach((step, index) => {
        const op = validateOperation({ id: `step-${index}`, actor: step.actor, ownerEpoch: "ownerEpoch" in step ? step.ownerEpoch : 1, event: step.event });
        const before = log.get("commands", "command"), head = log.state().head;
        if ("error" in step && step.error) {
          expect(() => log.append([op])).toThrow(step.error);
          expect(log.get("commands", "command")).toEqual(before);
          expect(log.state().head).toBe(head);
        } else log.append([op]);
      });
    });
    await inLog(`lifecycle-${scenario.name}`, log => {
      expect(log.get("commands", "command")?.command.status).toBe(scenario.status);
      if (scenario.name === "rejected-before-run") {
        expect(log.hasAcceptedRun("run")).toBe(false);
        log.transferOwner(1, "new-host");
      }
      if (scenario.name === "applied-and-immutable") {
        expect(() => log.transferOwner(1, "new-host")).toThrow("execution_unresolved");
      }
    });
  });
  it("canonical numeric input answers deduplicate and persist consistently", async () => {
    await inLog("numeric-domain", log => {
      log.initialize("account", "host");
      const ack = log.append([validateOperation(numbers.source)]);
      expect(log.append([validateOperation(numbers.canonical)])).toEqual(ack);
      expect(log.state().head).toBe(1);
      expect(log.get("commands", "numeric-command")?.command).toEqual(numbers.canonical.event.command);
    });
  });
  it("shared invalid vectors never commit a poison row", async () => {
    await inLog("invalid-domain", log => {
      log.initialize("account", "host");
      for (const operation of invalid) {
        expect(() => log.append([operation as unknown as Operation])).toThrow();
        expect(log.state().head).toBe(0);
      }
    });
  });
  it("lost-ACK replay retains exact receipt and immutable history", async () => {
    await inLog("replay", log => {
      log.initialize("account", "host");
      const first = log.append(ops());
      expect(first.receipts.map(r => r.seq)).toEqual([1,2,3,4,5,6,7,8,9,10]);
      expect(log.append(ops())).toEqual(first);
      expect(log.state().head).toBe(10);
    });
    await inLog("replay", log => {
      // New object model over the persisted tables, not cached dedupe state.
      expect(log.append(ops()).receipts.at(-1)?.seq).toBe(10);
      expect(log.get("messages", "message")?.text).toBe("你好!");
    });
  });
  it("invalid last operation rolls back earlier event, projection and head", async () => {
    await inLog("rollback", log => {
      log.initialize("account", "host");
      const bad = { ...queued("bad"), event: { type: "runStarted", runId: "absent" }, actor: "host" } as Operation;
      expect(() => log.append([queued("good"), bad])).toThrow("run_not_accepted");
      expect(log.state().head).toBe(0);
      expect(log.get("commands", "good")).toBeUndefined();
      expect(log.append([queued("good")]).receipts[0].seq).toBe(1);
    });
  });
  it("conflicting IDs never mutate or consume another sequence", async () => {
    await inLog("conflict", log => {
      log.initialize("account", "host");
      log.append([queued("one")]);
      expect(() => log.append([{ ...queued("two"), id: "one" }])).toThrow("operation_id_conflict");
      expect(log.state().head).toBe(1);
      expect(() => log.initialize("other-account", "host")).toThrow("forbidden");
      expect(() => log.append([queued("two")], "host")).toThrow("actor_mismatch");
    });
  });
  it("fixed page ceiling excludes concurrent writes and rejects rollback epochs", async () => {
    await inLog("paging", log => {
      log.initialize("account", "host");
      log.append([queued("one"), queued("two")]);
      const through = log.state().head;
      log.append([queued("three")]);
      expect(log.page(1,0,through).rows.map(r=>r.seq)).toEqual([1,2]);
      expect(log.page(1,0,through).done).toBe(true);
      expect(() => log.page(2,0,through)).toThrow("epoch_mismatch");
      expect(() => log.page(1,4,4)).toThrow("invalid_cursor");
    });
  });
  it("bounds page bytes and resumes at the last delivered event", async () => {
    await inLog("bytes", log => {
      log.initialize("account", "host");
      const large = (id: string) => queued(id, "x".repeat(110_000));
      for (let i=0;i<5;i++) log.append([large(`large-${i}`)]);
      const first=log.page(1,0,5);
      expect(first.rows.length).toBe(2); expect(first.done).toBe(false);
      expect(new TextEncoder().encode(JSON.stringify(first)).length).toBeLessThan(256*1024);
      const next=log.page(1,first.next,5);
      expect(next.rows[0].seq).toBe(3);
    });
  });
  it("ownership transfer is fenced and refuses uncertain execution", async () => {
    await inLog("owner", log => {
      log.initialize("account", "host"); log.append(ops().slice(0,2));
      expect(()=>log.transferOwner(1,"new-host")).toThrow("execution_unresolved");
      log.append(ops().slice(2)); log.transferOwner(1,"new-host");
      expect(log.state().ownerEpoch).toBe(2);
      expect(()=>log.append([queued("old")])).toThrow("stale_owner_epoch");
      // Previously committed operations remain safely deduplicated.
      expect(log.append(ops()).receipts[0].seq).toBe(1);
      expect(()=>log.transferOwner(1,"third")).toThrow("stale_owner_epoch");
    });
  });
});

describe("sync3 direct WebSocket room", () => {
  function stub(name: string) { return env.TEST_SYNC3.get(env.TEST_SYNC3.idFromName(name)); }
  async function exchange(room: DurableObjectStub, body: unknown, account="account") {
    const response = await room.fetch("https://test/exchange", {
      method:"POST", headers:{"x-cypher-auth-user":account}, body:JSON.stringify(body),
    });
    return response;
  }
  async function socket(room: DurableObjectStub) {
    const response=await room.fetch("https://test/ws", {headers:{"x-cypher-auth-user":"account",
      "x-cypher-auth-deadline":String(Date.now()+300_000),Upgrade:"websocket"}});
    const ws=response.webSocket!;
    ws.accept(); return ws;
  }
  function next(ws: WebSocket): Promise<Reply> {
    return new Promise((resolve,reject) => {
      const timeout=setTimeout(()=>reject(new Error("WS response timeout")),3000);
      ws.addEventListener("message",event=>{clearTimeout(timeout);resolve(JSON.parse(String(event.data)));},{once:true});
    });
  }
  it("auth, hello, live head, durable ACK and cursor resume use real sockets", async () => {
    const room=stub("socket");
    expect((await room.fetch("https://test/ws")).status).toBe(401);
    await room.fetch("https://test/init",{method:"POST",headers:{"x-cypher-auth-user":"account"},body:'{"owner":"host"}'});
    expect((await room.fetch("https://test/ws", {headers:{"x-cypher-auth-user":"account",
      "x-cypher-auth-deadline":"1",Upgrade:"websocket"}})).status).toBe(401);
    expect((await exchange(room,{type:"probe",version:3},"other")).status).toBe(403);
    const ws=await socket(room);
    try {
      let received=next(ws); ws.send(JSON.stringify({type:"probe",version:3}));
      expect(await received).toMatchObject({type:"error",code:"hello_required"});
      received=next(ws);ws.send(JSON.stringify({type:"hello",version:3,actor:"phone",epoch:0,after:0}));
      expect(await received).toMatchObject({type:"state",head:0,epoch:1});
      received=next(ws);
      const ack=await exchange(room,{type:"push",version:3,operations:[queued("remote")]});
      expect(await ack.json()).toMatchObject({type:"ack",receipts:[{id:"remote",seq:1}]});
      expect(await received).toMatchObject({type:"state",head:1});
      received=next(ws);ws.send(JSON.stringify({type:"pull",version:3,epoch:1,after:0,through:1}));
      expect(await received).toMatchObject({type:"page",done:true,next:1,rows:[{seq:1}]});
    } finally { ws.close(); }
    const rejoined=await socket(room);
    try {
      const received=next(rejoined);rejoined.send(JSON.stringify({type:"hello",version:3,actor:"phone",epoch:1,after:1}));
      expect(await received).toMatchObject({type:"state",head:1});
    } finally {rejoined.close();}
  });
});
