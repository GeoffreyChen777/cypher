/** Test-only byte helpers shared by the unit and workerd tiers. Never
 * imported by the Worker. */

/** Deterministic `len`-byte pattern; distinct seeds give distinct payloads. */
export const bytesOf = (len: number, seed: number): Uint8Array => {
  const out = new Uint8Array(len);
  for (let i = 0; i < len; i++) out[i] = (seed + i * 31) & 0xff;
  return out;
};

export const sameBytes = (a: Uint8Array | undefined, b: Uint8Array): boolean => {
  if (a === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
};

/** Lowercase hex, the byte encoding of the shared vectors in protocol/vectors. */
export const fromHex = (hex: string): Uint8Array =>
  new Uint8Array((hex.match(/../g) ?? []).map((byte) => parseInt(byte, 16)));

export const toHex = (bytes: Uint8Array): string =>
  [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
