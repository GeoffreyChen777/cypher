/**
 * Device trust chain rules: the edge's mirror of crates/trust/src/chain.rs.
 * The Rust, TypeScript and Swift implementations all run
 * protocol/vectors/device-trust-v1.json (protocol/README.md).
 *
 * The edge validates every append so the log it serves is always well
 * formed and a stolen bearer cannot wedge it with junk. Devices still verify
 * the whole log themselves: the edge is not trusted to vouch for a device.
 */

export interface DevicePublic {
  id: string;
  name: string;
  signPub: string;
  dhPub: string;
}

export type EntryKind = "add" | "remove" | "requirePaired" | "allowUnpaired";

const KIND_BYTE: Record<EntryKind, number> = { add: 1, remove: 2, requirePaired: 3, allowUnpaired: 4 };

export interface TrustEntry {
  seq: number;
  prev: string;
  kind: EntryKind;
  device: DevicePublic;
  at: number;
  signer: string;
  sig: string;
}

export interface ChainState {
  length: number;
  head: string;
  members: Map<string, DevicePublic>;
  removed: Set<string>;
  keysSeen: Set<string>;
  /** The group's "Only paired devices" policy. */
  requirePaired: boolean;
}

export const MAX_ENTRIES = 1024;
const MAX_FIELD = 128;
const ZERO_HASH = "0".repeat(64);
const HEX32 = /^[0-9a-f]{64}$/;
const HEX64 = /^[0-9a-f]{128}$/;
// Rust's `char::is_control`: the C0 and C1 control ranges.
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

const encoder = new TextEncoder();

export const emptyState = (): ChainState => ({
  length: 0,
  head: ZERO_HASH,
  members: new Map(),
  removed: new Set(),
  keysSeen: new Set(),
  requirePaired: false
});

const fromHex = (hex: string): Uint8Array => {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
};

export const toHex = (bytes: Uint8Array): string =>
  [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");

const validField = (value: unknown): value is string =>
  typeof value === "string" &&
  value.length > 0 &&
  encoder.encode(value).length <= MAX_FIELD &&
  !CONTROL.test(value);

const isU64 = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

/** The entry in its wire shape, or `undefined`. */
export const parseEntry = (value: unknown): TrustEntry | undefined => {
  if (typeof value !== "object" || value === null) return undefined;
  const e = value as Record<string, unknown>;
  const d = e.device as Record<string, unknown> | undefined;
  if (
    !isU64(e.seq) ||
    typeof e.prev !== "string" || !HEX32.test(e.prev) ||
    typeof e.kind !== "string" || !Object.hasOwn(KIND_BYTE, e.kind) ||
    typeof d !== "object" || d === null ||
    !validField(d.id) || !validField(d.name) ||
    typeof d.signPub !== "string" || !HEX32.test(d.signPub) ||
    typeof d.dhPub !== "string" || !HEX32.test(d.dhPub) ||
    !isU64(e.at) ||
    !validField(e.signer) ||
    typeof e.sig !== "string" || !HEX64.test(e.sig)
  ) {
    return undefined;
  }
  return {
    seq: e.seq,
    prev: e.prev,
    kind: e.kind as EntryKind,
    device: { id: d.id, name: d.name, signPub: d.signPub, dhPub: d.dhPub },
    at: e.at,
    signer: e.signer,
    sig: e.sig
  };
};

class Writer {
  private parts: Uint8Array[] = [];
  bytes(value: Uint8Array): this {
    this.parts.push(value);
    return this;
  }
  u64(value: number): this {
    const out = new Uint8Array(8);
    new DataView(out.buffer).setBigUint64(0, BigInt(value));
    return this.bytes(out);
  }
  str(value: string): this {
    const utf8 = encoder.encode(value);
    const len = new Uint8Array(4);
    new DataView(len.buffer).setUint32(0, utf8.length);
    return this.bytes(len).bytes(utf8);
  }
  done(): Uint8Array {
    const total = this.parts.reduce((n, p) => n + p.length, 0);
    const out = new Uint8Array(total);
    let offset = 0;
    for (const part of this.parts) {
      out.set(part, offset);
      offset += part.length;
    }
    return out;
  }
}

export const signingBytes = (entry: TrustEntry): Uint8Array =>
  new Writer()
    .bytes(encoder.encode("cypher-trust-entry-v1\0"))
    .u64(entry.seq)
    .bytes(fromHex(entry.prev))
    .bytes(Uint8Array.of(KIND_BYTE[entry.kind]))
    .str(entry.device.id)
    .str(entry.device.name)
    .bytes(fromHex(entry.device.signPub))
    .bytes(fromHex(entry.device.dhPub))
    .u64(entry.at)
    .str(entry.signer)
    .done();

export const entryHash = async (entry: TrustEntry): Promise<string> => {
  const data = new Writer()
    .bytes(encoder.encode("cypher-trust-hash-v1\0"))
    .bytes(signingBytes(entry))
    .bytes(fromHex(entry.sig))
    .done();
  return toHex(new Uint8Array(await crypto.subtle.digest("SHA-256", data)));
};

const verifySignature = async (signPub: string, entry: TrustEntry): Promise<boolean> => {
  try {
    const key = await crypto.subtle.importKey("raw", fromHex(signPub), { name: "Ed25519" }, false, ["verify"]);
    return await crypto.subtle.verify({ name: "Ed25519" }, key, fromHex(entry.sig), signingBytes(entry));
  } catch {
    return false;
  }
};

const sameDevice = (a: DevicePublic | undefined, b: DevicePublic): boolean =>
  a !== undefined && a.id === b.id && a.name === b.name && a.signPub === b.signPub && a.dhPub === b.dhPub;

/**
 * Apply `entry` on top of `state`, or answer why it is refused. `signatures:
 * false` replays entries that were verified when they were stored.
 */
export const applyEntry = async (
  state: ChainState,
  entry: TrustEntry,
  { signatures = true }: { signatures?: boolean } = {}
): Promise<string | undefined> => {
  if (state.length >= MAX_ENTRIES) return "too many entries";
  if (entry.seq !== state.length) return "out of sequence";
  if (entry.prev !== state.head) return "does not follow the previous entry";
  let signer: DevicePublic | undefined;
  if (entry.seq === 0) {
    if (entry.kind !== "add" || entry.signer !== entry.device.id) {
      return "the first entry must be a device adding itself";
    }
    signer = entry.device;
  } else {
    signer = state.members.get(entry.signer);
    if (!signer) return "signed by a device outside the group";
  }
  if (signatures && !(await verifySignature(signer.signPub, entry))) return "bad signature";
  if (entry.kind === "add") {
    // A removed device's keys stay out (keysSeen); its id may return with new keys.
    if (state.members.has(entry.device.id)) return "device already a member";
    if (state.keysSeen.has(entry.device.signPub) || state.keysSeen.has(entry.device.dhPub)) {
      return "key already used";
    }
    state.keysSeen.add(entry.device.signPub);
    state.keysSeen.add(entry.device.dhPub);
    state.members.set(entry.device.id, entry.device);
  } else if (entry.kind === "remove") {
    if (!sameDevice(state.members.get(entry.device.id), entry.device)) {
      return "removes a device that is not a member";
    }
    state.members.delete(entry.device.id);
    state.removed.add(entry.device.id);
  } else {
    if (!sameDevice(signer, entry.device)) return "a policy entry must carry its signer's record";
    state.requirePaired = entry.kind === "requirePaired";
  }
  state.length += 1;
  state.head = await entryHash(entry);
  return undefined;
};

/** Replay a whole log; the state, or why it is refused. */
export const replay = async (
  entries: readonly unknown[],
  options?: { signatures?: boolean }
): Promise<ChainState | string> => {
  const state = emptyState();
  for (const value of entries) {
    const entry = parseEntry(value);
    if (!entry) return "malformed entry";
    const refused = await applyEntry(state, entry, options);
    if (refused) return refused;
  }
  return state;
};
