/**
 * The owner's denials as evidence (round 4, design §7.4.1; scope by effect and target since round 5): an
 * explicit Deny click on a request makes later requests that do the same to the same thing ask in that
 * chat, until the owner's own words since ask for that very action on that target.
 *
 * A request is read as units (effects.ts): what it does — its effect class (`EFFECT_CLASSES`: the tool
 * table's action, the catalogue's operation, the program's effect, the file tool's capability) — and to
 * what — typed entities (entities.ts kinds: a canonical path, a branch, a tag, a pull request, a host…).
 * A bash line has one unit per pipeline (what `| xargs rm` deletes is what the command before it named);
 * wrappers (`sudo`, `env`, `xargs`, `bash -c`, `find -exec`) are read through.
 *
 * A later request matches a denial when one of its units has an effect class of a denied unit and a target
 * that is the same entity as a denied target, holds it or is held by it (entities.ts `relation`), whatever
 * tool or program asks: `find data/fixtures -delete` after a denied `rm -rf data/fixtures`, `head` of a
 * file after a denied `cat` of it, `ls` of the folder holding it, the write tool on a file under a
 * folder whose deletion was denied. A branch never matches a path (`release/2.0`), a read never matches a
 * push. A line that names nothing is its own scope (`line`).
 *
 * The denial is lifted only by the owner's own words since (typed, a plan, its card, an answer: not a
 * reply to a proposal) that ask for that effect on that target: they name it, say one of the effect's
 * action words (lexicon.ts, `CLASS_WORDS`), and are no question, negation or "wait" (`liftedBy`). Naming
 * it alone ("PR 44 怎么样了？") lifts nothing.
 *
 * Only a Deny click counts (and Deny and stop); a prompt closed, timed out or aborted (`dismissed`)
 * is no denial (§7.4). Kept per chat in `<agentDir>/cypher-permissions/denials/<chat>.jsonl`, part
 * of the permission system (config.ts `permissionSystemPatterns`), so a Pi restart reads the same
 * file and no command in the sandbox can write it.
 */

import { createHash } from "node:crypto";
import { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";

import { CLASS_WORDS, classOf } from "./effects.ts";
import type { EffectClass } from "./effects.ts";
import { relation } from "./entities.ts";
import type { EntityKind } from "./entities.ts";
import { consentText, mentions, QUESTION } from "./explicit.ts";
import type { OwnerWords } from "./explicit.ts";
import { actionWords, listWords } from "./lexicon.ts";

export type ScopeKind = EntityKind | "line";

export interface DenialScope {
  kind: ScopeKind;
  /** A canonical path, a host, a branch, a pull request's number, … ; for `line`, the command as written. */
  name: string;
  /** How the owner would name it, besides `name` (a home-relative path, a file's name). */
  says?: readonly string[];
  /** A branch's or tag's remote; a pull request's repository. */
  remote?: string;
  repo?: string;
  /** A resource's namespace and type (schema.ts). */
  scope?: string;
  type?: string;
  /** The fixed folder of a glob (where it looks), not a target named in full; not recorded. */
  glob?: boolean;
}

/** What one part of a request does, to what (a bash pipeline; a file tool's access). */
export interface DenialUnit {
  effects: readonly EffectClass[];
  scopes: readonly DenialScope[];
  /** It destroys what it names (a delete, erase, stop, move away): the other-task floor's (round 6, W3); not recorded. */
  destroys?: boolean;
  /** The canonical globs it is given (what they could match it acts on); not recorded. */
  globs?: readonly string[];
}

export interface ActionClass {
  tool: string;
  /** What each command does by the tool table, else its program (for the evidence). */
  actions: readonly string[];
  concern?: string;
  /** Every scope of every unit (for the evidence and records before round 5). */
  scopes: readonly DenialScope[];
  units: readonly DenialUnit[];
  /**
   * The request's argv words (bash: every command's, the program's own name left out), for the safety net of
   * denial matching (round 6, W2: `tokenMatch`); not recorded.
   */
  tokens?: readonly string[];
}

export interface DenialRecord extends Omit<ActionClass, "units"> {
  /** Absent in records from before round 5: read from `actions` and `scopes` (`unitsOf`). */
  units?: readonly DenialUnit[];
  at: string;
  /** The request in words (its command, or its paths), for the evidence. */
  what: string;
  /** Hashes of the owner's words that existed when the owner denied it: a word not among them is newer. */
  seen: readonly string[];
  chatId?: string;
}

/** Kinds whose names are matched as paths (a subtree). */
const PATH_KINDS: ReadonlySet<ScopeKind> = new Set(["path", "worktree"]);
/** Consent kinds that can lift a denial: the owner's own words, not a reply to a proposal (§7.3.4). */
const LIFTING_SOURCES: ReadonlySet<string> = new Set(["typed", "plan", "card", "answer"]);
const MAX_SEEN = 200;
const MAX_RECORDS = 500;

/** An owner word's identity: its time when known, and its text. */
export function wordHash(word: { text: string; at?: number }): string {
  return createHash("sha256").update(`${word.at ?? ""}\u0000${word.text}`).digest("hex").slice(0, 16);
}

/** Whether a denied scope and a requested one are the same thing, or one holds the other (entities.ts `relation`), typed. */
export function scopeMatches(denied: DenialScope, asked: DenialScope): boolean {
  if (denied.kind === "line" || asked.kind === "line") return denied.kind === asked.kind && denied.name.replace(/\s+/g, " ").trim() === asked.name.replace(/\s+/g, " ").trim();
  const entity = (scope: DenialScope) => ({ kind: scope.kind as EntityKind, name: scope.name, ...(scope.remote ? { remote: scope.remote } : {}), ...(scope.repo ? { repo: scope.repo } : {}), ...(scope.scope ? { scope: scope.scope } : {}), ...(scope.type ? { type: scope.type } : {}) });
  const how = relation(entity(denied), entity(asked));
  if (how === "same" || how === "contains" || how === "contained") return true;
  // What can't be told apart (a branch with its remote unsaid on one side) is the same name of the same kind.
  return how === "unknown" && denied.kind === asked.kind && denied.name === asked.name;
}

/** A record's units; one from before round 5 is read from its actions (as effect classes) and scopes. */
export function unitsOf(record: Pick<DenialRecord, "units" | "actions" | "scopes" | "tool">): readonly DenialUnit[] {
  if (record.units?.length) return record.units;
  const effects = [...new Set(record.actions.map((action) => (action === "read" ? "read" : action === "write" ? "change" : action === "network" ? "send" : classOf(action.split(" ").pop() ?? action))))];
  return [{ effects: effects.length ? effects : ["run"], scopes: record.scopes }];
}

/** The denied unit and the scope of `asked` a denial covers: the same effect class on the same thing, or one holding the other. */
export function classMatch(denied: Pick<DenialRecord, "units" | "actions" | "scopes" | "tool">, asked: ActionClass): { unit: DenialUnit; scope: DenialScope } | undefined {
  for (const unit of unitsOf(denied)) {
    for (const other of asked.units) {
      if (!unit.effects.some((effect) => other.effects.includes(effect))) continue;
      const scope = other.scopes.find((candidate) => unit.scopes.some((scope) => scopeMatches(scope, candidate)));
      if (scope) return { unit, scope };
    }
  }
  return undefined;
}

/** Whether an effect of `asked` is one of `denied`'s, or is unknown (`run`: a program no table reads; an MCP or extension tool's own), which may be any. */
function effectsMeet(denied: readonly EffectClass[], asked: readonly EffectClass[]): boolean {
  const unknown = asked.some((effect) => effect === "run" || effect.startsWith("tool:"));
  return denied.some((effect) => asked.includes(effect) || unknown);
}

/**
 * The safety net of denial matching (round 6, W2), for tools without a schema: an argv word of the request
 * equal to a denied target's name (or a name the owner would use for it) counts as touching that target,
 * when the effects meet (`effectsMeet`). Typed matching (`classMatch`) stays first; this only adds matches.
 */
export function tokenMatch(denied: Pick<DenialRecord, "units" | "actions" | "scopes" | "tool">, asked: ActionClass): { unit: DenialUnit; scope: DenialScope } | undefined {
  if (!asked.tokens?.length) return undefined;
  const tokens = new Set(asked.tokens.map((token) => token.replace(/\/+$/, "")));
  const effects = [...new Set(asked.units.flatMap((unit) => unit.effects))];
  for (const unit of unitsOf(denied)) {
    if (!effectsMeet(unit.effects, effects)) continue;
    for (const scope of unit.scopes) {
      if (scope.kind === "line") continue;
      const names = [scope.name, ...(scope.says ?? [])].filter((name) => name.length >= 2);
      if (names.some((name) => tokens.has(name))) return { unit, scope };
    }
  }
  return undefined;
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

const NEGATION = listWords("negate");
const RETRACTION = listWords("retract");

/**
 * The owner word newer than the denial (not among those seen when they denied it), of a lifting kind,
 * that asks for the denied effect on the scope: it names the scope, says an action word of the effect
 * (`CLASS_WORDS`), and is no question, negation or "wait". Undefined: still denied.
 */
export function liftedBy(denial: DenialRecord, scope: DenialScope, owner: OwnerWords | undefined, unit?: DenialUnit): string | undefined {
  if (!owner) return undefined;
  const seen = new Set(denial.seen);
  const units = unit ? [unit] : unitsOf(denial);
  const names = [...new Set([...namesOf(scope), ...units.flatMap((item) => item.scopes.filter((other) => scopeMatches(other, scope)).flatMap(namesOf))])];
  const actions = [...new Set(units.flatMap((item) => item.effects.flatMap((effect) => CLASS_WORDS[effect] ?? [])))];
  for (const word of [...owner.messages].reverse()) {
    if (seen.has(wordHash(word))) continue;
    if (!LIFTING_SOURCES.has(word.source ?? "typed")) continue;
    const text = consentText(word);
    const named = scope.kind === "line" ? text.includes(scope.name) : names.some((name) => mentions(text, name));
    if (!named) continue;
    if (QUESTION.test(text) || NEGATION.test(text) || RETRACTION.test(text)) continue;
    // A line the owner quotes whole asks for it; anything else must say what it asks for.
    if (scope.kind !== "line" && !actions.some((action) => actionWords(action).test(text))) continue;
    return word.text;
  }
  return undefined;
}

/**
 * What a Deny of `cls` records: a line that changes, pushes or sends something records that, not the reads
 * beside it (`git status && npm publish`: the publish); a line that only reads records its reads.
 */
export function forRecord(cls: ActionClass): ActionClass {
  const strong = cls.units.filter((unit) => unit.effects.some((effect) => effect !== "read"));
  if (!strong.length || strong.length === cls.units.length) return cls;
  return { ...cls, units: strong, scopes: [...new Set(strong.flatMap((unit) => unit.scopes))] };
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
    Array.isArray(record.seen) &&
    (record.units === undefined || (Array.isArray(record.units) && record.units.every((unit) => unit && Array.isArray(unit.effects) && Array.isArray(unit.scopes))))
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
    const scope = (item: DenialScope): DenialScope => ({ kind: item.kind, name: item.name, ...(item.says?.length ? { says: [...item.says] } : {}), ...(item.remote ? { remote: item.remote } : {}), ...(item.repo ? { repo: item.repo } : {}), ...(item.scope ? { scope: item.scope } : {}), ...(item.type ? { type: item.type } : {}) });
    const record: DenialRecord = {
      tool: cls.tool,
      actions: [...cls.actions],
      ...(cls.concern ? { concern: cls.concern } : {}),
      scopes: cls.scopes.map(scope),
      units: cls.units.map((unit) => ({ effects: [...unit.effects], scopes: unit.scopes.map(scope) })),
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
   * The newest denial that does what `asked` does to what it acts on (`classMatch`) and that the owner's
   * words since haven't lifted, with the scope it covers; and the denials of the same effect on other
   * targets (evidence for the judge).
   */
  check(asked: ActionClass, owner: OwnerWords | undefined): { active?: { denial: DenialRecord; scope: DenialScope }; similar: DenialRecord[] } {
    this.load();
    const similar: DenialRecord[] = [];
    for (const denial of [...this.records].reverse()) {
      const match = classMatch(denial, asked) ?? tokenMatch(denial, asked);
      if (match) {
        if (liftedBy(denial, match.scope, owner, match.unit) === undefined) return { active: { denial, scope: match.scope }, similar };
        continue;
      }
      // The same effect on another target (a read or a run only by the same program): evidence.
      const sameAction = denial.actions.some((action) => asked.actions.includes(action));
      if (unitsOf(denial).some((unit) => asked.units.some((other) => unit.effects.some((effect) => other.effects.includes(effect) && (sameAction || (effect !== "read" && effect !== "run")))))) similar.push(denial);
    }
    return { similar: similar.slice(0, 3) };
  }

  all(): readonly DenialRecord[] {
    this.load();
    return this.records;
  }
}
