import { env } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { decodeDeviceFrame, encodeDeviceFrame, type DeviceFrameHeader } from "../../src/device/device-frame";
import { AUTH_USER_HEADER } from "../../src/env";

// DeviceRoom's relay through real hibernatable WebSockets: what a client is
// told when the host's socket goes away or is replaced.

const uid = () => crypto.randomUUID().slice(0, 8);
const room = (device: string) => env.DEVICE_ROOMS.get(env.DEVICE_ROOMS.idFromName(`d2/${device}`));

interface Frame {
  header: DeviceFrameHeader;
  text: string;
}

const join = async (stub: DurableObjectStub, role: "host" | "client", connId?: string) => {
  const query = connId ? `role=${role}&connId=${connId}` : `role=${role}`;
  const response = await stub.fetch(`https://room/ws?${query}`, {
    headers: { Upgrade: "websocket", [AUTH_USER_HEADER]: "owner" }
  });
  expect(response.status).toBe(101);
  const ws = response.webSocket!;
  const frames: Frame[] = [];
  let closed: number | undefined;
  // Binary frames arrive as an ArrayBuffer or a Blob, by compatibility date;
  // the chain keeps Blob reads in arrival order.
  let reading = Promise.resolve();
  ws.addEventListener("message", (event) => {
    const data: unknown = event.data;
    if (typeof data === "string") return;
    reading = reading.then(async () => {
      const bytes = new Uint8Array(data instanceof Blob ? await data.arrayBuffer() : (data as ArrayBuffer));
      const { header, payload } = decodeDeviceFrame(bytes);
      frames.push({ header, text: new TextDecoder().decode(payload) });
    });
  });
  ws.addEventListener("close", (event) => {
    closed = event.code;
  });
  ws.accept();
  return {
    frames,
    closeCode: () => closed,
    send: (header: DeviceFrameHeader, text: string) =>
      ws.send(encodeDeviceFrame(header, new TextEncoder().encode(text))),
    close: () => ws.close(1000, "bye")
  };
};

/** Host sockets the room still lists, corpses included (`GET /status`). */
const hostSockets = async (stub: DurableObjectStub): Promise<number> => {
  const response = await stub.fetch("https://room/status", { headers: { [AUTH_USER_HEADER]: "owner" } });
  return ((await response.json()) as { hostSockets: number }).hostSockets;
};

const relayErrors = (frames: Frame[]) =>
  frames.filter((f) => f.header.k === " relay").map((f) => (JSON.parse(f.text) as { error: string }).error);

describe("DeviceRoom relay", () => {
  it("routes a client's frames to the host and the host's answers back", async () => {
    const stub = room(`dev-${uid()}`);
    const host = await join(stub, "host");
    const client = await join(stub, "client", "c1");

    client.send({ s: "rpc", k: "rpc" }, "ping-1");
    await vi.waitFor(() => expect(host.frames).toHaveLength(1));
    expect(host.frames[0]).toEqual({ header: { s: "rpc", k: "rpc", from: "c1" }, text: "ping-1" });

    host.send({ s: "rpc", k: "rpc", to: "c1" }, "pong-1");
    await vi.waitFor(() => expect(client.frames).toHaveLength(1));
    expect(client.frames[0]).toEqual({ header: { s: "rpc", k: "rpc" }, text: "pong-1" });
  });

  // The host's uplink died silently and it re-joined: its new relay session
  // knows none of the clients' virtual connections or Noise sessions, and the
  // clients' sockets never dropped. Unless told, a client keeps sending
  // sealed frames the new session can't open and waits out every call.
  it("tells the clients when the host's socket is replaced", async () => {
    const stub = room(`dev-${uid()}`);
    const first = await join(stub, "host");
    const client = await join(stub, "client", "c1");
    client.send({ s: "rpc", k: "rpcx" }, "sealed-for-the-first-session");
    await vi.waitFor(() => expect(first.frames).toHaveLength(1));
    expect(relayErrors(client.frames)).toEqual([]);

    const second = await join(stub, "host");

    await vi.waitFor(() => expect(relayErrors(client.frames)).toEqual(["host_closed"]));
    await vi.waitFor(() => expect(first.closeCode()).toBe(4409));
    // The superseded socket closing announces nothing more: a link dialed to
    // the successor meanwhile must not be knocked off.
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(relayErrors(client.frames)).toEqual(["host_closed"]);

    // The same client socket (as a redial would) now reaches the new host.
    client.send({ s: "noise", k: "noise" }, "hello-again");
    await vi.waitFor(() => expect(second.frames).toHaveLength(1));
    expect(second.frames[0]!.header.from).toBe("c1");
  });

  it("says nothing to a waiting client when a host joins an empty room", async () => {
    const stub = room(`dev-${uid()}`);
    // The owner claims the room, then leaves: the client below dials while
    // no host socket exists.
    const claim = await join(stub, "host");
    claim.close();
    await vi.waitFor(async () => expect(await hostSockets(stub)).toBe(0));

    const client = await join(stub, "client", "c1");
    const host = await join(stub, "host");
    client.send({ s: "rpc", k: "rpc" }, "first-call");
    await vi.waitFor(() => expect(host.frames).toHaveLength(1));
    expect(relayErrors(client.frames)).toEqual([]);
  });

  it("tells the clients when the last host leaves, and bounces what they send after", async () => {
    const stub = room(`dev-${uid()}`);
    const host = await join(stub, "host");
    const client = await join(stub, "client", "c1");

    host.close();
    await vi.waitFor(() => expect(relayErrors(client.frames)).toEqual(["host_closed"]));

    client.send({ s: "rpc", k: "rpc" }, "anyone?");
    await vi.waitFor(() => expect(relayErrors(client.frames)).toEqual(["host_closed", "host_offline"]));
  });
});
