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
