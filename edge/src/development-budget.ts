/** Conservative initial limits for tiny fixtures in the production account. */
export const LIMITS = { events: 1000, perMinute: 120, rows: 10000, rooms: 8, concurrent: 4 } as const;
export interface Budget { day: number; minute: number; events: number; minuteEvents: number; rows: number; rooms: string[]; leases: Record<string, number> }
export function admit(old: Budget | undefined, room: string, now: number, lease = crypto.randomUUID()):
  { allowed: true; budget: Budget; lease: string } | { allowed: false; reason: string } {
  const day = Math.floor(now / 86400000), minute = Math.floor(now / 60000);
  const budget: Budget = old?.day === day ? { ...old, rooms: [...old.rooms] }
    : { day, minute, events: 0, minuteEvents: 0, rows: 0, rooms: [...(old?.rooms ?? [])], leases: {} };
  budget.leases = Object.fromEntries(Object.entries(old?.leases ?? {}).filter(([, expiry]) => expiry > now));
  if (budget.minute !== minute) { budget.minute = minute; budget.minuteEvents = 0; }
  if (budget.rows >= LIMITS.rows) return { allowed: false, reason: "development SQL write budget exhausted" };
  if (budget.events >= LIMITS.events) return { allowed: false, reason: "development daily event budget exhausted" };
  if (budget.minuteEvents >= LIMITS.perMinute) return { allowed: false, reason: "development rate limit" };
  const concurrent = Object.keys(budget.leases).length;
  if (concurrent >= LIMITS.concurrent) return { allowed: false, reason: "development concurrency limit" };
  // Reserve headroom so a burst cannot admit all 120 operations against a
  // stale pre-settlement SQL total. Large operations can still overshoot.
  if (budget.rows + (concurrent + 1) * 256 > LIMITS.rows) return { allowed: false, reason: "development SQL headroom exhausted" };
  if (!budget.rooms.includes(room)) {
    if (budget.rooms.length >= LIMITS.rooms) return { allowed: false, reason: "development room limit" };
    budget.rooms.push(room);
  }
  budget.events++; budget.minuteEvents++; budget.rows++; // the gate's own admission write
  budget.leases[lease] = now + 120000; // crash recovery; not an exact row reservation
  return { allowed: true, budget, lease };
}
