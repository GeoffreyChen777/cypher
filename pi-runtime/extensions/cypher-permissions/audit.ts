/**
 * Audit log: every decision that was not a plain default allow, one JSON line
 * each, in the Pi agent directory. Rotated at 10 MB (one previous file kept).
 */

import { appendFileSync, mkdirSync, readFileSync, renameSync, statSync } from "node:fs";
import { dirname, join } from "node:path";

export const AUDIT_FILE = "permission-audit.jsonl";
const MAX_BYTES = 10 * 1024 * 1024;

export interface AuditRecord {
  time?: string;
  chatId?: string;
  tool?: string;
  command?: string;
  capability?: string;
  zone?: string;
  guard?: string;
  paths?: string[];
  decision: "allow" | "deny" | "abort";
  decidedBy: "policy" | "grant" | "judge" | "user" | "rule" | "sandbox" | "inspect";
  reason?: string;
  /** Model first: why the reviewing model asked the user instead of approving. */
  reviewer?: string;
  grantId?: string;
  scope?: string;
  /** The risk tier (risk.ts): inspect, local or danger. */
  tier?: string;
  /** What made it dangerous, or why it is local. */
  kind?: string;
  /** From the request to the decision. */
  latencyMs?: number;
  /** A human saw a dialog. */
  prompted?: boolean;
  /** Why no rule could be offered. */
  onceOnly?: string;
  /** The patterns the dialog offered. */
  offered?: string[];
  /** The button pressed, `dismissed` or `timeout`. */
  choice?: string;
  /** The reviewing model: approved, asked, or unavailable. */
  judge?: "approved" | "asked" | "unavailable" | "paused";
}

export class AuditLog {
  private readonly path: string;
  private readonly chatId: string | undefined;

  constructor(agentDir: string, chatId: string | undefined) {
    this.path = join(agentDir, AUDIT_FILE);
    this.chatId = chatId;
  }

  write(record: AuditRecord): void {
    const line = `${JSON.stringify({ time: new Date().toISOString(), chatId: this.chatId, ...record })}\n`;
    try {
      mkdirSync(dirname(this.path), { recursive: true });
      try {
        if (statSync(this.path).size > MAX_BYTES) renameSync(this.path, `${this.path}.1`);
      } catch {
        // No log yet.
      }
      appendFileSync(this.path, line, { mode: 0o600 });
    } catch {
      // Auditing never blocks a decision.
    }
  }

  /**
   * The last 7 days across chats: how many decisions reached a human, by
   * tier and by why, and what was decided without asking. The number the
   * design targets is the questions (permission-friction.md §1, §11).
   */
  stats(now = Date.now()): string {
    let records: AuditRecord[] = [];
    try {
      records = readFileSync(this.path, "utf8")
        .split("\n")
        .filter(Boolean)
        .map((line) => {
          try {
            return JSON.parse(line) as AuditRecord;
          } catch {
            return undefined;
          }
        })
        .filter((record): record is AuditRecord => record !== undefined && Date.parse(record.time ?? "") > now - 7 * 86_400_000);
    } catch {
      return "No permission decisions recorded yet.";
    }
    const asked = records.filter((record) => record.prompted === true || (record.prompted === undefined && record.decidedBy === "user"));
    const count = (list: AuditRecord[], key: (record: AuditRecord) => string | undefined) => {
      const counts = new Map<string, number>();
      for (const record of list) {
        const value = key(record);
        if (value) counts.set(value, (counts.get(value) ?? 0) + 1);
      }
      return [...counts].sort((a, b) => b[1] - a[1]);
    };
    const lines = [`Last 7 days: ${records.length} decisions beyond the defaults, ${asked.length} asked a human.`];
    const automatic = count(records.filter((record) => !asked.includes(record) && record.decision === "allow"), (record) => record.decidedBy);
    if (automatic.length) lines.push(`Allowed without asking: ${automatic.map(([by, n]) => `${n} by ${by}`).join(", ")}.`);
    const tiers = count(asked, (record) => record.tier ?? "untiered");
    if (tiers.length) lines.push(`Questions by tier: ${tiers.map(([tier, n]) => `${tier} ${n}`).join(", ")}.`);
    const kinds = count(asked, (record) => record.kind).slice(0, 8);
    if (kinds.length) lines.push("Why they asked:", ...kinds.map(([kind, n]) => `  ${String(n).padStart(4)}  ${kind}`));
    const once = count(asked, (record) => record.onceOnly?.replace(/`[^`]+`/g, "`…`")).slice(0, 5);
    if (once.length) lines.push("Why no rule could be kept:", ...once.map(([why, n]) => `  ${String(n).padStart(4)}  ${why}`));
    const judge = count(records, (record) => record.judge);
    if (judge.length) lines.push(`Reviewing model: ${judge.map(([state, n]) => `${state} ${n}`).join(", ")}.`);
    const waits = asked.map((record) => record.latencyMs).filter((ms): ms is number => typeof ms === "number").sort((a, b) => a - b);
    if (waits.length) lines.push(`Time a question waited: median ${Math.round(waits[Math.floor(waits.length / 2)] / 1000)} s.`);
    return lines.join("\n");
  }

  recent(limit: number): AuditRecord[] {
    try {
      const lines = readFileSync(this.path, "utf8").trim().split("\n");
      return lines
        .slice(-limit * 4)
        .map((line) => {
          try {
            return JSON.parse(line) as AuditRecord & { chatId?: string };
          } catch {
            return undefined;
          }
        })
        .filter((record): record is AuditRecord & { chatId?: string } => record !== undefined && (!this.chatId || record.chatId === this.chatId))
        .slice(-limit);
    } catch {
      return [];
    }
  }
}
