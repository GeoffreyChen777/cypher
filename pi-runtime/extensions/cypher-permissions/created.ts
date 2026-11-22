/**
 * What this task made: the facts that let the reviewing model approve a
 * dangerous-looking action that only touches the chat's own leavings (its
 * worktrees and branches, its simulators and containers, its private
 * development instance, its scratch files, its pull requests).
 *
 * The facts are read from the commands the chat actually ran, not from what
 * the model says. Every bash call this extension lets through is read
 * against the creator/destroyer catalogue (catalogue.ts): before it runs,
 * `planCreated` lists what it may create and what is there already; after,
 * `settleCreated` keeps what it surely made (its exit status, its own
 * output, what exists now), and the provenance ledger (ledger.ts) records
 * it per chat, on disk, where commands in the sandbox can't write. A later
 * `git worktree remove`, `git branch -D`, `xcrun simctl delete`, `docker
 * rm` or `rm -rf` is then checked against that record (risk.ts
 * `facts.created`, `CreatedStore.covers`): what the chat made is local
 * work, everything else stays dangerous.
 *
 * Nothing sensitive, protected, Cypher's own, or the installed app's is ever
 * recorded, however it was created: a record must never turn a write the
 * user approved once into a free hand to delete it later.
 */

import { Ledger } from "./ledger.ts";
import type { LedgerEntity, LedgerEntry } from "./ledger.ts";
import { planCreations, settleCreations, targetsOf } from "./catalogue.ts";
import type { CatalogueCommand, CatalogueFacts, Candidate, Destruction } from "./catalogue.ts";

export const CREATED_ENTRY = "cypher-permissions.created";

/** One command of a line, as core.ts reads it. */
export type CreationCommand = CatalogueCommand;

/** What a line creates, as paths and git names (the session's record). */
export interface Creations {
  /** Directories and files it makes (canonical paths). */
  paths: string[];
  /** Worktrees it adds (canonical paths). */
  worktrees: string[];
  /** Branches it creates. */
  branches: string[];
}

/** What recording needs from the machine (the older, single-step reading). */
export interface CreationFacts {
  /** A path as the permission engine canonicalizes it, from `cwd`; undefined when that is not known. */
  canonical(path: string, cwd: string | undefined): string | undefined;
  /**
   * Whether a path may be recorded at all: not sensitive, protected,
   * Cypher's own data or the installed app's, not a whole home folder, and
   * not something that was already there (a command naming an existing
   * directory never makes it the task's).
   */
  recordable(path: string): boolean;
  /** The home folder. */
  home: string;
}

const EMPTY: Creations = { paths: [], worktrees: [], branches: [] };

/**
 * Everything one line names as created, read before it runs, without
 * confirming that it ran: paths that are not there yet, worktrees, local
 * branches. Kept for the session's record; the ledger takes only what
 * `settleCreated` confirms.
 */
export function creationsOf(commands: readonly CreationCommand[], facts: CreationFacts): Creations {
  const plan = planCreations(commands, {
    home: facts.home,
    canonical: facts.canonical,
    recordable: facts.recordable,
    exists: () => false,
    // Branches by name alone, as the session has always kept them.
    repository: () => "",
    refExists: () => false,
  });
  const out: Creations = { paths: [], worktrees: [], branches: [] };
  for (const { entity, confirm, before, needs } of plan) {
    if (before || needs || confirm === "output" || entity.name === "") continue;
    const list = entity.kind === "path" ? out.paths : entity.kind === "worktree" ? out.worktrees : entity.kind === "branch" && entity.remote === undefined ? out.branches : undefined;
    if (list && !list.includes(entity.name)) list.push(entity.name);
  }
  return out;
}

/** What one record holds, as it is kept in the session. */
export interface CreatedRecord {
  paths?: string[];
  worktrees?: string[];
  branches?: string[];
  chatId?: string;
  createdAt?: string;
}

/** A plan made before a line ran, to settle after. */
export interface CreationPlan {
  command: string;
  commands: readonly CreationCommand[];
  candidates: Candidate[];
}

/**
 * What the chat has made: its ledger (on disk, when it has a file), and
 * the records of its earlier turns kept in the session
 * (`cypher-permissions.created` entries, from before the ledger).
 */
export class CreatedStore {
  readonly ledger: Ledger;

  private readonly chatId: string | undefined;

  constructor(chatId: string | undefined, file?: string) {
    this.chatId = chatId;
    this.ledger = new Ledger(file, chatId);
  }

  /** Read the records of this chat from the session's entries. */
  restore(entries: readonly { type?: string; customType?: string; data?: unknown }[]): void {
    for (const entry of entries) {
      if (entry.type !== "custom" || entry.customType !== CREATED_ENTRY) continue;
      const record = entry.data as CreatedRecord | undefined;
      if (!record || typeof record !== "object") continue;
      if (this.chatId && record.chatId !== this.chatId) continue;
      this.ledger.record(recordEntities(record), undefined, false);
    }
  }

  /** Record what a line created; returns what was new, to keep in the session. */
  note(creations: Creations, by?: string): CreatedRecord | undefined {
    const fresh = this.ledger.record(recordEntities(creations), by);
    if (fresh.length === 0) return undefined;
    const record: CreatedRecord = {};
    for (const entry of fresh) {
      const list = entry.kind === "path" ? (record.paths ??= []) : entry.kind === "worktree" ? (record.worktrees ??= []) : (record.branches ??= []);
      list.push(entry.name);
    }
    return { ...record, ...(this.chatId ? { chatId: this.chatId } : {}), createdAt: new Date().toISOString() };
  }

  /** Before a line runs: what it may create, and what is there already. */
  plan(command: string, commands: readonly CreationCommand[], facts: CatalogueFacts): CreationPlan {
    return { command, commands, candidates: planCreations(commands, facts) };
  }

  /** After it ran: record what it surely made. */
  settle(plan: CreationPlan, result: { exitCode?: number; output?: string }, facts: CatalogueFacts): LedgerEntry[] {
    const made = settleCreations(plan.candidates, plan.commands, result, { ...facts, ledgered: (entity) => this.covers(entity) });
    return this.ledger.record(made, plan.command);
  }

  /** Whether this chat made the entity (or what holds it). */
  covers(entity: LedgerEntity): boolean {
    return this.ledger.covers(entity);
  }

  /** What a command destroys, and whether this chat made every target of it. */
  destroys(command: CreationCommand, facts: Pick<CatalogueFacts, "canonical" | "repository">): (Destruction & { own: boolean }) | undefined {
    const destruction = targetsOf(command, facts);
    if (!destruction) return undefined;
    const own = !destruction.unknown && destruction.targets.length > 0 && destruction.targets.every((alternatives) => alternatives.some((entity) => this.covers(entity)));
    return { ...destruction, own };
  }

  /** A canonical path this chat made, or one inside a directory it made. */
  has(path: string): boolean {
    return this.covers({ kind: "path", name: path }) || this.covers({ kind: "worktree", name: path });
  }

  worktree(path: string): boolean {
    return this.covers({ kind: "worktree", name: path });
  }

  /** A local branch this chat made, by name (in any repository). */
  branch(name: string): boolean {
    return this.ledger.all().some((entry) => entry.kind === "branch" && entry.remote === undefined && entry.name === name);
  }

  /** For the status text and tests. */
  all(): { paths: string[]; worktrees: string[]; branches: string[]; entities: LedgerEntry[] } {
    const entries = this.ledger.all();
    const names = (kind: string) => entries.filter((entry) => entry.kind === kind && entry.remote === undefined).map((entry) => entry.name);
    return { paths: names("path"), worktrees: names("worktree"), branches: names("branch"), entities: [...entries] };
  }
}

/** A session record's (or a reading's) things, as ledger entities. */
function recordEntities(record: Partial<Creations>): LedgerEntity[] {
  const strings = (list: unknown) => (Array.isArray(list) ? list.filter((item): item is string => typeof item === "string" && item !== "") : []);
  return [
    ...strings(record.paths).map((name): LedgerEntity => ({ kind: "path", name })),
    ...strings(record.worktrees).map((name): LedgerEntity => ({ kind: "worktree", name })),
    ...strings(record.branches).map((name): LedgerEntity => ({ kind: "branch", name })),
  ];
}

export { EMPTY as NO_CREATIONS };
