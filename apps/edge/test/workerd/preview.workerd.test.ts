import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ChatRoom } from "../../src/chat/chat-room";
import type { Env } from "../../src/env";
import { decodeFrame, encodeFrame, type FrameType } from "../../src/chat/chat-frames";

const binary = (bytes: Uint8Array) => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;

/** The dormant stream-preview protocol's `start` frame (0x24), a type the
 * production decoder does not register. */
const PREVIEW_START = 0x24 as FrameType;

describe("stream preview frames through real ChatRoom/workerd", () => {
  it("production ChatRoom rejects preview frames: no relay is wired", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("preview-production-off"));
    await runInDurableObject(stub, async (_, ctx) => {
      const room = new ChatRoom(ctx, { AUTH_MODE: "dev" } as unknown as Env);
      const sent: Uint8Array[] = [];
      const ws = { send: (bytes: ArrayBuffer) => sent.push(new Uint8Array(bytes)), deserializeAttachment: () => ({ userId: "u", device: "d", ready: true }) } as unknown as WebSocket;
      await room.webSocketMessage(ws, binary(encodeFrame(PREVIEW_START, { chatId: "c", runId: "r", segmentId: "s" })));
      expect(decodeFrame(sent[0]!)?.header.code).toBe("bad_frame");
    });
  });
});
