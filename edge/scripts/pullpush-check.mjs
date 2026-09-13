// End-to-end check for the plain-HTTPS pull/push transport against a running
// `wrangler dev --var AUTH_MODE:dev`.
import { randomUUID } from "node:crypto";

const base = process.env.EDGE_URL ?? "http://127.0.0.1:8787";
const org = `pp-org-${randomUUID().slice(0, 8)}`;
const user = `pp-user-${randomUUID().slice(0, 8)}`;
const token = `${user}@${org}`;
const chatId = `pp-chat-${randomUUID().slice(0, 8)}`;
const device = "pp-device-a";
const auth = { authorization: `Bearer ${token}` };
let failures = 0;

const ok = (message) => console.log(`ok: ${message}`);
const fail = (message) => {
  console.error(`FAIL: ${message}`);
  failures += 1;
};
const json = async (response) => {
  const text = await response.text();
  try {
    return JSON.parse(text);
  } catch {
    return { raw: text };
  }
};

const assertStatus = (response, expected, label) => {
  if (response.status !== expected) fail(`${label}: expected ${expected}, got ${response.status}`);
  else ok(`${label}: HTTP ${response.status}`);
};

// HTTP requests must be authenticated by the Worker.
{
  const unauthenticated = await fetch(`${base}/registry/${org}/rows`);
  assertStatus(unauthenticated, 401, "unauthenticated registry pull");
}

// Registry: HTTP push, replay, full pull, delta pull, and presence beat.
{
  const now = Date.now();
  const hlc = `${String(now).padStart(13, "0")}-000001-${device}`;
  const op = {
    kind: "chats",
    id: "pp-chat-row",
    op: "upsert",
    hlc,
    set: {
      id: "pp-chat-row",
      deviceId: device,
      title: "pull push check",
      createdAt: now
    }
  };
  const push = await fetch(`${base}/registry/${org}/push?device=${device}`, {
    method: "POST",
    headers: { ...auth, "content-type": "application/json" },
    body: JSON.stringify({ batch: "pp-reg-batch-1", ops: [op] })
  });
  const ack = await json(push);
  if (push.status !== 200 || ack.batch !== "pp-reg-batch-1" || !(ack.seq >= 1)) {
    fail(`registry push: ${push.status} ${JSON.stringify(ack)}`);
  } else ok(`registry push acked seq=${ack.seq}`);

  const replay = await fetch(`${base}/registry/${org}/push?device=${device}`, {
    method: "POST",
    headers: { ...auth, "content-type": "application/json" },
    body: JSON.stringify({ batch: "pp-reg-batch-1", ops: [op] })
  });
  const replayAck = await json(replay);
  if (replay.status !== 200 || replayAck.applied !== 0) {
    fail(`registry push replay: ${replay.status} ${JSON.stringify(replayAck)}`);
  } else ok("registry push replay is an LWW no-op");

  const fullResponse = await fetch(
    `${base}/registry/${org}/rows?device=${device}&beat=1`,
    { headers: auth }
  );
  const full = await json(fullResponse);
  if (
    fullResponse.status !== 200 ||
    !full.full ||
    !Array.isArray(full.rows) ||
    full.rows.length < 1
  ) {
    fail(`registry full pull: ${fullResponse.status} ${JSON.stringify(full).slice(0, 300)}`);
  } else ok(`registry full pull: rows=${full.rows.length}, seq=${full.seq}`);
  if (!full.presence?.[device]) fail("registry beat was not recorded");
  else ok("registry HTTP pull recorded presence");

  const deltaResponse = await fetch(
    `${base}/registry/${org}/rows?since=${full.seq}&device=${device}`,
    { headers: auth }
  );
  const delta = await json(deltaResponse);
  if (deltaResponse.status !== 200 || delta.full !== false || delta.rows.length !== 0) {
    fail(`registry delta pull: ${deltaResponse.status} ${JSON.stringify(delta).slice(0, 300)}`);
  } else ok("registry pull at head returned an empty delta");
}

// Chat2: first-contact claim, HTTP push, batch dedupe, framed pull, and
// exclude-own filtering.
{
  const seed = await fetch(`${base}/chat2/${chatId}/checkpoint?seqCovered=0`, {
    method: "POST",
    headers: { ...auth, "x-chat2-frontier": "" },
    body: new Uint8Array([1, 2, 3])
  });
  assertStatus(seed, 200, "chat2 first-contact checkpoint claim");

  const payload = new Uint8Array([9, 9, 9, 9]);
  const push = await fetch(
    `${base}/chat2/${chatId}/rows?batchId=pp-chat-batch-1&device=${device}`,
    { method: "POST", headers: auth, body: payload }
  );
  const ack = await json(push);
  if (push.status !== 200 || ack.seq !== 1 || ack.dup !== false) {
    fail(`chat2 push: ${push.status} ${JSON.stringify(ack)}`);
  } else ok(`chat2 push acked seq=${ack.seq}`);

  const replay = await fetch(
    `${base}/chat2/${chatId}/rows?batchId=pp-chat-batch-1&device=${device}`,
    { method: "POST", headers: auth, body: payload }
  );
  const replayAck = await json(replay);
  if (replay.status !== 200 || replayAck.seq !== 1 || replayAck.dup !== true) {
    fail(`chat2 push replay: ${replay.status} ${JSON.stringify(replayAck)}`);
  } else ok("chat2 push replay deduped");

  const pull = await fetch(
    `${base}/chat2/${chatId}/rows?after=0&device=pp-device-b`,
    { headers: auth }
  );
  if (pull.status !== 200) {
    fail(`chat2 pull: HTTP ${pull.status}`);
  } else {
    const bytes = new Uint8Array(await pull.arrayBuffer());
    const frames = [];
    let offset = 0;
    while (offset < bytes.length) {
      if (offset + 4 > bytes.length) {
        fail("chat2 pull has a truncated frame length");
        break;
      }
      const length = new DataView(bytes.buffer, bytes.byteOffset + offset, 4).getUint32(0, true);
      offset += 4;
      if (length === 0 || offset + length > bytes.length) {
        fail("chat2 pull has a truncated frame");
        break;
      }
      const frame = bytes.subarray(offset, offset + length);
      const type = frame[0];
      const headerLength = new DataView(frame.buffer, frame.byteOffset + 1, 4).getUint32(0, true);
      const header = JSON.parse(
        new TextDecoder().decode(frame.subarray(5, 5 + headerLength))
      );
      frames.push({ type, header, payload: frame.subarray(5 + headerLength) });
      offset += length;
    }
    const kinds = frames.map((frame) => frame.type);
    if (kinds[0] !== 0x02 || kinds.at(-1) !== 0x05) {
      fail(`chat2 pull framing: ${JSON.stringify(kinds)}`);
    } else ok(`chat2 pull framing state → rows → rowsDone (${frames.length} frames)`);
    const state = frames[0]?.header;
    if (state?.headSeq !== 1 || state?.checkpointSize !== 3) {
      fail(`chat2 pull state: ${JSON.stringify(state)}`);
    } else ok("chat2 pull state metadata is correct");
    const row = frames.find((frame) => frame.type === 0x04);
    if (
      !row ||
      row.header.seq !== 1 ||
      row.header.batchId !== "pp-chat-batch-1" ||
      row.payload.length !== 4 ||
      row.payload[0] !== 9
    ) {
      fail(`chat2 pull row: ${JSON.stringify(row?.header)}`);
    } else ok("chat2 pull preserved row metadata and bytes");
  }

  const own = await fetch(
    `${base}/chat2/${chatId}/rows?after=0&device=${device}&excludeOwn=1`,
    { headers: auth }
  );
  if (own.status !== 200) {
    fail(`chat2 exclude-own pull: HTTP ${own.status}`);
  } else {
    const bytes = new Uint8Array(await own.arrayBuffer());
    // The row frame type byte is at offset 4 (length prefix) for this
    // one-row-or-no-row response; decode enough to ensure no row is present.
    let hasRow = false;
    let offset = 0;
    while (offset + 4 <= bytes.length) {
      const length = new DataView(bytes.buffer, bytes.byteOffset + offset, 4).getUint32(0, true);
      offset += 4;
      if (offset + length > bytes.length || length < 5) break;
      hasRow ||= bytes[offset] === 0x04;
      offset += length;
    }
    if (hasRow) fail("chat2 excludeOwn returned the sender's row");
    else ok("chat2 excludeOwn filtered the sender's row");
  }
}

if (failures > 0) {
  console.error(`${failures} pull/push check(s) failed`);
  process.exit(1);
}
console.log("ALL PULL/PUSH CHECKS PASSED");
