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
  decidedBy: "policy" | "grant" | "judge" | "user" | "rule" | "sandbox";
  reason?: string;
  /** Model first: why the reviewing model asked the user instead of approving. */
  reviewer?: string;
  grantId?: string;
  scope?: string;
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
