/**
 * What this task made: the facts that let the reviewing model approve a
 * dangerous-looking action that only touches the chat's own leavings (its
 * worktrees and branches, its private development instance, its scratch
 * files and the processes it started).
 *
 * The facts are read from the commands the chat actually ran, not from what
 * the model says: every bash call this extension lets through is parsed here
 * (`creationsOf`), and what it creates is recorded (`CreatedStore`, kept in
 * the Pi session like a grant, tagged with the chat id). A later `git
 * worktree remove`, `git branch -D`, `rm -rf` or `kill` is then checked
 * against that record (risk.ts `facts.created`): what the chat made is local
 * work, everything else stays dangerous.
 *
 * Nothing sensitive, protected, Cypher's own, or the installed app's is ever
 * recorded, however it was created: a record must never turn a write the
 * user approved once into a free hand to delete it later.
 */

import { basename, isAbsolute, join, resolve } from "node:path";

import { UNKNOWN } from "./shell.ts";

export const CREATED_ENTRY = "cypher-permissions.created";

/** One command of a line, as core.ts reads it. */
export interface CreationCommand {
  words: readonly string[];
  /** The directory it runs in; undefined when known only when it runs. */
  cwd: string | undefined;
  /** Files its redirections write, as written. */
  writes: readonly string[];
  /** Values assigned in front of it and its wrappers' words. */
  carried?: readonly string[];
  /** Variables assigned in front of it, by name, with their values in order. */
  env?: readonly string[];
  envValues?: readonly string[];
}

/** What a line creates, as paths and git names. */
export interface Creations {
  /** Directories and files it makes (canonical paths). */
  paths: string[];
  /** Worktrees it adds (canonical paths). */
  worktrees: string[];
  /** Branches it creates. */
  branches: string[];
}

/** What recording needs from the machine. */
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

/** Options of `git worktree add` that take a value. */
const WORKTREE_VALUES = new Set(["-b", "-B", "--reason", "--orphan-branch"]);

/** Options of `mkdir` and `install` that take a value. */
const MODE_VALUES = new Set(["-m", "--mode", "-o", "--owner", "-g", "--group"]);

/** A dev instance's data directories: `~/.cypher-development/<mode>-<part>[-<name>]`. */
function devInstanceDirs(name: string, home: string): string[] {
  // Never the whole `~/.cypher-development`: other chats keep their instances there too.
  const root = join(home, ".cypher-development");
  const dirs: string[] = [];
  for (const mode of ["local", "dev"]) {
    for (const part of ["engine", "ui"]) {
      const base = `${mode}-${part}${name ? `-${name}` : ""}`;
      dirs.push(join(root, base), join(root, `${base}.log`));
    }
  }
  if (name) dirs.push(join(root, `dev-engine-${name}`), join(root, `dev-ui-${name}`));
  return dirs;
}

/** `NAME=value` as a command carries it, for the names a dev instance is named by. */
const INSTANCE_ASSIGNMENT = /^(CYPHER_DEV_INSTANCE|CYPHER_DATA_DIR|CYPHER_ENGINE_DATA_DIR|CYPHER_CAPTURE_DIR)=(.*)$/;

/** Everything one line creates, from its commands as the shell reader read them. */
export function creationsOf(commands: readonly CreationCommand[], facts: CreationFacts): Creations {
  const out: Creations = { paths: [], worktrees: [], branches: [] };
  const add = (list: string[], path: string | undefined) => {
    if (path === undefined || !facts.recordable(path) || list.includes(path)) return;
    list.push(path);
  };
  const resolved = (word: string, cwd: string | undefined) => (word === UNKNOWN || word.startsWith("-") ? undefined : facts.canonical(word, cwd));
  for (const command of commands) {
    const words = command.words;
    if (words.length === 0) continue;
    const program = basename(words[0] === UNKNOWN ? "" : words[0]);
    const args = words.slice(1);
    const operands = args.filter((word) => word !== UNKNOWN && !word.startsWith("-"));
    for (const path of command.writes) add(out.paths, resolved(path, command.cwd));
    // A dev instance names its own data directories, whoever starts it.
    const assignments = (command.env ?? []).map((name, index) => `${name}=${command.envValues?.[index] ?? UNKNOWN}`);
    for (const word of [...words, ...(command.carried ?? []), ...assignments]) {
      const match = INSTANCE_ASSIGNMENT.exec(word);
      if (!match) continue;
      const [, name, value] = match;
      if (value === UNKNOWN) continue;
      if (name === "CYPHER_DEV_INSTANCE") {
        if (!/^[A-Za-z0-9._-]*$/.test(value)) continue;
        for (const dir of devInstanceDirs(value, facts.home)) add(out.paths, dir);
      } else {
        const dir = facts.canonical(value, command.cwd);
        if (dir === undefined) continue;
        add(out.paths, dir);
        add(out.paths, `${dir}.log`);
      }
    }
    switch (program) {
      case "git": {
        const { sub, rest, dir } = gitParts(words, command.cwd);
        if (sub === "worktree" && rest[0] === "add") {
          let branch: string | undefined;
          const places: string[] = [];
          for (let i = 1; i < rest.length; i++) {
            const word = rest[i];
            if (WORKTREE_VALUES.has(word)) {
              const value = rest[++i];
              if (value !== undefined && value !== UNKNOWN && (word === "-b" || word === "-B" || word === "--orphan-branch")) branch = value;
              continue;
            }
            if (word.startsWith("-")) continue;
            places.push(word);
          }
          const place = places[0];
          if (place !== undefined && place !== UNKNOWN) {
            const path = facts.canonical(place, dir);
            add(out.worktrees, path);
            add(out.paths, path);
          }
          if (branch) add(out.branches, branch);
          break;
        }
        if (sub === "clone") {
          const places = rest.filter((word) => word !== UNKNOWN && !word.startsWith("-"));
          const place = places[1] ?? (places[0] ? basename(places[0].replace(/\/+$/, "").replace(/\.git$/, "")) : undefined);
          if (place) add(out.paths, facts.canonical(place, dir));
          break;
        }
        if (sub === "switch" || sub === "checkout") {
          const at = rest.findIndex((word) => /^(-c|-C|-b|-B|--create|--force-create)$/.test(word));
          const name = at >= 0 ? rest[at + 1] : undefined;
          if (name !== undefined && name !== UNKNOWN && !name.startsWith("-")) add(out.branches, name);
          break;
        }
        if (sub === "branch") {
          // `git branch <name> [start]`, never a delete, a rename or a listing.
          if (rest.some((word) => /^(-d|-D|-m|-M|-c|-C|--delete|--move|--copy|--list|-l|-a|-r)$/.test(word))) break;
          const name = rest.find((word) => word !== UNKNOWN && !word.startsWith("-"));
          if (name) add(out.branches, name);
          break;
        }
        break;
      }
      case "mkdir":
      case "install": {
        if (program === "install" && !args.some((word) => word === "-d" || word === "--directory")) break;
        for (let i = 0; i < args.length; i++) {
          const word = args[i];
          if (MODE_VALUES.has(word)) {
            i++;
            continue;
          }
          if (word === UNKNOWN || word.startsWith("-")) continue;
          add(out.paths, facts.canonical(word, command.cwd));
        }
        break;
      }
      case "touch":
      case "tee": {
        for (const word of operands) add(out.paths, facts.canonical(word, command.cwd));
        break;
      }
      case "cp":
      case "mv":
      case "rsync":
      case "ditto": {
        // Its destination: the last operand (a local one).
        const dest = operands.at(-1);
        if (dest !== undefined && !/^[^/]*:/.test(dest)) add(out.paths, facts.canonical(dest, command.cwd));
        break;
      }
      case "mktemp": {
        // The template says where, not which name: record the directory.
        const template = operands[0];
        if (template && template.includes("/")) add(out.paths, facts.canonical(template.replace(/\/[^/]*$/, ""), command.cwd));
        break;
      }
    }
  }
  return out;
}

/** git's global options, and the directory `-C` moves to (as explicit.ts reads them). */
function gitParts(words: readonly string[], cwd: string | undefined): { sub?: string; rest: string[]; dir: string | undefined } {
  let dir = cwd;
  let i = 1;
  for (; i < words.length; i++) {
    const word = words[i];
    if (word === "-C") {
      const next = words[++i];
      if (next === undefined || next === UNKNOWN) dir = undefined;
      else dir = isAbsolute(next) ? next : dir === undefined ? undefined : resolve(dir, next);
    } else if (word === "-c" || word === "--git-dir" || word === "--work-tree" || word === "--namespace") i++;
    else if (!word.startsWith("-")) break;
  }
  return { sub: words[i], rest: words.slice(i + 1), dir };
}

/** What one record holds, as it is kept in the session. */
export interface CreatedRecord {
  paths?: string[];
  worktrees?: string[];
  branches?: string[];
  chatId?: string;
  createdAt?: string;
}

/** How much one chat may record (a bound on the session entries and the memory). */
const MAX_PATHS = 400;
const MAX_BRANCHES = 200;

/**
 * What the chat has made, in this process and in its earlier turns (the
 * session's `cypher-permissions.created` entries).
 */
export class CreatedStore {
  private readonly pathSet = new Set<string>();
  private readonly worktreeSet = new Set<string>();
  private readonly branchSet = new Set<string>();

  private readonly chatId: string | undefined;

  constructor(chatId: string | undefined) {
    this.chatId = chatId;
  }

  /** Read the records of this chat from the session's entries. */
  restore(entries: readonly { type?: string; customType?: string; data?: unknown }[]): void {
    for (const entry of entries) {
      if (entry.type !== "custom" || entry.customType !== CREATED_ENTRY) continue;
      const record = entry.data as CreatedRecord | undefined;
      if (!record || typeof record !== "object") continue;
      if (this.chatId && record.chatId !== this.chatId) continue;
      this.take(record);
    }
  }

  private take(record: CreatedRecord): void {
    for (const path of record.paths ?? []) if (typeof path === "string" && this.pathSet.size < MAX_PATHS) this.pathSet.add(path);
    for (const path of record.worktrees ?? []) if (typeof path === "string" && this.worktreeSet.size < MAX_PATHS) this.worktreeSet.add(path);
    for (const name of record.branches ?? []) if (typeof name === "string" && this.branchSet.size < MAX_BRANCHES) this.branchSet.add(name);
  }

  /** Record what a line created; returns what was new, to keep in the session. */
  note(creations: Creations): CreatedRecord | undefined {
    const fresh: CreatedRecord = {};
    const paths = creations.paths.filter((path) => !this.pathSet.has(path));
    const worktrees = creations.worktrees.filter((path) => !this.worktreeSet.has(path));
    const branches = creations.branches.filter((name) => !this.branchSet.has(name));
    if (paths.length) fresh.paths = paths;
    if (worktrees.length) fresh.worktrees = worktrees;
    if (branches.length) fresh.branches = branches;
    if (Object.keys(fresh).length === 0) return undefined;
    this.take(fresh);
    return { ...fresh, ...(this.chatId ? { chatId: this.chatId } : {}), createdAt: new Date().toISOString() };
  }

  /** A canonical path this chat made, or one inside a directory it made. */
  has(path: string): boolean {
    if (this.pathSet.has(path) || this.worktreeSet.has(path)) return true;
    for (const made of this.pathSet) if (path.startsWith(`${made}/`)) return true;
    for (const made of this.worktreeSet) if (path.startsWith(`${made}/`)) return true;
    return false;
  }

  worktree(path: string): boolean {
    if (this.worktreeSet.has(path)) return true;
    for (const made of this.worktreeSet) if (path.startsWith(`${made}/`)) return true;
    return false;
  }

  branch(name: string): boolean {
    return this.branchSet.has(name);
  }

  /** For the status text and tests. */
  all(): { paths: string[]; worktrees: string[]; branches: string[] } {
    return { paths: [...this.pathSet], worktrees: [...this.worktreeSet], branches: [...this.branchSet] };
  }
}

export { EMPTY as NO_CREATIONS };
