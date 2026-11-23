/**
 * The owner's denials as evidence (round 4, design §7.4.1): an explicit Deny click on a request
 * makes later requests of the same action class in that chat ask, until the owner's own words since
 * ask for that very action on that target.
 *
 * An action class is three things, read generally (no per-command rules):
 * - `tool`: the tool that asked (`bash`, `read`, `write`, an MCP tool's name);
 * - `actions`: what each command does, by the tool table (tools.ts `readTool`: program and
 *   subcommand, `gh pr merge`), else the program (`sqlite3`); a file tool's capability (`read`); and
 *   `concern`: what the classifier says the request does when it names it (risk.ts `concern`:
 *   reading a secret or the installed app's data; a danger's key), so the same reach by another
 *   program (`ls` after a denied `sqlite3`) is the same class;
 * - `scopes`: what it acts on, as entities (entities.ts kinds): a path covers its subtree (either
 *   way: a directory read holds the file the owner denied), a host or branch its own name; a line
 *   that names nothing is its own scope (`line`).
 *
 * Only a Deny click counts (and Deny and stop); a prompt closed, timed out or aborted (`dismissed`)
 * is no denial (§7.4). Kept per chat in `<agentDir>/cypher-permissions/denials/<chat>.jsonl`, part
 * of the permission system (config.ts `permissionSystemPatterns`), so a Pi restart reads the same
 * file and no command in the sandbox can write it.
 */

import { createHash } from "node:crypto";
import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";

import type { EntityKind } from "./entities.ts";
import { consentText, mentions } from "./explicit.ts";
import type { OwnerWords } from "./explicit.ts";

export type ScopeKind = EntityKind | "line";

export interface DenialScope {
  kind: ScopeKind;
  /** A canonical path, a host, a branch, a pull request's number, … ; for `line`, the command as written. */
  name: string;
  /** How the owner would name it, besides `name` (a home-relative path, a file's name). */
  says?: readonly string[];
}

export interface ActionClass {
  tool: string;
  actions: readonly string[];
  concern?: string;
  scopes: readonly DenialScope[];
}

export interface DenialRecord extends ActionClass {
  at: string;
  /** The request in words (its command, or its paths), for the evidence. */
  what: string;
  /** Hashes of the owner's words that existed when the owner denied it: a word not among them is newer. */
  seen: readonly string[];
  chatId?: string;
}

/** Kinds whose names are matched as paths (a subtree), and kinds matched without case. */
const PATH_KINDS: ReadonlySet<ScopeKind> = new Set(["path", "worktree"]);
const CASELESS: ReadonlySet<ScopeKind> = new Set(["host", "repo", "simulator"]);
/** Consent kinds that can lift a denial: the owner's own words, not a reply to a proposal (§7.3.4). */
const LIFTING_SOURCES: ReadonlySet<string> = new Set(["typed", "plan", "card", "answer"]);
const MAX_SEEN = 200;
const MAX_RECORDS = 500;

/** An owner word's identity: its time when known, and its text. */
export function wordHash(word: { text: string; at?: number }): string {
  return createHash("sha256").update(`${word.at ?? ""}\u0000${word.text}`).digest("hex").slice(0, 16);
}

function under(child: string, parent: string): boolean {
  return child === parent || child.startsWith(parent.endsWith("/") ? parent : `${parent}/`);
}

/** Whether a denied scope and a requested one are the same thing, or one holds the other. */
export function scopeMatches(denied: DenialScope, asked: DenialScope): boolean {
  if (PATH_KINDS.has(denied.kind) && PATH_KINDS.has(asked.kind)) return under(asked.name, denied.name) || under(denied.name, asked.name);
  if (denied.kind !== asked.kind) return false;
  if (CASELESS.has(denied.kind)) return denied.name.toLowerCase() === asked.name.toLowerCase();
  if (denied.kind === "line") return denied.name.replace(/\s+/g, " ").trim() === asked.name.replace(/\s+/g, " ").trim();
  return denied.name === asked.name;
}

/** The scope of `asked` a denial covers when the two are the same class, else undefined. */
export function classMatch(denied: ActionClass, asked: ActionClass): DenialScope | undefined {
  if (denied.tool !== asked.tool) return undefined;
  const sameAction = denied.actions.some((action) => asked.actions.includes(action)) || (denied.concern !== undefined && denied.concern === asked.concern);
  if (!sameAction) return undefined;
  return asked.scopes.find((scope) => denied.scopes.some((other) => scopeMatches(other, scope)));
}

/** The names the owner's words may use for a scope. */
function namesOf(scope: DenialScope): string[] {
  const names = [scope.name, ...(scope.says ?? [])];
  if (PATH_KINDS.has(scope.kind)) {
    const base = basename(scope.name);
    if (base.length >= 3) names.push(base);
  }
  return [...new Set(names.filter((name) => name.length >= 2))];
}

/**
 * The owner word newer than the denial (not among those seen when they denied it), of a lifting
 * kind, that names the scope: the owner asked for that very action since. Undefined: still denied.
 */
export function liftedBy(denial: DenialRecord, scope: DenialScope, owner: OwnerWords | undefined): string | undefined {
  if (!owner) return undefined;
  const seen = new Set(denial.seen);
  const names = [...new Set([...namesOf(scope), ...denial.scopes.filter((other) => scopeMatches(other, scope)).flatMap(namesOf)])];
  for (const word of [...owner.messages].reverse()) {
    if (seen.has(wordHash(word))) continue;
    if (!LIFTING_SOURCES.has(word.source ?? "typed")) continue;
    const text = consentText(word);
    if (scope.kind === "line" ? text.includes(scope.name) : names.some((name) => mentions(text, name))) return word.text;
  }
  return undefined;
}

export function denialsFile(agentDir: string, chatId: string | undefined): string | undefined {
  if (!chatId) return undefined;
  return join(agentDir, "cypher-permissions", "denials", `${chatId.replace(/[^A-Za-z0-9._-]/g, "_")}.jsonl`);
}

function valid(value: unknown): value is DenialRecord {
  if (!value || typeof value !== "object") return false;
  const record = value as Partial<DenialRecord>;
  return (
    typeof record.tool === "string" &&
    Array.isArray(record.actions) &&
    record.actions.every((action) => typeof action === "string") &&
    Array.isArray(record.scopes) &&
    record.scopes.every((scope) => scope && typeof scope.kind === "string" && typeof scope.name === "string") &&
    typeof record.at === "string" &&
    typeof record.what === "string" &&
    Array.isArray(record.seen)
  );
}

/** One chat's denials: in memory, and in its file. */
export class Denials {
  private readonly records: DenialRecord[] = [];
  private loaded = false;
  readonly file: string | undefined;
  private readonly chatId: string | undefined;

  constructor(file: string | undefined, chatId: string | undefined) {
    this.file = file;
    this.chatId = chatId;
  }

  private load(): void {
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
        const record = JSON.parse(line);
        if (valid(record) && (!this.chatId || !record.chatId || record.chatId === this.chatId) && this.records.length < MAX_RECORDS) this.records.push(record);
      } catch {
        // A torn line: skipped.
      }
    }
  }

  /** Record the owner's Deny click on a request of `cls`. */
  record(cls: ActionClass, what: string, owner: OwnerWords | undefined): DenialRecord {
    this.load();
    const record: DenialRecord = {
      tool: cls.tool,
      actions: [...cls.actions],
      ...(cls.concern ? { concern: cls.concern } : {}),
      scopes: cls.scopes.map((scope) => ({ kind: scope.kind, name: scope.name, ...(scope.says?.length ? { says: [...scope.says] } : {}) })),
      at: new Date().toISOString(),
      what: what.length > 300 ? `${what.slice(0, 300)}…` : what,
      seen: (owner?.messages ?? []).slice(-MAX_SEEN).map((word) => wordHash(word)),
      ...(this.chatId ? { chatId: this.chatId } : {}),
    };
    if (this.records.length < MAX_RECORDS) this.records.push(record);
    if (this.file) {
      try {
        mkdirSync(dirname(this.file), { recursive: true, mode: 0o700 });
        appendFileSync(this.file, `${JSON.stringify(record)}\n`, { mode: 0o600 });
        chmodSync(this.file, 0o600);
      } catch {
        // Not kept on disk: it still counts in this process.
      }
    }
    return record;
  }

  any(): boolean {
    this.load();
    return this.records.length > 0;
  }

  /**
   * The newest denial of `asked`'s class that the owner's words since haven't lifted, with the scope
   * it covers; and the denials of the same action on other targets (evidence for the judge).
   */
  check(asked: ActionClass, owner: OwnerWords | undefined): { active?: { denial: DenialRecord; scope: DenialScope }; similar: DenialRecord[] } {
    this.load();
    const similar: DenialRecord[] = [];
    for (const denial of [...this.records].reverse()) {
      const scope = classMatch(denial, asked);
      if (scope) {
        if (liftedBy(denial, scope, owner) === undefined) return { active: { denial, scope }, similar };
        continue;
      }
      if (denial.tool === asked.tool && denial.actions.some((action) => asked.actions.includes(action))) similar.push(denial);
    }
    return { similar: similar.slice(0, 3) };
  }

  all(): readonly DenialRecord[] {
    this.load();
    return this.records;
  }
}
