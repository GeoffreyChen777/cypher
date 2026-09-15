import { afterEach, describe, expect, it, vi } from "vitest";
import { createDevelopmentPreview, DevelopmentPreviewRelay, PREVIEW_CAPABILITY_HEADER, PREVIEW_PUBLISH_HEADER, PREVIEW_LIMITS } from "./development-preview";
import { decodeStream, encodeStream, STREAM, STREAM_CAPABILITY } from "./stream-preview";
import { decodeFrame, FRAME } from "./chat-frames";

const secret = "a".repeat(64); // Public, local test fixture only.
const configuration = { AUTH_MODE: "dev-locked", DEV_ACCESS_TOKEN: "test-viewer", DEV_PREVIEW_ENABLED: "true", DEV_PREVIEW_PUBLISH_TOKEN: secret };
function request(publisher = false, capable = true, token = secret) {
  const headers = new Headers();
  if (capable) headers.set(PREVIEW_CAPABILITY_HEADER, STREAM_CAPABILITY);
  if (publisher) headers.set(PREVIEW_PUBLISH_HEADER, token);
  return new Request("https://room/ws?chatId=c", { headers });
}
function socket() {
  const frames: Uint8Array[] = [];
  const socket = { readyState: 1, send: (b: Uint8Array) => frames.push(b),
    close: vi.fn(() => { socket.readyState = 3; }) };
  return { socket: socket as unknown as WebSocket, frames, close: socket.close };
}
function setup() {
  const sockets: WebSocket[] = [];
  const relay = new DevelopmentPreviewRelay(() => sockets, secret);
  function join(publisher = false, capable = true) {
    const p = socket();
    const admission = relay.admit(request(publisher, capable));
    if (admission instanceof Response) throw new Error("admission failed");
    sockets.push(p.socket); relay.joined(p.socket, admission); relay.hello(p.socket);
    return p;
  }
  const author = join(true), viewer = join();
  const send = (p: ReturnType<typeof socket>, kind: number, header: Record<string, unknown>, text = "") => {
    expect(relay.message(p.socket, encodeStream(kind, header, text)!)).toBe(true);
  };
  function start(): Record<string, unknown> {
    send(author, STREAM.start, { chatId: "c", runId: "r", segmentId: "s" });
    const state = decodeStream(author.frames.at(-1)!)!;
    expect(state.header.mode).toBe("preview");
    const { mode: _, ...scope } = state.header;
    return { ...scope, revision: 0, baseSeq: 0 };
  }
  const error = (p: ReturnType<typeof socket>) => decodeFrame(p.frames.at(-1)!)?.header.code;
  return { relay, join, author, viewer, send, start, error, sockets };
}
afterEach(() => vi.restoreAllMocks());

describe("development preview authorization and bounded relay", () => {
  it("requires all development gates and a distinct, well-formed secret", () => {
    const ctx = { getWebSockets: () => [] } as unknown as DurableObjectState;
    for (const patch of [{ AUTH_MODE: "workos" }, { AUTH_MODE: "dev" }, { DEV_PREVIEW_ENABLED: "false" },
      { DEV_ACCESS_TOKEN: "" }, { DEV_PREVIEW_PUBLISH_TOKEN: "" }, { DEV_PREVIEW_PUBLISH_TOKEN: secret + "\n" },
      { DEV_ACCESS_TOKEN: secret }]) {
      expect(createDevelopmentPreview(ctx, { ...configuration, ...patch })).toBeUndefined();
    }
    expect(createDevelopmentPreview(ctx, configuration)).toBeDefined();
  });
  it("rejects a login token as a publishing credential without echoing it", async () => {
    const s = setup();
    for (const r of [request(true, true, "test-viewer"), request(true, false), request(true, true, "b".repeat(64))]) {
      const response = s.relay.admit(r) as Response;
      expect(response.status).toBe(403); expect(await response.text()).not.toContain(secret);
    }
    expect((s.relay.admit(new Request("https://room/ws?chatId=other")) as Response).status).toBe(400);
  });
  it("does not trust a viewer claiming the author IDs or a different room", () => {
    const s = setup(), h = s.start();
    s.send(s.viewer, STREAM.snapshot, h, "forged"); expect(s.error(s.viewer)).toBe("preview_publisher_required");
    s.send(s.author, STREAM.snapshot, { ...h, chatId: "other" }, "x"); expect(s.error(s.author)).toBe("bad_preview_frame");
    s.send(s.author, STREAM.snapshot, { ...h, epoch: "forged" }, "x"); expect(s.error(s.author)).toBe("preview_stale_grant");
    s.send(s.viewer, STREAM.start, { chatId: "c", runId: "r", segmentId: "s" }); expect(s.error(s.viewer)).toBe("preview_not_ready");
  });
  it("starts from a snapshot, rejects revision gaps, and routes bytes unchanged", () => {
    const s = setup(), h = s.start();
    s.send(s.author, STREAM.delta, { ...h, revision: 1, prevRevision: 0 }, "x"); expect(s.error(s.author)).toBe("preview_snapshot_required");
    s.send(s.author, STREAM.snapshot, h, "你好"); expect(decodeStream(s.viewer.frames.at(-1)!)?.text).toBe("你好");
    const delta = { ...h, revision: 1, prevRevision: 0 };
    s.send(s.author, STREAM.delta, delta, " 👩🏽‍💻");
    expect(s.viewer.frames.at(-1)).toEqual(encodeStream(STREAM.delta, delta, " 👩🏽‍💻"));
    s.send(s.author, STREAM.delta, delta, "duplicate"); expect(s.error(s.author)).toBe("preview_snapshot_required");
  });
  it("revokes on legacy join without sending new frames to the legacy peer", () => {
    const s = setup(), old = s.start(), legacy = s.join(false, false);
    expect(legacy.frames).toHaveLength(0);
    expect(decodeStream(s.author.frames.at(-1)!)?.header.mode).toBe("legacy");
    s.send(s.author, STREAM.snapshot, old, "late"); expect(s.error(s.author)).toBe("preview_stale_grant");
    s.send(s.author, STREAM.start, { chatId: "c", runId: "r", segmentId: "s" }); expect(s.error(s.author)).toBe("preview_not_ready");
    s.relay.left(legacy.socket);
    expect(s.start().epoch).not.toBe(old.epoch);
  });
  it("requires fresh grants after capable membership changes or author replacement", () => {
    const s = setup(), old = s.start();
    s.join(); const next = s.start(); expect(next.epoch).not.toBe(old.epoch);
    s.send(s.author, STREAM.snapshot, old, "late"); expect(s.error(s.author)).toBe("preview_stale_grant");
    s.join(true); expect(s.author.close).toHaveBeenCalled();
    s.send(s.author, STREAM.snapshot, next, "old socket");
    expect(decodeStream(s.viewer.frames.at(-1)!)?.header.mode).toBe("ready");
  });
  it("does not revoke the successor on a replaced publisher's late close", () => {
    const s = setup(); s.start(); const replacement = s.join(true);
    s.send(replacement, STREAM.start, { chatId: "c", runId: "r2", segmentId: "s2" });
    const grant = decodeStream(replacement.frames.at(-1)!)!.header;
    s.relay.left(s.author.socket);
    s.relay.message(s.author.socket, new Uint8Array([0x20]));
    const { mode: _, ...scope } = grant;
    s.send(replacement, STREAM.snapshot, { ...scope, revision: 0, baseSeq: 0 }, "successor");
    expect(decodeStream(s.viewer.frames.at(-1)!)?.text).toBe("successor");
  });
  it("requires HELLO from every new connection and revokes on reset", () => {
    const s = setup(), h = s.start(), pending = socket();
    const admission = s.relay.admit(request());
    if (admission instanceof Response) throw new Error("unexpected rejection");
    s.sockets.push(pending.socket); s.relay.joined(pending.socket, admission);
    s.send(s.author, STREAM.start, { chatId: "c", runId: "r", segmentId: "s" }); expect(s.error(s.author)).toBe("preview_not_ready");
    s.relay.hello(pending.socket); s.start(); s.relay.reset();
    const before = s.viewer.frames.length;
    s.send(s.author, STREAM.snapshot, h, "after reset"); expect(s.viewer.frames.length).toBe(before);
  });
  it("does not restore authorization from sockets surviving a cold instance", () => {
    const s = setup(), h = s.start();
    const cold = new DevelopmentPreviewRelay(() => s.sockets, secret);
    cold.hello(s.author.socket);
    cold.message(s.author.socket, encodeStream(STREAM.snapshot, h, "stale")!);
    expect(s.author.close).toHaveBeenCalled();
  });
  it("expires an idle grant without a timer", () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1000);
    const s = setup(), h = s.start();
    now.mockReturnValue(1000 + PREVIEW_LIMITS.idleMs + 1);
    s.send(s.author, STREAM.snapshot, h, "late"); expect(s.error(s.author)).toBe("preview_stale_grant");
  });
  it("forwards and coalesces Resume without persisting a snapshot", () => {
    vi.spyOn(Date, "now").mockReturnValue(1000);
    const s = setup(), h = s.start();
    const before = s.author.frames.length;
    s.send(s.viewer, STREAM.resume, h); s.send(s.viewer, STREAM.resume, h);
    expect(s.author.frames.length).toBe(before + 1);
    expect(decodeStream(s.author.frames.at(-1)!)?.kind).toBe(STREAM.resume);
  });
  it("keeps receipts separate from durable ACK and rejects unsent receipts", () => {
    const s = setup(), h = s.start();
    s.send(s.viewer, STREAM.receipt, { ...h, revision: 100 }); expect(s.error(s.viewer)).toBe("bad_preview_receipt");
    for (let i = 0; i < 40; i++) {
      const snapshot = { ...h, revision: i };
      s.send(s.author, STREAM.snapshot, snapshot, "x");
      s.send(s.viewer, STREAM.receipt, snapshot);
    }
    expect(s.viewer.close).not.toHaveBeenCalled();
    expect(s.viewer.frames.some(f => decodeFrame(f)?.type === FRAME.ack)).toBe(false);
  });
  it("bounds slow-consumer bytes even across new epochs", () => {
    const s = setup();
    for (let i = 0; i < 5; i++) { const h = s.start(); s.send(s.author, STREAM.snapshot, h, "x".repeat(60_000)); }
    expect(s.viewer.close).toHaveBeenCalled();
  });
  it("bounds slow-consumer frame count", () => {
    const s = setup(), h = s.start();
    for (let i = 0; i <= PREVIEW_LIMITS.pendingFrames; i++) s.send(s.author, STREAM.snapshot, { ...h, revision: i }, "x");
    expect(s.viewer.close).toHaveBeenCalled();
  });
  it("bounds aggregate segment bytes and falls back without truncation", () => {
    const s = setup(), h = s.start();
    s.send(s.author, STREAM.snapshot, h, "x".repeat(61_440));
    s.send(s.author, STREAM.delta, { ...h, revision: 1, prevRevision: 0 }, "界");
    expect(decodeStream(s.author.frames.at(-1)!)?.header.mode).toBe("ready");
  });
  it("allows final snapshot recovery but never appends after Finished", () => {
    const s = setup(), h = s.start();
    s.send(s.author, STREAM.snapshot, h, "final");
    s.send(s.author, STREAM.finished, { ...h, batchId: "b" });
    s.send(s.author, STREAM.snapshot, h, "final"); expect(decodeStream(s.viewer.frames.at(-1)!)?.text).toBe("final");
    s.send(s.author, STREAM.delta, { ...h, revision: 1, prevRevision: 0 }, "extra"); expect(s.error(s.author)).toBe("preview_stale_revision");
  });
  it("bounds incoming frames and room connections", () => {
    const s = setup(); s.start();
    for (let i = 0; i <= PREVIEW_LIMITS.frames; i++) s.relay.message(s.viewer.socket, new Uint8Array([0x20]));
    expect(s.viewer.close).toHaveBeenCalled();
    const t = setup(); for (let i = 2; i < PREVIEW_LIMITS.peers; i++) t.join();
    expect((t.relay.admit(request()) as Response).status).toBe(429);
  });
});
