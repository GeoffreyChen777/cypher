/**
 * Device-room frame codec: uleb128 header-length ‖ UTF-8 JSON header ‖ payload.
 * Byte-identical to crates/rpc/src/device_room/frames.rs and the Swift client;
 * all three run protocol/vectors/device-frames-v1.json (protocol/README.md).
 * Pure and import-free so scripts/smoke.mjs can load this file directly (Node
 * type stripping).
 */
export interface DeviceFrameHeader {
  /** Stream id, unique per (connId, logical stream). */
  s: string;
  /** Stream kind: "rpc" | "term" | ... — opaque to the relay. */
  k: string;
  /** Routing: host→client target. */
  to?: string;
  /** Routing: client→host origin (stamped by the relay). */
  from?: string;
}

export const encodeDeviceFrame = (header: DeviceFrameHeader, payload: Uint8Array): Uint8Array => {
  const headerBytes = new TextEncoder().encode(JSON.stringify(header));
  const length: number[] = [];
  let n = headerBytes.length;
  do {
    length.push((n & 0x7f) | (n >= 0x80 ? 0x80 : 0));
    n >>>= 7;
  } while (n > 0);
  const out = new Uint8Array(length.length + headerBytes.length + payload.length);
  out.set(length, 0);
  out.set(headerBytes, length.length);
  out.set(payload, length.length + headerBytes.length);
  return out;
};

/** Throws on a truncated or oversized length prefix, a short header, or
 * non-JSON header bytes. */
export const decodeDeviceFrame = (
  bytes: Uint8Array
): { header: DeviceFrameHeader; payload: Uint8Array } => {
  let offset = 0;
  let length = 0;
  for (let shift = 0; ; shift += 7) {
    // Five bytes cover a u32 length; a sixth is an overflow (the Rust codec
    // rejects it the same way).
    if (shift > 28 || offset >= bytes.length) throw new Error("bad frame length");
    const byte = bytes[offset++]!;
    length |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) break;
  }
  length >>>= 0;
  if (offset + length > bytes.length) throw new Error("frame header out of bounds");
  const header = JSON.parse(new TextDecoder().decode(bytes.subarray(offset, offset + length))) as DeviceFrameHeader;
  return { header, payload: bytes.subarray(offset + length) };
};
