import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ChatRoom } from "../../src/chat-room";
import { AUTH_USER_HEADER, type Env } from "../../src/env";
import { PREVIEW_CAPABILITY_HEADER, PREVIEW_PUBLISH_HEADER } from "../../src/development-preview";
import { encodeFrame, decodeFrame, FRAME } from "../../src/chat-frames";
import { encodeStream, decodeStream, STREAM, STREAM_CAPABILITY } from "../../src/stream-preview";
import { previewTestEnv, type TestPreviewRoom } from "./preview-fixture";

declare global { namespace Cloudflare { interface Env { TEST_PREVIEW: DurableObjectNamespace<TestPreviewRoom>; TEST_LOG: DurableObjectNamespace } } }

const binary = (bytes: Uint8Array) => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
function request(publisher = false, token = previewTestEnv.DEV_PREVIEW_PUBLISH_TOKEN) {
  return new Request("https://room/ws?chatId=c", { headers: { upgrade: "websocket", [AUTH_USER_HEADER]: "local-user",
    [PREVIEW_CAPABILITY_HEADER]: STREAM_CAPABILITY, ...(publisher ? { [PREVIEW_PUBLISH_HEADER]: token } : {}) } });
}
function inbox(ws: WebSocket) {
  ws.binaryType = "arraybuffer";
  const buffered: Uint8Array[] = [];
  let wake: (() => void) | undefined;
  ws.addEventListener("message", e => { buffered.push(new Uint8Array(e.data as ArrayBuffer)); wake?.(); });
  const next = async (matches: (b: Uint8Array) => boolean): Promise<Uint8Array> => {
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      const index = buffered.findIndex(matches);
      if (index >= 0) return buffered.splice(index, 1)[0]!;
      await new Promise<void>(resolve => { const timer = setTimeout(resolve, 20); wake = () => { clearTimeout(timer); resolve(); }; });
      wake = undefined;
    }
    throw new Error(`Expected preview frame not received: ${JSON.stringify(buffered.map(b => ({ length: b.length, header: decodeStream(b)?.header ?? decodeFrame(b)?.header })))}`);
  };
  return { next, buffered };
}
const mode = (value: string) => (bytes: Uint8Array) => decodeStream(bytes)?.header.mode === value;
const kind = (value: number) => (bytes: Uint8Array) => decodeStream(bytes)?.kind === value;
async function clients(name: string, watchers = 1) {
  const stub = env.TEST_PREVIEW.get(env.TEST_PREVIEW.idFromName(name));
  const host = (await stub.fetch(request(true))).webSocket!;
  const viewers: WebSocket[] = [];
  for (let i = 0; i < watchers; i++) viewers.push((await stub.fetch(request())).webSocket!);
  host.accept(); const hi = inbox(host);
  const inboxes = viewers.map(ws => { ws.accept(); return inbox(ws); });
  const viewer = viewers[0]!, vi = inboxes[0]!;
  host.send(encodeFrame(FRAME.hello, { device: "same-id", cursor: 0 }));
  for (const ws of viewers) ws.send(encodeFrame(FRAME.hello, { device: "same-id", cursor: 0 })); // Impersonated ID has no authority.
  await hi.next(mode("ready")); for (const input of inboxes) await input.next(mode("ready"));
  host.send(encodeStream(STREAM.start, { chatId: "c", runId: "r", segmentId: "s" })!);
  const state = decodeStream(await hi.next(mode("preview")))!;
  for (const input of inboxes) await input.next(mode("preview"));
  const { mode: _, ...scope } = state.header;
  const header: Record<string, unknown> = { ...scope, revision: 0, baseSeq: 0 };
  return { stub, host, viewer, hi, vi, header, viewers, inboxes };
}

describe("development preview through real ChatRoom/workerd", () => {
  it("broadcasts to three real viewers without SQL amplification", async () => {
    const s = await clients("preview-three-viewers", 3);
    try {
      await runInDurableObject<TestPreviewRoom, void>(s.stub, instance => { instance.writes = 0; instance.sqlCalls = 0; });
      s.host.send(encodeStream(STREAM.snapshot, s.header, "")!);
      for (const input of s.inboxes) await input.next(kind(STREAM.snapshot));
      for (let revision = 1; revision <= 10; revision++) {
        const header = { ...s.header, revision, prevRevision: revision - 1 };
        const expected = encodeStream(STREAM.delta, header, "你好 👋")!;
        s.host.send(expected);
        for (const input of s.inboxes) expect(await input.next(kind(STREAM.delta))).toEqual(expected);
      }
      expect(await runInDurableObject<TestPreviewRoom, number[]>(s.stub, instance => [instance.writes, instance.sqlCalls])).toEqual([0, 0]);
    } finally { s.host.close(); for (const ws of s.viewers) ws.close(); }
  });
  it("relays real sockets with zero SQL, while old durable PUSH still writes", async () => {
    const s = await clients("preview-real-ws");
    try {
      await runInDurableObject<TestPreviewRoom, void>(s.stub, instance => { instance.writes = 0; instance.sqlCalls = 0; });
      s.host.send(encodeStream(STREAM.snapshot, s.header, "你好")!);
      expect(decodeStream(await s.vi.next(kind(STREAM.snapshot)))?.text).toBe("你好");
      s.viewer.send(encodeStream(STREAM.receipt, s.header)!);
      const delta = { ...s.header, revision: 1, prevRevision: 0 };
      s.host.send(encodeStream(STREAM.delta, delta, " 🌍")!);
      expect(await s.vi.next(kind(STREAM.delta))).toEqual(encodeStream(STREAM.delta, delta, " 🌍"));
      s.viewer.send(encodeStream(STREAM.resume, { ...s.header, revision: 1 })!);
      await s.hi.next(kind(STREAM.resume));
      expect(await runInDurableObject<TestPreviewRoom, number[]>(s.stub, instance => [instance.writes, instance.sqlCalls])).toEqual([0, 0]);
      s.viewer.send(encodeStream(STREAM.snapshot, { ...s.header, revision: 1 }, "forged")!);
      const error = decodeFrame(await s.vi.next(bytes => decodeFrame(bytes)?.type === FRAME.error))!;
      expect(error.header.code).toBe("preview_publisher_required");
      s.host.send(encodeFrame(FRAME.push, { batchId: "durable-b1" }, new Uint8Array([1, 2, 3])));
      const ack = decodeFrame(await s.hi.next(bytes => decodeFrame(bytes)?.type === FRAME.ack))!;
      expect(ack.header.seq).toBe(1);
      expect(await runInDurableObject<TestPreviewRoom, number>(s.stub, instance => instance.writes)).toBeGreaterThan(0);
    } finally { s.host.close(); s.viewer.close(); }
  });
  it("cannot authorize with a viewer credential or a missing user identity", async () => {
    const stub = env.TEST_PREVIEW.get(env.TEST_PREVIEW.idFromName("preview-auth"));
    expect((await stub.fetch(request(true, previewTestEnv.DEV_ACCESS_TOKEN))).status).toBe(403);
    const unauthenticated = request(true); unauthenticated.headers.delete(AUTH_USER_HEADER);
    expect((await stub.fetch(unauthenticated)).status).toBe(401);
    expect(await runInDurableObject<TestPreviewRoom, number>(stub, (_, ctx) =>
      [...ctx.storage.sql.exec("SELECT value FROM meta WHERE key='owner'")].length)).toBe(0);
  });
  it("fails closed after simulated cold application state with surviving WSs", async () => {
    const s = await clients("preview-cold-ws");
    try {
      await runInDurableObject<TestPreviewRoom, void>(s.stub, instance => instance.cold());
      const closed = new Promise<number>(resolve => s.host.addEventListener("close", event => resolve(event.code), { once: true }));
      s.host.send(encodeStream(STREAM.snapshot, s.header, "stale")!);
      expect(await closed).toBe(1013);
    } finally { s.host.close(); s.viewer.close(); }
  });
  it("room reset closes preview sockets using the existing reset contract", async () => {
    const s = await clients("preview-reset-ws");
    try {
      const closed = new Promise<number>(resolve => s.viewer.addEventListener("close", e => resolve(e.code), { once: true }));
      const response = await s.stub.fetch("https://room/reset", { method: "POST", headers: { [AUTH_USER_HEADER]: "local-user" } });
      expect(response.status).toBe(200); expect(await closed).toBe(4410);
    } finally { s.host.close(); s.viewer.close(); }
  });
  it("production ChatRoom ignores preview configuration without the dev-only injection", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("preview-production-off"));
    await runInDurableObject(stub, async (_, ctx) => {
      const room = new ChatRoom(ctx, previewTestEnv as unknown as Env);
      const sent: Uint8Array[] = [];
      const ws = { send: (bytes: ArrayBuffer) => sent.push(new Uint8Array(bytes)), deserializeAttachment: () => ({ userId: "u", device: "d", ready: true }) } as unknown as WebSocket;
      await room.webSocketMessage(ws, binary(encodeStream(STREAM.start, { chatId: "c", runId: "r", segmentId: "s" })!));
      expect(decodeFrame(sent[0]!)?.header.code).toBe("bad_frame");
    });
  });
});
