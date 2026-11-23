/**
 * What a request does, and to what (round 5, design §7.4.1): the owner's Deny is matched by effect class
 * and target, not by the words of the command. `rm -rf data/fixtures` and `find data/fixtures -delete`
 * change the same folder; `cat ~/.cypher/logs/engine.log` and `head` of it read the same file; `git push
 * origin release/2.0` and `git push origin HEAD:release/2.0` push the same branch — and a branch is no
 * path, whatever its name looks like.
 *
 * Tables, no per-command rules:
 * - `EFFECT_CLASSES`: every action of the tool table (tools.ts `TOOLS`), the catalogue's operations, the
 *   program effects (tools.ts `REMOTE_EFFECTS`) and the file tools' capabilities, by effect class;
 * - `EFFECT_WRAPPERS`: programs that run the next one (`sudo`, `env`, `xargs`, `timeout`, …): the effect
 *   is the wrapped command's;
 * - `GIT_OPERANDS`: which git subcommands' operands are refs (branches, tags) and which are paths.
 *
 * Entities are typed (entities.ts kinds), so a branch never overlaps a path and the relation
 * (entities.ts `relation`: same, contains, contained) decides what overlaps.
 */

import { basename } from "node:path";

import type { EntityKind } from "./entities.ts";
import { sshHostIndex } from "./commands.ts";
import { readTool, remoteEffect } from "./tools.ts";
import { UNKNOWN } from "./shell.ts";

/** What a request does, as a class: what a denial is matched by. */
export type EffectClass = "read" | "change" | "stop" | "push" | "merge" | "publish" | "send" | "install" | "comment" | "create" | "run" | `tool:${string}`;

/** Every action word the tables use (tools.ts actions, catalogue ops, program effects, file capabilities), by effect class. */
export const EFFECT_CLASSES: Readonly<Record<string, EffectClass>> = {
  read: "read",
  write: "change",
  edit: "change",
  delete: "change",
  destroy: "change",
  erase: "change",
  rename: "change",
  archive: "change",
  close: "change",
  stop: "stop",
  push: "push",
  merge: "merge",
  publish: "publish",
  release: "publish",
  unpublish: "publish",
  tag: "publish",
  send: "send",
  reach: "send",
  network: "send",
  install: "install",
  apply: "install",
  comment: "comment",
  review: "comment",
  ready: "comment",
  reopen: "comment",
  create: "create",
  start: "create",
  run: "run",
};

/** The lexicon actions (lexicon.ts) whose words ask for an effect class: what a newer owner word must say to lift a denial. */
export const CLASS_WORDS: Readonly<Record<string, readonly string[]>> = {
  read: ["read"],
  change: ["delete", "erase", "write", "edit", "rename", "archive", "close"],
  stop: ["stop"],
  push: ["push"],
  merge: ["merge"],
  publish: ["publish", "release", "unpublish"],
  send: ["reach", "push", "publish"],
  install: ["install", "apply"],
  comment: ["comment", "review"],
  create: ["create"],
  run: ["reach", "apply"],
};

/** Programs that run the command after them (and their options): its effect is the wrapped command's. */
export const EFFECT_WRAPPERS: readonly { programs: readonly string[]; values?: readonly string[] }[] = [
  { programs: ["sudo", "doas"], values: ["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"] },
  { programs: ["env"], values: ["-u", "--unset", "-C", "--chdir", "-S", "--split-string"] },
  { programs: ["nohup", "time", "command", "builtin", "exec", "caffeinate", "chronic", "unbuffer"] },
  { programs: ["nice", "ionice"], values: ["-n", "-c", "--adjustment", "--class", "--classdata"] },
  { programs: ["timeout"], values: ["-s", "--signal", "-k", "--kill-after"] },
  { programs: ["stdbuf"], values: ["-i", "-o", "-e"] },
  { programs: ["xargs"], values: ["-I", "-J", "-L", "-n", "-P", "-s", "-E", "-d", "-a", "-R", "-S", "--max-args", "--max-procs", "--delimiter", "--arg-file", "--eof", "--replace", "--max-lines", "--max-chars"] },
  { programs: ["watch"], values: ["-n", "--interval", "-d"] },
];

/** The command a wrapper runs: its words after the wrappers, their options and values (`sudo -u www rm x` → `rm x`). */
export function unwrapped(words: readonly string[]): string[] {
  let rest = [...words];
  for (let depth = 0; depth < 8 && rest.length; depth++) {
    const row = EFFECT_WRAPPERS.find((candidate) => candidate.programs.includes(basename(rest[0] ?? "")));
    if (!row) break;
    let i = 1;
    for (; i < rest.length; i++) {
      const word = rest[i];
      if (word === UNKNOWN) break;
      if (row.values?.includes(word)) {
        i++;
        continue;
      }
      if (word.startsWith("-") || /^[A-Za-z_][A-Za-z0-9_]*=/.test(word) || (row.programs.includes("timeout") && /^\d+(\.\d+)?[smhd]?$/.test(word))) continue;
      break;
    }
    rest = rest.slice(i);
  }
  return rest;
}

/**
 * Git subcommands by what their operands are (data): refs (a branch, a tag) or paths. Words after `--`
 * are paths whatever the subcommand; `git push` reads refspecs (its destination is the ref pushed).
 */
export const GIT_OPERANDS: readonly { subs: readonly string[]; operands: "ref" | "path"; kind?: EntityKind }[] = [
  { subs: ["branch", "checkout", "switch", "merge", "rebase", "reset", "cherry-pick", "revert", "log", "diff", "show", "worktree", "pull", "fetch"], operands: "ref" },
  { subs: ["tag"], operands: "ref", kind: "tag" },
  { subs: ["add", "rm", "mv", "restore", "clean", "blame", "ls-files", "grep", "checkout-index", "update-index", "apply", "am"], operands: "path" },
];

/** One thing a command acts on, typed: a path (canonical), a branch, a tag, a pull request, a host, … */
export interface Acted {
  kind: EntityKind;
  name: string;
  remote?: string;
  repo?: string;
}

/** What one command does (`commandEffects`): its effect classes, and the typed things it acts on. */
export interface CommandEffects {
  effects: EffectClass[];
  targets: Acted[];
  /** Words that may be paths (path-shaped or a file program's operands), for the caller to make canonical. */
  paths: string[];
}

/** A word that looks like a path (absolute, `~`, relative with a slash or a leading dot), not a URL or an option. */
export function pathShaped(word: string): boolean {
  return word !== UNKNOWN && !word.startsWith("-") && !/^[a-z][\w+.-]*:\/\//i.test(word) && (word.startsWith("/") || word.startsWith("~") || word.startsWith(".") || word.includes("/"));
}

/** The class of an action word of the tables (`EFFECT_CLASSES`), else `run`. */
export function classOf(action: string): EffectClass {
  return action.startsWith("tool:") ? (action as EffectClass) : (EFFECT_CLASSES[action] ?? "run");
}

/** Options of git that take a value before the subcommand. */
const GIT_GLOBAL_VALUES = new Set(["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path", "--config-env"]);

/**
 * What a command does and to what, through its wrappers (`EFFECT_WRAPPERS`), by the tables: the tool
 * table's action and entities (tools.ts `readTool`), git's operands (`GIT_OPERANDS`, refspecs), the
 * program effects (tools.ts `REMOTE_EFFECTS`: `rm` changes, `kill` stops), what `find` runs or deletes,
 * a network sink's host. `sink`: whether it is a network sink (risk.ts `sinkOf`); `reads`: whether it only
 * reads what it names (risk.ts `fileEffect`).
 */
export function commandEffects(words: readonly string[], options: { sink?: (words: readonly string[]) => string | undefined; reads?: (words: readonly string[]) => boolean; current?: string } = {}): CommandEffects {
  const out: CommandEffects = { effects: [], targets: [], paths: [] };
  const effect = (value: EffectClass) => {
    if (!out.effects.includes(value)) out.effects.push(value);
  };
  const inner = unwrapped(words);
  if (!inner.length || inner[0] === UNKNOWN) {
    effect("run");
    return out;
  }
  const program = basename(inner[0]);
  // `find … -delete`, `find … -exec rm {} +`: what it runs, on where it starts.
  if (program === "find" || program === "fd") {
    const at = inner.findIndex((word) => /^-(exec|execdir|ok|okdir|x|X)$|^--exec/.test(word));
    const starts: string[] = [];
    for (const word of inner.slice(1)) {
      if (word === UNKNOWN || word.startsWith("-") || word === "!" || word === "(") break;
      starts.push(word);
    }
    out.paths.push(...(starts.length ? starts : ["."]));
    if (inner.includes("-delete")) effect("change");
    if (at >= 0) for (const value of commandEffects(inner.slice(at + 1).filter((word) => word !== "{}" && word !== ";" && word !== "+"), options).effects) effect(value);
    if (!out.effects.length) effect("read");
    return out;
  }
  // The tool table: its action, its entities.
  const tool = readTool(inner);
  if (tool && "row" in tool) {
    effect(classOf(tool.action));
    for (const entity of tool.entities) if (entity.name) out.targets.push({ kind: entity.kind, name: entity.name, ...(entity.repo ? { repo: entity.repo } : {}) });
  }
  if (program === "git") {
    let i = 1;
    for (; i < inner.length; i++) {
      const word = inner[i];
      if (GIT_GLOBAL_VALUES.has(word)) i++;
      else if (!word.startsWith("-")) break;
    }
    const sub = inner[i];
    const args = inner.slice(i + 1);
    if (sub === "push") {
      effect("push");
      const positionals = args.filter((word) => word !== UNKNOWN && !word.startsWith("-"));
      const [remote, ...refspecs] = positionals;
      for (const refspec of refspecs) {
        const target = refspec.replace(/^\+/, "").split(":").pop() ?? "";
        const dest = target.replace(/^refs\/heads\//, "");
        if (!dest || dest === "HEAD") {
          if (options.current) out.targets.push({ kind: "branch", name: options.current, ...(remote ? { remote } : {}) });
          continue;
        }
        if (dest.startsWith("refs/tags/")) out.targets.push({ kind: "tag", name: dest.slice("refs/tags/".length), ...(remote ? { remote } : {}) });
        else out.targets.push({ kind: "branch", name: dest, ...(remote ? { remote } : {}) });
      }
      if (remote && !refspecs.length && options.current) out.targets.push({ kind: "branch", name: options.current, remote });
      return out;
    }
    const row = GIT_OPERANDS.find((candidate) => candidate.subs.includes(sub ?? ""));
    const dashes = args.indexOf("--");
    const before = dashes < 0 ? args : args.slice(0, dashes);
    const after = dashes < 0 ? [] : args.slice(dashes + 1);
    out.paths.push(...after.filter((word) => word !== UNKNOWN));
    if (row?.operands === "ref") for (const word of before) if (word !== UNKNOWN && !word.startsWith("-")) out.targets.push({ kind: row.kind ?? "branch", name: word.replace(/^refs\/heads\//, "") });
    if (row?.operands === "path") out.paths.push(...before.filter((word) => word !== UNKNOWN && !word.startsWith("-")));
    if (/^(branch|tag)$/.test(sub ?? "") && args.some((word) => /^-[A-Za-z]*[dD]|^--delete$/.test(word))) effect("change");
    else if (/^(rm|clean|reset|restore|checkout|mv)$/.test(sub ?? "")) effect("change");
    else if (/^(commit|add|merge|rebase|cherry-pick|revert|stash|apply|am|tag|branch|switch|worktree|pull|fetch)$/.test(sub ?? "")) effect(sub === "merge" ? "merge" : "change");
    else effect("read");
    return out;
  }
  // What the program does (tools.ts `REMOTE_EFFECTS`, read here for this machine): delete, stop, apply, write.
  const changes = remoteEffect(inner);
  if (changes) effect(classOf(changes.action));
  const sink = options.sink?.(inner);
  if (sink) effect("send");
  if (/^(ssh|mosh)$/.test(program)) {
    const at = sshHostIndex(inner);
    const host = at === undefined ? undefined : inner[at];
    if (host !== undefined && host !== UNKNOWN) out.targets.push({ kind: "host", name: host.replace(/^[^@]*@/, "") });
  }
  if (/^(scp|sftp|rsync)$/.test(program)) {
    for (const word of inner.slice(1)) {
      const host = /^(?:[\w.-]+@)?([\w-]+(?:\.[\w-]+)*):/.exec(word)?.[1];
      if (host) out.targets.push({ kind: "host", name: host });
    }
  }
  for (const word of inner.slice(1)) {
    const url = /^[a-z][\w+.-]*:\/\/(?:[^@/]*@)?([^/:?#]+)/i.exec(word)?.[1];
    if (url) out.targets.push({ kind: "host", name: url });
  }
  // Its operands: a file program's, or what is shaped like a path. A program the tool table reads names entities, not paths.
  if (!tool || !("row" in tool)) for (const word of inner.slice(1)) if (pathShaped(word) || (changes !== undefined && word !== UNKNOWN && !word.startsWith("-") && !/^[a-z][\w+.-]*:\/\//i.test(word))) out.paths.push(word);
  if (!out.effects.length) effect(options.reads?.(inner) === false ? "run" : "read");
  return out;
}
