/**
 * The provenance ledger: what this chat's own commands made, as typed
 * entities (a worktree, a branch, a simulator's UDID, a container, a pull
 * request, a scratch file), so that undoing it later is local work
 * (created.ts, risk.ts `facts.created`).
 *
 * Entries come only from commands the chat actually ran, and only when the
 * creator succeeded (catalogue.ts decides that from the exit status, the
 * creator's own output, and what exists before and after). They are kept
 * per chat in one append-only file, `<agentDir>/cypher-permissions/ledger/
 * <chat>.jsonl`, written by the extension's own process: the directory is
 * part of the permission system (config.ts `permissionSystemPatterns`,
 * `DEFAULT_PROTECTED`), so commands in the sandbox can't write it and no
 * reviewing model can approve a write there. A Pi restart, or a chat
 * resumed later, reads the same file.
 *
 * Nothing sensitive, protected, the permission system's or the installed
 * app's is ever recorded, and nothing that was there before the command
 * (catalogue.ts asks `recordable` and checks existence before it runs).
 */

import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { ENTITY_KINDS } from "./entities.ts";
import type { EntityKind } from "./entities.ts";

/** The kinds of things a ledger entry can be: the entities' (entities.ts). */
export type LedgerKind = EntityKind;

export const LEDGER_KINDS: readonly LedgerKind[] = ENTITY_KINDS;

/** One thing a command made, or one a command acts on. Field names as entities.ts has them. */
export interface LedgerEntity {
  kind: LedgerKind;
  /** Its name: a canonical path, a branch, a UDID, a container's id or name, `owner/name` of a repository, a pull request's number. */
  name: string;
  /** The repository a branch or tag is in (its shared git directory), or a pull request's or release's `owner/name`. */
  repo?: string;
  /** The remote a branch or tag is on; none for a local one. */
  remote?: string;
  /** A pull request's head branch. */
  head?: string;
  /** A package's version. */
  version?: string;
}

/** What a command did to the entity: made it (the default), started it, merged it. */
export type LedgerAction = "create" | "start" | "merge";

export interface LedgerEntry extends LedgerEntity {
  /** What was done; absent: created. */
  action?: LedgerAction;
  /** The command that did it (shortened). */
  by?: string;
  at?: string;
  chatId?: string;
}

/**
 * How a name of each kind is matched: `path` covers what is inside too;
 * `hex-prefix` lets a short id name a long one (`docker rm 3f2a9c1b2d4e`);
 * `caseless` compares without case (UDIDs).
 */
type Match = "exact" | "path" | "hex-prefix" | "caseless";

export const MATCHING: Readonly<Record<LedgerKind, Match>> = {
  repo: "caseless",
  remote: "exact",
  branch: "exact",
  pr: "exact",
  tag: "exact",
  release: "exact",
  path: "path",
  worktree: "path",
  process: "exact",
  simulator: "caseless",
  host: "caseless",
  package: "exact",
  container: "hex-prefix",
  image: "exact",
  resource: "exact",
};

/** Bounds on one chat's ledger (the file and the memory). */
const MAX_ENTRIES = 2000;
const MAX_LINE = 4000;

/** Where a chat's ledger is kept. */
export function ledgerFile(agentDir: string, chatId: string | undefined): string | undefined {
  if (!chatId) return undefined;
  return join(agentDir, "cypher-permissions", "ledger", `${chatId.replace(/[^A-Za-z0-9._-]/g, "_")}.jsonl`);
}

/** Whether `name` (what a command acts on) is `made` (what the ledger holds), or inside it. */
function nameCovers(match: Match, made: string, name: string): boolean {
  switch (match) {
    case "exact":
      return made === name;
    case "caseless":
      return made.toLowerCase() === name.toLowerCase();
    case "path":
      return name === made || name.startsWith(made.endsWith("/") ? made : `${made}/`);
    case "hex-prefix":
      // A container's name, or a prefix of its id of at least 12 hex digits.
      return made === name || (/^[0-9a-f]{12,}$/.test(name) && /^[0-9a-f]{64}$/.test(made) && made.startsWith(name));
  }
}

/** Whether an entity the ledger holds is the one a command acts on (or holds it). */
export function entryCovers(made: LedgerEntity, target: LedgerEntity): boolean {
  if (made.kind !== target.kind) return false;
  if (!nameCovers(MATCHING[made.kind], made.name, target.name)) return false;
  // A branch made in one repository is not one of the same name in another; a local one is not a remote one.
  for (const field of ["repo", "remote"] as const) {
    if ((made[field] ?? "") !== (target[field] ?? "")) {
      // An unknown repository on the target's side (`git branch -D x` where the repository can't be told): only a local branch's name.
      if (field === "repo" && target.repo === undefined && made.remote === undefined && target.remote === undefined) continue;
      return false;
    }
  }
  if (target.version !== undefined && made.version !== undefined && target.version !== made.version) return false;
  return true;
}

function validEntry(value: unknown): value is LedgerEntry {
  if (!value || typeof value !== "object") return false;
  const entry = value as Partial<LedgerEntry>;
  if (typeof entry.kind !== "string" || !LEDGER_KINDS.includes(entry.kind as LedgerKind)) return false;
  if (typeof entry.name !== "string" || entry.name === "") return false;
  if (entry.action !== undefined && !["create", "start", "merge"].includes(entry.action)) return false;
  for (const field of ["repo", "remote", "head", "version", "by", "at", "chatId"] as const) {
    if (entry[field] !== undefined && typeof entry[field] !== "string") return false;
  }
  return true;
}

const key = (entity: LedgerEntity & { action?: LedgerAction }) => JSON.stringify([entity.action ?? "create", entity.kind, entity.name, entity.repo ?? "", entity.remote ?? "", entity.version ?? ""]);

/** One chat's ledger: in memory, and in its file when it has one. */
export class Ledger {
  private readonly entries: LedgerEntry[] = [];
  private readonly keys = new Set<string>();
  readonly file: string | undefined;
  private readonly chatId: string | undefined;
  private loaded = false;

  constructor(file: string | undefined, chatId: string | undefined) {
    this.file = file;
    this.chatId = chatId;
  }

  /** Read the file (once; a missing or damaged line is skipped). */
  load(): void {
    if (this.loaded) return;
    this.loaded = true;
    if (!this.file || !existsSync(this.file)) return;
    let text = "";
    try {
      text = readFileSync(this.file, "utf8");
    } catch {
      return;
    }
    for (const line of text.split("\n")) {
      if (!line.trim()) continue;
      try {
        const entry = JSON.parse(line);
        if (validEntry(entry) && (!this.chatId || !entry.chatId || entry.chatId === this.chatId)) this.take(entry);
      } catch {
        // A torn last line: skipped.
      }
    }
  }

  private take(entry: LedgerEntry): boolean {
    const id = key(entry);
    if (this.keys.has(id) || this.entries.length >= MAX_ENTRIES) return false;
    this.keys.add(id);
    this.entries.push(entry);
    return true;
  }

  /** Record what a command made; returns the entries that were new. `persist: false` keeps them in memory (restored from elsewhere). */
  record(entities: readonly (LedgerEntity & { action?: LedgerAction })[], by?: string, persist = true): LedgerEntry[] {
    this.load();
    const fresh: LedgerEntry[] = [];
    const at = new Date().toISOString();
    for (const entity of entities) {
      const { action, ...rest } = entity;
      const entry: LedgerEntry = {
        ...rest,
        ...(action && action !== "create" ? { action } : {}),
        ...(by ? { by: by.length > 200 ? `${by.slice(0, 200)}…` : by } : {}),
        at,
        ...(this.chatId ? { chatId: this.chatId } : {}),
      };
      if (this.take(entry)) fresh.push(entry);
    }
    if (persist && fresh.length && this.file) {
      try {
        mkdirSync(dirname(this.file), { recursive: true, mode: 0o700 });
        const lines = fresh.map((entry) => JSON.stringify(entry)).filter((line) => line.length <= MAX_LINE);
        appendFileSync(this.file, lines.map((line) => `${line}\n`).join(""), { mode: 0o600 });
        chmodSync(this.file, 0o600);
      } catch {
        // Not kept on disk: it still counts in this process.
      }
    }
    return fresh;
  }

  /** The entry that says this chat made `target`, or something holding it. */
  covering(target: LedgerEntity): LedgerEntry | undefined {
    this.load();
    return this.entries.find((entry) => (entry.action ?? "create") === "create" && entryCovers(entry, target));
  }

  covers(target: LedgerEntity): boolean {
    return this.covering(target) !== undefined;
  }

  /** Everything recorded, oldest first. */
  all(): readonly LedgerEntry[] {
    this.load();
    return this.entries;
  }
}
