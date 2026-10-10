/** Identifier patterns for URL segments and notification payloads. The
 * registry wire validators (registry-core.ts) keep their own patterns because
 * the Rust and Swift clients mirror them. */

/** Chat, org and device ids, `&device=` attribution values and notification
 * identifiers. */
export const ID_RE = /^[A-Za-z0-9_-]{1,128}$/;

/** Tool part ids are harness-minted (`tool-1`, `call_x`, `m1#c1`-style) —
 * wider than ID_RE but still no slashes, so a part id can't traverse keys. */
export const PART_RE = /^[A-Za-z0-9._:#~-]{1,200}$/;

/** Device sidecar slot names (`/device/:id/sidecar/:name`). */
export const SIDECAR_NAME_RE = /^[a-z0-9-]{1,64}$/;
