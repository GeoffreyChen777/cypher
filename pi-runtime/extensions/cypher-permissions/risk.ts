/**
 * Risk tiers for commands that would leave the sandbox (design:
 * docs/design/permission-friction.md §3). A tier is a property of the
 * effect, not of the program:
 *
 * - `inspect` (R0): changes nothing and moves no data off the machine. Runs
 *   outside the sandbox without asking.
 * - `local` (R1): changes only this machine's, the user's, undoable things,
 *   wherever they are. The reviewing model decides; failing that the user,
 *   once per kind.
 * - `danger` (R2): irreversible, reaches other people or shared systems, or
 *   persists beyond the task. The reviewing model may approve one only on a
 *   fact Cypher checks itself: the owner's own words asking for exactly it
 *   (explicit.ts), or that it touches nothing but what this task made
 *   (created.ts, `facts.created`).
 * - `floor` on top of `danger`: the few things no rule, grant or model ever
 *   approves (a secret leaving the machine, a change to the permission
 *   system itself). The user decides, every time.
 *
 * The classifier is deterministic and conservative: a word known only when
 * the line runs counts as whatever would make the command dangerous, and a
 * line the reader cannot parse is never `inspect`. Program knowledge lives
 * in packs.ts; this file holds the checks that need the machine (paths,
 * processes, the repository) and the ones that span packs.
 */

import { basename, isAbsolute } from "node:path";

import { statSync } from "node:fs";

import { copyOperands, patternMatches, remoteOperand, ruleCannotVouch, sshHostIndex, unwrap } from "./commands.ts";
import { asksOnly, DANGER, dangerMatches, INSPECT, inspectMatches, viewOf } from "./packs.ts";
import { isUnder } from "./paths.ts";
import type { CommandView, PackFacts } from "./packs.ts";
import type { Classification } from "./paths.ts";
import { UNKNOWN } from "./shell.ts";
import { genericDestroyer, TOLD_BY_PLACE } from "./catalogue.ts";
import { gradeOf, judgeDecides } from "./grades.ts";

export type Tier = "inspect" | "local" | "danger";

export const TIER_RANK: Readonly<Record<Tier, number>> = { inspect: 0, local: 1, danger: 2 };

export interface Assessment {
  tier: Tier;
  /** An inspection the sandbox cannot run (packs.ts `outside`). */
  outside?: boolean;
  /** Why, for the dialog and the judge: "publishes a package". */
  kind?: string;
  /**
   * The kind of danger, for task grants: an approval covers later commands
   * with the same key (every tag of a release, not a force-push).
   */
  key?: string;
  /**
   * The always-ask floor: the user decides, whatever anyone says. Never set
   * for anything but a secret leaving the machine or a change to the
   * permission system itself.
   */
  floor?: true;
  /** An approval covers this call alone and is never remembered (a secret read). */
  once?: true;
  /**
   * A concern the classifier itself established for a local command (judge.ts `CONCERNS`): a secret
   * read, a read of the installed app's data. The reviewing model's ask on it always stands; an
   * installed-app read also runs only on owner words cited as consent (core.ts).
   */
  concern?: "secret" | "installed-app";
  /** For an installed-app read: the data's path, whose names a consent must say (explicit.ts `checkDataConsent`). */
  data?: string;
}

/**
 * Text that shows a command means to change a file it names, not read it:
 * an interpreter opening it for writing, a database writing or restoring,
 * a script deleting or renaming. Read by `fileEffect` for programs whose
 * operands could be either.
 */
const WRITE_HINT =
  /\bopen\s*\([^)]*['"][waxr]\+?[bt+]*['"]|>\s*["'$\w/~.]|\bwrite(?:lines|_text|_bytes|File|FileSync)?\s*\(|\bdump\s*\(|\bunlink\b|\brmtree\b|\bremove\s*\(|\brename\s*\(|\bmkdir\b|\bchmod\b|\bcopy(?:file|tree)?\s*\(|\btruncate\b|\.backup\b|\.restore\b|\.import\b|\binsert\s+into\b|\bupdate\s+\w+\s+set\b|\bdelete\s+from\b|\bdrop\s+(table|index)\b|\balter\s+table\b|\bcreate\s+(table|index)\b|\bvacuum\b|\bsave\b/i;

/** Programs that only read the files they name. */
const READERS: ReadonlySet<string> = new Set([
  "cat", "head", "tail", "less", "more", "bat", "grep", "rg", "egrep", "fgrep", "wc", "sort", "uniq", "cut", "tr", "nl", "fold", "column", "rev",
  "diff", "cmp", "comm", "file", "strings", "xxd", "od", "hexdump", "md5", "md5sum", "shasum", "sha1sum", "sha256sum", "cksum", "jq", "yq", "plutil",
  "awk", "gawk", "json_pp", "xmllint", "iconv", "base64", "gunzip", "zcat", "bzcat", "xzcat", "open", "qlmanage", "wc",
]);

/** Interpreters and tools whose operands may be read or written: their text says which. */
const EITHER = /^(python\d*(\.\d+)?|node|deno|bun|ruby|perl|php|lua|osascript|sqlite3|sed|tee|tar|unzip|zip|ditto|plutil|defaults|jq|java)$/;

/**
 * Whether a command changes a path it names, or only reads it: a copy reads
 * its sources and writes its destination, and anything else is decided by the
 * program (`fileEffect`).
 */
function pathEffect(command: RiskCommand, view: CommandView, path: string): "read" | "write" {
  if (/^(cp|mv|rsync|scp|ditto|install|ln)$/.test(view.program)) {
    const copy = copyOperands(view.words);
    if (copy) return path === copy.dest ? "write" : "read";
  }
  return fileEffect(command, view, path);
}

/**
 * Whether a command changes the file it names, or only reads it. A program
 * this cannot place counts as writing: the facts a danger may be approved on
 * must never rest on a guess.
 */
export function fileEffect(command: RiskCommand, view: CommandView, path: string): "read" | "write" {
  if (command.writes.includes(path)) return "write";
  if (command.reads.includes(path)) return "read";
  const program = view.program;
  // `mkdir` is named-only for reading a secret's directory, but it does make it.
  if (program !== "mkdir" && (NAME_ONLY.has(program) || READERS.has(program))) return "read";
  // `find` and friends only list what they name, unless told to delete it,
  // write a file, or run a program on it that is more than a reader.
  if (/^(find|fd|tree|du|rg|ag|ack)$/.test(program)) {
    if (view.args.some((word) => /^-(delete|fprint.*|fls)$/.test(word))) return "write";
    const runs = findRuns(view);
    if (runs === undefined) return "write";
    return runs.every((run) => readsOnly(basename(run[0] ?? ""))) ? "read" : "write";
  }
  if (EITHER.test(program)) {
    if (program === "sed" || program === "perl") return view.args.some((word) => /^-[A-Za-z]*i/.test(word) || word.startsWith("--in-place")) ? "write" : "read";
    if (program === "sqlite3" && (view.args.includes("-readonly") || view.words.some((word) => /[?&]mode=ro\b/.test(word)))) return "read";
    if (program === "tar" || program === "unzip" || program === "zip" || program === "ditto") return "write";
    if (program === "tee") return "write";
    return view.words.some((word) => word !== UNKNOWN && WRITE_HINT.test(word)) ? "write" : "read";
  }
  return "write";
}

/** A program that only reads the files it names, or only their names. */
function readsOnly(program: string): boolean {
  return program !== "mkdir" && (NAME_ONLY.has(program) || READERS.has(program));
}

/** The words that end what `find -exec` runs. */
const FIND_RUNS = /^-(exec|execdir|ok|okdir)$/;

/**
 * The commands `find` (or `fd -x`) runs on what it finds, each with its
 * words as written (`{}` stands for a found file). Empty when it runs none;
 * undefined when one cannot be read (a word known only when it runs, no
 * program after `-exec`).
 */
function findRuns(view: CommandView): string[][] | undefined {
  const args = view.args;
  const runs: string[][] = [];
  if (view.program === "fd") {
    const at = args.findIndex((word) => /^(-x|-X|--exec|--exec-batch)$/.test(word));
    if (at < 0) return args.includes(UNKNOWN) ? undefined : [];
    const run = args.slice(at + 1);
    return run.length === 0 || run[0] === UNKNOWN ? undefined : [run];
  }
  if (view.program !== "find") return [];
  if (args.includes(UNKNOWN)) return undefined;
  for (let i = 0; i < args.length; i++) {
    if (!FIND_RUNS.test(args[i])) continue;
    const run: string[] = [];
    for (i++; i < args.length && args[i] !== ";" && args[i] !== "+"; i++) run.push(args[i]);
    if (run.length === 0) return undefined;
    runs.push(run);
  }
  return runs;
}

/** Where `find` starts: its operands before its tests (`find -L dir -name x`), `.` when it names none. */
function findStarts(view: CommandView): string[] {
  const starts: string[] = [];
  let i = 0;
  // Options before the paths (`find -L dir`), BSD's `-f path` among them.
  for (; i < view.args.length; i++) {
    const word = view.args[i];
    if (/^-[HLPEXdsx]+$/.test(word) || /^-O\d$/.test(word) || word === "-D") {
      if (word === "-D") i++;
      continue;
    }
    if (word === "-f") {
      const path = view.args[++i];
      if (path !== undefined) starts.push(path);
      continue;
    }
    break;
  }
  for (; i < view.args.length; i++) {
    const word = view.args[i];
    if (word !== UNKNOWN && (word.startsWith("-") || word === "!" || word === "(")) break;
    starts.push(word);
  }
  return starts.length ? starts : ["."];
}

/** The danger a kind names, without the specifics that differ from command to command (which tag). */
function dangerKey(kind: string): string {
  if (/tag/.test(kind)) return "push-tag";
  if (/force-push|deletes a remote branch/.test(kind)) return "push-force";
  if (/pushes .* known only when it runs|cannot tell/.test(kind)) return "push-unknown";
  return kind;
}

/** One command of a line, seen through its wrappers. */
export interface RiskCommand {
  words: readonly string[];
  /** The directory it runs in; undefined when known only when it runs. */
  cwd: string | undefined;
  reads: readonly string[];
  writes: readonly string[];
  /** Words it carries besides its own: values assigned in front of it, its wrappers' words. */
  carried?: readonly string[];
  /** What each word known only when it runs surely starts with (shell.ts `hints`), one per word. */
  hints?: readonly (string | undefined)[];
  /** It is a shell or interpreter that runs code from its input, which is known only when it runs (`… | sh`, `sh < file`). */
  stdinCode?: boolean;
}

/** What `kill`, `pkill` and `killall` would stop. */
export type KillTarget = { pids: number[] } | { pgrep: string[] };

/** What can be told about the processes a kill would stop (each undefined when it cannot be). */
export interface ProcessFacts {
  /** Every one carries this chat's marker: it started them, or they descend from its commands. */
  owned?: boolean;
  /** Every one runs in the chat's project folders (its working directory). */
  inRoots?: boolean;
  /** Every one runs in, or on, what this task made (created.ts): its dev instance, its worktrees. */
  inCreated?: boolean;
  /** One is the installed Cypher app's (its program or folder). */
  installedApp?: boolean;
}

export interface GitFacts {
  /** The branch checked out, if any. */
  current?: string;
  /** Branches a push to which publishes: the remote's default, `main`, `master`. */
  defaults: string[];
  isBranch(name: string): boolean;
}

export interface RiskFacts extends PackFacts {
  /** A path's classification as the command sees it; undefined when relative to a directory not known. */
  classify(path: string, cwd: string | undefined): Classification | undefined;
  /** A canonical path the agent may delete or move without danger: the workspace, scratch, caches. */
  disposable(path: string): boolean;
  /** Names that mark a target as production (globs, case-insensitive). */
  protectedTargets: readonly string[];
  /** The user's own inspection patterns. */
  inspect: readonly string[][];
  /** Whether every target is a process this chat started; undefined when that cannot be told. */
  ownsProcesses(target: KillTarget): boolean | undefined;
  /** More about the targets: where they run, whose they are. Without it, only `ownsProcesses`. */
  processFacts?(target: KillTarget): ProcessFacts;
  /** A canonical path that is the installed Cypher app or its data (not a chat's worktree). */
  installedApp?(path: string): boolean;
  /** The repository at a directory (undefined: none, or not known). */
  git(dir: string): GitFacts | undefined;
  /** A directory inside the chat's workspace. */
  inWorkspace(dir: string): boolean;
  /** The branches checked out in the chat's project folders, bar a default branch: the chat's own work. */
  workspaceBranches?(): readonly string[];
  /** What this task made (created.ts): what it may undo without asking anyone. */
  created?: {
    has(path: string): boolean;
    worktree(path: string): boolean;
    branch(name: string): boolean;
    /**
     * What a command deletes or stops (catalogue.ts), and whether every
     * target is a follow-up of what the ledger holds (lifecycle.ts): what
     * this task created or started, the head branch of a pull request it
     * merged. `why`: the lifecycle rules.
     */
    destroys?(command: RiskCommand): { own: boolean; op: "destroy" | "stop"; why: string[]; kinds: readonly string[] } | undefined;
    /** The ledger holds something of any kind by this name. */
    named?(name: string): boolean;
    /** A pull request this chat opened (the ledger's), by number. */
    pullRequest?(number: string): boolean;
  };
  /** A canonical path of the permission system itself (config.ts): a change there always asks. */
  permissionSystem?(path: string): boolean;
}

/** A path the task may delete, move or discard: the workspace, scratch, caches, or what it made itself. */
function disposable(path: string, facts: RiskFacts): boolean {
  return facts.disposable(path) || facts.created?.has(path) === true;
}

/** Programs whose file operands are read only by name (listing a key's directory reads no key). */
const NAME_ONLY: ReadonlySet<string> = new Set(["ls", "stat", "du", "readlink", "realpath", "basename", "dirname", "which", "test", "[", "cd", "mkdir"]);

/** Programs that change the files their operands name. */
const WRITERS: ReadonlySet<string> = new Set([
  "cp", "mv", "ln", "install", "tee", "touch", "chmod", "chown", "chgrp", "chflags", "xattr", "rm", "rmdir", "unlink", "truncate", "shred", "srm",
  "sed", "perl", "rsync", "ditto", "dd", "patch", "mkdir", "trash", "tar", "unzip", "git",
]);

/** Programs that delete or move what their operands name. */
const DESTROYERS: ReadonlySet<string> = new Set(["rm", "rmdir", "unlink", "shred", "srm", "trash", "truncate", "mv"]);

/** Programs that reach a remote system: protected target names apply to them. */
const REMOTE: ReadonlySet<string> = new Set([
  "ssh", "scp", "sftp", "rsync", "mosh", "gh", "glab", "kubectl", "helm", "k9s", "terraform", "tofu", "pulumi", "aws", "gcloud", "az", "doctl",
  "fly", "flyctl", "heroku", "vercel", "netlify", "firebase", "supabase", "wrangler", "psql", "pg_dump", "pg_restore", "mysql", "mysqldump",
  "mongosh", "mongo", "redis-cli", "sqlcmd", "cqlsh", "curl", "wget", "http", "https", "ansible", "ansible-playbook", "serverless", "sls", "cdk",
  "sst", "railway", "docker", "podman",
]);

const DOWNLOADERS: ReadonlySet<string> = new Set(["curl", "wget", "fetch", "aria2c", "http", "https"]);
const INTERPRETERS = /^(sh|bash|zsh|dash|ksh|fish|python\d*(\.\d+)?|node|deno|bun|ruby|perl|php|lua|osascript)$/;

function globMatches(glob: string, word: string): boolean {
  const source = glob.toLowerCase().replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
  return new RegExp(`^${source}$`).test(word.toLowerCase());
}

/** The names a word can stand for: itself, an option's value, a URL's or `user@host`'s host. */
function targetNames(word: string): string[] {
  const names = [word];
  const value = /^--?[\w-]+=(.+)$/.exec(word)?.[1];
  if (value) names.push(value);
  for (const text of value ? [word, value] : [word]) {
    const url = /^[a-z][\w+.-]*:\/\/(?:[^@/]*@)?([^/:?#]+)/i.exec(text);
    if (url) names.push(url[1], url[1].split(".")[0]);
    const host = /^(?:[\w.-]+@)?([\w-]+(?:\.[\w-]+)*)(?::|$)/.exec(text);
    if (host && (text.includes("@") || text.includes(":"))) names.push(host[1], host[1].split(".")[0]);
  }
  return names;
}

/** gh and glab name what they reach with these options (or a URL); their other words are titles, labels, searches. */
const HOSTING_TARGET_OPTIONS = /^(-R|--repo|--hostname|-H)$/;

function protectedTarget(view: CommandView, facts: RiskFacts): string | undefined {
  if (!REMOTE.has(view.program) && view.program !== "git") return undefined;
  if (view.program === "git" && view.positionals[0] !== "push") return undefined;
  const words =
    view.program === "gh" || view.program === "glab"
      ? view.args.filter((word, i) => HOSTING_TARGET_OPTIONS.test(view.args[i - 1] ?? "") || /^(--repo|--hostname)=/.test(word) || /:\/\//.test(word))
      : view.args;
  for (const word of words) {
    if (word === UNKNOWN) continue;
    for (const name of targetNames(word)) {
      if (facts.protectedTargets.some((glob) => globMatches(glob, name))) return `reaches ${name}, a protected target`;
    }
  }
  return undefined;
}

/** Path-like words a command names: absolute, `~`, or relative with a slash or a leading dot. */
function pathWords(words: readonly string[]): string[] {
  return words.filter((word) => word !== UNKNOWN && !word.startsWith("-") && (word.startsWith("/") || word.startsWith("~") || word.startsWith(".") || word.includes("/")) && !/^[a-z][\w+.-]*:\/\//i.test(word));
}

/**
 * What a command's redirections make dangerous, whatever the command: `echo
 * x >> ~/.zshrc` changes a file that runs code later, `cat < key` reads a
 * secret. Every command is checked, inert builtins and filters too.
 */
export function redirectRisk(command: Pick<RiskCommand, "reads" | "writes" | "cwd">, facts: RiskFacts): Assessment | undefined {
  let soft: Assessment | undefined;
  for (const path of command.reads) {
    // `< "$file"`: which file, nobody can say before it runs. No inspection, and no rule's to vouch for (core.ts).
    if (path === UNKNOWN) {
      soft ??= { tier: "local", kind: "reads a file known only when it runs" };
      continue;
    }
    const classification = facts.classify(path, command.cwd) ?? named(path, facts);
    if (!classification) continue;
    // Reading a secret sends it to the model provider: the model may approve
    // it for the request at hand, once, and nothing is remembered.
    if (classification.zone === "sensitive") soft ??= { tier: "local", kind: `reads a secret (${path})`, once: true, concern: "secret" };
  }
  for (const path of command.writes) {
    if (path === UNKNOWN) {
      soft ??= { tier: "local", kind: "writes a file known only when it runs" };
      continue;
    }
    const classification = facts.classify(path, command.cwd) ?? named(path, facts);
    // A name in a directory known only when it runs that could be no secret
    // and no startup file is ordinary work.
    if (!classification) continue;
    if (facts.permissionSystem?.(classification.path)) return { tier: "danger", kind: `changes the permission system itself (${path})`, key: "permission-system", floor: true };
    const why = writeDanger(classification, facts);
    if (why) return { tier: "danger", kind: `${why} (${path})`, key: dangerKey(why) };
  }
  return soft;
}

/**
 * What a path in a directory known only when it runs could be, by its name
 * alone: `cat > step2.py` could land anywhere, but only a name like
 * `id_rsa` or `.zshrc` could be a secret or a startup file. Undefined when
 * the name is an ordinary one, whichever directory it turns out to be in.
 */
function named(path: string, facts: RiskFacts): Classification | undefined {
  const home = facts.classify("~", undefined)?.path;
  if (home === undefined) return undefined;
  const classification = facts.classify(basename(path), home);
  if (!classification) return undefined;
  return classification.zone === "sensitive" || classification.guard !== undefined ? classification : undefined;
}

/** Programs that send a file named `@file` (`curl -d @f`, `-F x=@f`, `http x=@f`). */
const SENDS_AT_FILES = /^(curl|http|https|xh)$/;

function pathRisk(command: RiskCommand, view: CommandView, facts: RiskFacts): Assessment | undefined {
  const redirected = redirectRisk(command, facts);
  if (redirected?.tier === "danger") return redirected;
  let soft: Assessment | undefined = redirected;
  if (SENDS_AT_FILES.test(view.program)) {
    for (const word of view.args) {
      const file = /^(?:[^=@\s]*=)?@([^;]+)/.exec(word)?.[1];
      if (!file || file === "-") continue;
      const classification = facts.classify(file, command.cwd);
      // A secret leaving the machine: the floor, never approved by anyone but the user.
      if (classification?.zone === "sensitive") return { tier: "danger", kind: `sends a secret off this machine (${file})`, key: "secret-off-machine", floor: true };
    }
  }
  const words = view.args;
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (!pathWords([word]).length) continue;
    // ssh's identity file: the key is used, not read out.
    if (/^(ssh|scp|sftp|rsync|git|mosh)$/.test(view.program) && (words[i - 1] === "-i" || /IdentityFile/i.test(words[i - 1] ?? ""))) continue;
    // `host:path` and a URL are the other machine's files, whatever their names.
    if (!localOperand(view, word, words[i - 1])) continue;
    const classification = facts.classify(word, command.cwd);
    if (!classification) continue;
    const effect = pathEffect(command, view, word);
    if (facts.permissionSystem?.(classification.path) && effect === "write") {
      return { tier: "danger", kind: `changes the permission system itself (${word})`, key: "permission-system", floor: true };
    }
    if (classification.zone === "sensitive" && !NAME_ONLY.has(view.program)) {
      if (effect === "write") return { tier: "danger", kind: `writes a secret (${word})`, key: "writes a secret" };
      // A bare name that is not there holds nothing to read.
      if (isAbsolute(word) || word.startsWith("~") || present(classification.path)) soft ??= { tier: "local", kind: `reads a secret (${word})`, once: true, concern: "secret" };
      continue;
    }
    if (WRITERS.has(view.program) && view.program !== "git" && effect === "write") {
      const why = writeDanger(classification, facts);
      if (why) return { tier: "danger", kind: why, key: dangerKey(why) };
    }
  }
  return soft;
}

/**
 * scp and rsync: copying between two other machines (one could be anyone's),
 * or downloading over a secret, a startup file or Cypher's own data. A
 * copy to or from one machine is local work, like `ssh` to it.
 */
function copyDanger(command: RiskCommand, view: CommandView, facts: RiskFacts): string | undefined {
  if (view.program !== "scp" && view.program !== "rsync") return undefined;
  const copy = copyOperands(view.words);
  if (!copy) return undefined;
  const hosts = [...new Set([...copy.sources, copy.dest].map((word) => remoteOperand(word)?.host).filter((host) => host !== undefined))];
  if (hosts.length > 1) return `copies files between ${hosts.join(" and ")}`;
  if (remoteOperand(copy.dest)) return undefined;
  // A download: where the files land.
  const dest = facts.classify(copy.dest, command.cwd);
  if (!dest) return `copies files to ${copy.dest}, in a directory known only when it runs`;
  const why = writeDanger(dest);
  if (why) return `${why} (${copy.dest})`;
  let directory = copy.dest.endsWith("/") || copy.dest === "." || copy.dest === "..";
  try {
    directory ||= statSync(dest.path).isDirectory();
  } catch {
    // Not there yet: a file.
  }
  if (!directory) return undefined;
  const home = facts.classify("~", undefined)?.path;
  const recursive = view.program === "rsync" || view.args.some((word) => /^-[A-Za-z]*r/.test(word));
  for (const source of copy.sources) {
    const name = basename(source.replace(/\/+$/, "").replace(/^[^/]*:/, "")) || source;
    // `dir/` copies its contents, a glob any names: into the home folder, they could be startup files.
    if (source.endsWith("/") || source.endsWith(":") || /[*?[{]/.test(name)) {
      if (recursive || /[*?[{]/.test(name)) {
        if (home && (dest.path === home || isUnder(home, dest.path))) return "copies files of names known only when it runs into your home folder, where startup files live";
        continue;
      }
    }
    const target = facts.classify(`${dest.path}/${name}`, command.cwd);
    const landed = target && writeDanger(target);
    if (landed) return `${landed} (${dest.path}/${name})`;
  }
  return undefined;
}

/**
 * Transfer tools (data): how each one names a destination off this machine. Copying files there is
 * "send-data" (grades.ts): consent-gated, whatever the files are; the floor (`exfiltrates`) still decides secrets.
 * - `dest`: the last operand is the destination, off this machine when it matches.
 * - `options`: options whose value is a file sent (`curl -T f`, `-F x=@f`, `--data @f`).
 */
interface Transfer {
  programs: readonly string[];
  /** Subcommand words before the operands (`s3 cp`, `copy`). */
  sub?: readonly (readonly string[])[];
  dest?: (word: string) => string | undefined;
  options?: RegExp;
  /** An option's value is a file only when it starts with `@` (`--data @f`); else any value is. */
  at?: RegExp;
}

const SCHEME_DEST = (schemes: string) => (word: string) => new RegExp(`^(?:${schemes})://([^/]+)`, "i").exec(word)?.[1];
export const TRANSFERS: readonly Transfer[] = [
  { programs: ["scp", "rsync", "sftp"], dest: (word) => remoteOperand(word)?.host },
  { programs: ["curl"], options: /^(-T|--upload-file|-F|--form|--form-string|-d|--data|--data-binary|--data-urlencode|--json)$/, at: /^(-d|--data|--data-binary|--data-urlencode|--json)$/ },
  { programs: ["rclone"], sub: [["copy"], ["copyto"], ["sync"], ["move"], ["moveto"], ["bisync"]], dest: (word) => /^([A-Za-z0-9_.-]+):/.exec(word)?.[1] },
  { programs: ["aws"], sub: [["s3", "cp"], ["s3", "sync"], ["s3", "mv"]], dest: SCHEME_DEST("s3") },
  { programs: ["gsutil"], sub: [["cp"], ["rsync"], ["mv"], ["-m", "cp"], ["-m", "rsync"]], dest: SCHEME_DEST("gs") },
  { programs: ["gcloud"], sub: [["storage", "cp"], ["storage", "rsync"], ["storage", "mv"]], dest: SCHEME_DEST("gs") },
  { programs: ["azcopy"], sub: [["copy"], ["sync"]], dest: SCHEME_DEST("https") },
  { programs: ["mc"], sub: [["cp"], ["mirror"], ["mv"]], dest: (word) => /^([A-Za-z0-9_.-]+)\/./.exec(word)?.[1] },
];

/** Copying or uploading files to another machine, by the TRANSFERS table: "sends files to <where>". */
export function uploadDanger(view: CommandView): string | undefined {
  for (const row of TRANSFERS) {
    if (!row.programs.includes(view.program)) continue;
    let args = view.args;
    if (row.sub) {
      const sub = row.sub.find((words) => words.every((word, i) => args[i] === word));
      if (!sub) continue;
      args = args.slice(sub.length);
    }
    if (row.options) {
      for (let i = 0; i < args.length; i++) {
        const word = args[i];
        const [name, inline] = word.startsWith("--") && word.includes("=") ? [word.slice(0, word.indexOf("=")), word.slice(word.indexOf("=") + 1)] : [word, undefined];
        if (!row.options.test(name)) continue;
        const value = inline ?? args[i + 1];
        if (value === undefined) continue;
        const sendsFile = row.at?.test(name) ? value.startsWith("@") || value === UNKNOWN : name.startsWith("-F") || name.startsWith("--form") ? /=[<@]/.test(value) || value === UNKNOWN : true;
        if (sendsFile) return `sends files off this machine (${view.program} ${name} ${value})`;
      }
    }
    if (row.dest) {
      const operands = args.filter((word) => !word.startsWith("-"));
      const last = operands.at(-1);
      if (operands.length < 2 || last === undefined) continue;
      if (last === UNKNOWN) return `sends files to a place known only when it runs (${view.program})`;
      const where = row.dest(last);
      // The sources must be this machine's: between two other places is copyDanger's.
      if (where !== undefined && !operands.slice(0, -1).some((word) => row.dest!(word) !== undefined)) return `sends files to ${where}, another machine`;
    }
  }
  return undefined;
}

function writeDanger(classification: Classification, facts?: RiskFacts): string | undefined {
  // What this task made is its own to change, even where a file there runs
  // code later (its dev instance's Runtime); the installed app and the
  // permission system are never recorded as made (created.ts `recordable`).
  if (facts?.created?.has(classification.path) && classification.zone !== "sensitive") return undefined;
  if (classification.zone === "sensitive") return "writes a secret";
  if (classification.guard === "self") return "changes Cypher's own data";
  if (classification.guard === "protected") return "changes a file that runs code later";
  return undefined;
}

/** Deleting or moving files outside the project folders, scratch space and caches. */
function destroyDanger(command: RiskCommand, view: CommandView, facts: RiskFacts): string | undefined {
  let operands: string[] = [];
  if (DESTROYERS.has(view.program)) operands = view.args.filter((word) => word === UNKNOWN || !word.startsWith("-"));
  else if (view.program === "rsync" && view.args.some((word) => word === UNKNOWN || word.startsWith("--del") || word === "--remove-source-files")) {
    const copy = copyOperands(view.words);
    if (!copy) return "deletes files named only when it runs";
    const deletes = view.args.some((word) => word.startsWith("--del"));
    const remote = remoteOperand(copy.dest);
    if (deletes && remote) return `deletes files on ${remote.host}`;
    // --delete empties the destination of what the source lacks; --remove-source-files the sources.
    operands = [...(deletes ? [copy.dest] : []), ...(view.args.includes("--remove-source-files") ? copy.sources : [])].filter((word) => !remoteOperand(word));
  }
  else if (view.program === "find" && view.args.some((word) => word === UNKNOWN || word === "-delete")) {
    // `-delete`, or a word known only when it runs that could be it. What
    // `-exec` runs is assessed as its own command (`assessCommand`).
    operands = findStarts(view);
  } else if (/^(chmod|chown|chgrp)$/.test(view.program) && view.args.some((word) => /^-[A-Za-z]*R/.test(word))) {
    // A recursive mode change outside the project folders; one path at a
    // time, or a path known only when it runs, is ordinary work (`mkdir -p
    // "$E" && chmod 700 "$E"`).
    operands = view.positionals.slice(1).filter((word) => word !== UNKNOWN);
  } else return undefined;
  for (const operand of operands) {
    if (operand === UNKNOWN) return `${verb(view.program)} files named only when it runs`;
    if (command.cwd === undefined && !isAbsolute(operand) && !operand.startsWith("~")) return `${verb(view.program)} files in a directory known only when it runs`;
    const classification = facts.classify(operand, command.cwd);
    if (!classification || !disposable(classification.path, facts)) return `${verb(view.program)} files outside the project folders (${operand})`;
  }
  return undefined;
}

function verb(program: string): string {
  if (program === "mv") return "moves";
  if (/^(chmod|chown|chgrp)$/.test(program)) return "changes permissions of";
  return "deletes";
}

/**
 * What `kill`, `pkill` or `killall` would stop; `harmless` for signal 0
 * (a liveness check) and `kill -l`; undefined when not known before it
 * runs or given options this does not read.
 */
export function killTarget(view: CommandView): KillTarget | "harmless" | undefined {
  const args = view.args;
  if (args.includes(UNKNOWN)) return undefined;
  let signalZero = false;
  if (view.program === "kill") {
    const pids: number[] = [];
    for (let i = 0; i < args.length; i++) {
      const word = args[i];
      if (word === "-s" || word === "-n") {
        signalZero ||= args[i + 1] === "0";
        i++;
      } else if (word === "-l" || word === "-L") return "harmless";
      else if (/^-\d+$/.test(word)) signalZero ||= word === "-0";
      else if (/^-(SIG)?[A-Z]+[0-9]*$/i.test(word)) continue;
      else if (/^\d+$/.test(word)) pids.push(Number(word));
      else return undefined;
    }
    if (pids.length === 0) return undefined;
    return signalZero ? "harmless" : { pids };
  }
  const names: string[] = [];
  const flags: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (word === "-s" || word === "--signal") {
      signalZero ||= args[i + 1] === "0";
      i++;
    } else if (/^-\d+$/.test(word)) signalZero ||= word === "-0";
    else if (view.program === "pkill" && /^-[fxil]+$/.test(word)) flags.push(word.replace(/l/g, ""));
    else if (/^-(SIG)?[A-Z]+[0-9]*$/.test(word)) continue;
    else if (word.startsWith("-")) return undefined;
    else names.push(word);
  }
  if (names.length === 0) return undefined;
  if (signalZero) return "harmless";
  if (view.program === "killall") return { pgrep: ["-x", names.map(escapeRegex).join("|")] };
  if (names.length !== 1) return undefined;
  return { pgrep: [...flags.filter((flag) => flag !== "-"), names[0]] };
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function killAssessment(view: CommandView, facts: RiskFacts): Assessment | undefined {
  if (!/^(kill|pkill|killall)$/.test(view.program)) return undefined;
  const target = killTarget(view);
  if (target === "harmless") return { tier: "inspect", kind: "checks whether processes are running", outside: true };
  if (target === undefined) return { tier: "local", kind: "stops processes known only when it runs" };
  const known = facts.processFacts?.(target) ?? { owned: facts.ownsProcesses(target) };
  // Its own, even when they run the installed Runtime's node (the agent's PATH finds it first).
  if (known.owned === true) return { tier: "inspect", kind: "stops processes this chat started", outside: true };
  if (known.installedApp) return { tier: "danger", kind: "stops the installed Cypher app's processes" };
  // Started in the chat's folders (its dev instance, a test server), perhaps through `env -i` or before an engine restart: the model decides.
  if (known.inRoots === true) return { tier: "local", kind: "stops processes running in this chat's project folders" };
  // Running in, or on, what this task made (its dev instance's data directory).
  if (known.inCreated === true) return { tier: "local", kind: "stops processes running in what this task made" };
  if (known.owned === false) return { tier: "danger", kind: "stops processes this chat did not start" };
  // macOS hides the environment of its own binaries; nothing matched a name.
  return { tier: "danger", kind: "stops processes it cannot tell are this chat's" };
}

/** A path into `~/.cypher` or the app bundle written inside a word or a script's text (`python3 -c "open('~/.cypher/…')"`). */
const INSTALLED_MENTION = /(?:~|\$HOME|\$\{HOME\})\/\.cypher(?![\w.-])(?:\/[^\s'"`;,|&<>()]*)?|\/Applications\/Cypher\.app(?![\w.-])/g;

/** The first path in `text` that is the installed app or its data (not a worktree, not the chat's own folder). */
function installedMention(text: string, facts: RiskFacts, home: string | undefined): string | undefined {
  if (!facts.installedApp || home === undefined) return undefined;
  for (const match of text.split(home).join("~").matchAll(INSTALLED_MENTION)) {
    const mention = match[0];
    if (mention.startsWith("/Applications/")) return mention;
    const classification = facts.classify(`${home}${mention.replace(/^(~|\$HOME|\$\{HOME\})/, "")}`, undefined);
    if (classification && facts.installedApp(classification.path)) return mention;
  }
  return undefined;
}

/** The installed app's and Runtime's program files (not the owner's data): `/Applications/Cypher.app`, a Runtime version outside its `agent/` folder. */
const INSTALLED_PROGRAM = /^\/Applications\/Cypher\.app(\/|$)|\/\.cypher\/pi-runtime\/(current|versions\/[^/]+)(\/(?!agent(\/|$))|$)/;

/**
 * Touching the installed Cypher app (`/Applications/Cypher.app`, its data in
 * `~/.cypher` outside the chats' worktrees, its Runtime): a human decides,
 * whatever the user said. A chat's own dev instance lives elsewhere
 * (`~/.cypher-development`).
 */
function installedAppRisk(command: RiskCommand, view: CommandView, facts: RiskFacts): Assessment | undefined {
  const installed = facts.installedApp;
  if (!installed) return undefined;
  const home = facts.classify("~", undefined)?.path;
  const changes = (path: string): Assessment => ({ tier: "danger", kind: `changes the installed Cypher app or its data (${path})`, key: "installed Cypher app" });
  // Reading the installed app's logs and data is how a report about it gets
  // diagnosed: ordinary work the model decides on.
  // The owner's data (chats, journals, databases) is a concern the model must find asked for; the app's and Runtime's program files are not.
  const reads = (path: string): Assessment => ({ tier: "local", kind: `reads the installed Cypher app's data (${path})`, ...(INSTALLED_PROGRAM.test(path) ? {} : { concern: "installed-app" as const, data: path }) });
  let soft: Assessment | undefined;
  if (command.cwd !== undefined) {
    const cwd = facts.classify(command.cwd, undefined);
    // Running in its folder: whatever it does, it does there.
    if (cwd && installed(cwd.path)) {
      if (pathEffect(command, view, command.cwd) === "write") return changes(command.cwd);
      soft ??= reads(command.cwd);
    }
  }
  // Running one of the installed app's programs only to ask it for its help or version reads it.
  const asking = asksOnly(view);
  for (const [index, word] of [...command.words, ...command.reads, ...command.writes, ...(command.carried ?? [])].entries()) {
    if (word === UNKNOWN) continue;
    if (pathWords([word]).length || command.reads.includes(word) || command.writes.includes(word)) {
      const classification = facts.classify(word, command.cwd);
      if (classification && installed(classification.path)) {
        if (!(index === 0 && asking) && pathEffect(command, view, word) === "write") return changes(word);
        soft ??= reads(word);
      }
    }
    // Inside a script or an option's value: its own text says whether it writes.
    const inside = installedMention(word, facts, home);
    if (inside) {
      if (WRITE_HINT.test(word) || (!(index === 0 && asking) && pathEffect(command, view, inside) === "write")) return changes(inside);
      soft ??= reads(inside);
    }
  }
  return soft;
}

/** `git push` publishes: to a default or protected branch, tags, force, deletes. */
function gitPushDanger(command: RiskCommand, view: CommandView, facts: RiskFacts): string | undefined {
  if (view.positionals[0] !== "push") return undefined;
  const at = view.args.indexOf("push");
  const rest = view.args.slice(at + 1);
  if (rest.includes(UNKNOWN)) return "pushes to a branch known only when it runs";
  if (rest.some((word) => /^(-f|--force|--force-with-lease.*|--force-if-includes|-d|--delete|--mirror|--prune)$/.test(word) || /^-[A-Za-z]*f[A-Za-z]*$/.test(word))) return "force-pushes or deletes a remote branch";
  if (rest.some((word) => /^(--tags|--follow-tags|--all)$/.test(word))) return "pushes tags (publishes a release)";
  const dir = gitDirectory(command, view);
  const git = dir === undefined ? undefined : facts.git(dir);
  const positionals = rest.filter((word) => !word.startsWith("-"));
  const refspecs = positionals.slice(1);
  if (refspecs.length === 0) {
    if (!git || !git.current) return "pushes a branch known only when it runs";
    return isDefault(git.current, git, facts) ? `publishes to ${git.current}` : undefined;
  }
  for (const spelled of refspecs) {
    if (spelled.startsWith("+")) return "force-pushes a branch";
    // One spelling for one push: `refs/heads/x:refs/heads/x`, `x:x` and `HEAD:x` (on x) are `x`.
    const refspec = normalRefspec(spelled, git?.current);
    const [source, target = source] = refspec.split(":");
    if (source === "") return "deletes a remote branch";
    const name = target.replace(/^refs\/heads\//, "");
    if (target.startsWith("refs/tags/") || (source.startsWith("refs/tags/") && !refspec.includes(":"))) return "pushes a tag (publishes a release)";
    const branch = name === "HEAD" ? git?.current : name;
    if (!branch || !git) return "pushes to a branch it cannot tell";
    if (!refspec.includes(":") && !git.isBranch(branch) && name !== "HEAD") return `pushes ${branch}, which is no local branch (a tag publishes a release)`;
    if (isDefault(branch, git, facts)) return `publishes to ${branch}`;
    // `src:dest` writes the remote's `dest`: what is checked is the DESTINATION ref, which must be this task's own branch
    // (a source of its own says nothing about whose branch it lands on).
    if (refspec.includes(":") && !ownBranch(branch, git, facts)) return `pushes into ${branch}, a branch this task didn't make`;
  }
  return undefined;
}

/** A refspec as its shortest spelling: `refs/heads/` dropped, `x:x` and `HEAD:x` (x checked out) as `x`. */
export function normalRefspec(refspec: string, current: string | undefined): string {
  const heads = (ref: string) => ref.replace(/^refs\/heads\//, "");
  if (!refspec.includes(":")) return refspec.startsWith("refs/tags/") ? refspec : heads(refspec);
  const [source, target] = refspec.split(":", 2);
  if (source === "" || source.startsWith("refs/tags/") || target.startsWith("refs/tags/")) {
    const tag = (ref: string) => ref.replace(/^refs\/tags\//, "");
    return source !== "" && tag(source) === tag(target) ? `refs/tags/${tag(target)}` : refspec;
  }
  const from = heads(source) === "HEAD" && current ? current : heads(source);
  const to = heads(target);
  return from === to ? to : `${from}:${to}`;
}

/**
 * A branch this task made, as code-checked facts say: the ledger's (created.ts) or the chat's own
 * workspace branch (Cypher gives a chat a worktree on its own branch). Never a default or protected branch.
 */
function ownBranch(branch: string | undefined, git: GitFacts, facts: RiskFacts): boolean {
  if (!branch || branch === UNKNOWN || branch === "HEAD" || isDefault(branch, git, facts)) return false;
  return facts.created?.branch(branch) === true || (facts.workspaceBranches?.() ?? []).includes(branch);
}

/** `git push` options that change nothing about what is published or how. */
const PLAIN_PUSH = /^(-u|--set-upstream|-q|--quiet|-v|--verbose|--porcelain|--progress|--no-progress|--no-verify|--verify)$/;

/**
 * The hosting CLIs' pull/merge request commands a team step may be (data): the subcommands that open
 * and update one, the options naming its head, base and repository, the options that take a value,
 * and the options that only say what it holds (title, body, draft, labels, reviewers). Any other
 * option makes the command no team step.
 */
interface ReviewCli {
  program: string;
  noun: string;
  open: string;
  update: readonly string[];
  head: readonly string[];
  base: readonly string[];
  repo: readonly string[];
  values: RegExp;
  plain: RegExp;
}

const REVIEW_CLIS: readonly ReviewCli[] = [
  {
    program: "gh",
    noun: "pr",
    open: "create",
    update: ["edit", "ready"],
    head: ["-H", "--head"],
    base: ["-B", "--base"],
    repo: ["-R", "--repo"],
    values: /^(-t|--title|-b|--body|-F|--body-file|-B|--base|-H|--head|-R|--repo|-a|--assignee|-l|--label|-m|--milestone|-r|--reviewer|-p|--project|-T|--template|--add-label|--remove-label|--add-reviewer|--remove-reviewer|--add-assignee|--remove-assignee|--add-project|--remove-project)$/,
    plain: /^(-t|--title|-b|--body|-F|--body-file|-d|--draft|-f|--fill|--fill-first|--fill-verbose|-l|--label|-a|--assignee|-r|--reviewer|-m|--milestone|-p|--project|-T|--template|--no-maintainer-edit|--dry-run|--add-label|--remove-label|--add-reviewer|--remove-reviewer|--add-assignee|--remove-assignee|--add-project|--remove-project|--undo)$/,
  },
  {
    program: "glab",
    noun: "mr",
    open: "create",
    update: ["update"],
    head: ["-s", "--source-branch"],
    base: ["-b", "--target-branch"],
    repo: ["-R", "--repo"],
    values: /^(-t|--title|-d|--description|-s|--source-branch|-b|--target-branch|-R|--repo|-l|--label|-a|--assignee|--reviewer|-m|--milestone|--unlabel)$/,
    plain: /^(-t|--title|-d|--description|--draft|--wip|-f|--fill|--fill-commit-body|-l|--label|-a|--assignee|--reviewer|-m|--milestone|--unlabel|-y|--yes|--ready|--no-editor)$/,
  },
];

/** A review command's options and operands: values by option, the rest in order. Undefined when an option isn't one a team step may carry, or a name is known only when it runs. */
function reviewWords(cli: ReviewCli, args: readonly string[]): { options: Map<string, string>; operands: string[] } | undefined {
  const named = [...cli.head, ...cli.base, ...cli.repo];
  const allowed = (option: string) => cli.plain.test(option) || named.includes(option);
  const options = new Map<string, string>();
  const operands: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (word.startsWith("--") && word.includes("=")) {
      const name = word.slice(0, word.indexOf("="));
      if (!allowed(name)) return undefined;
      options.set(name, word.slice(word.indexOf("=") + 1));
    } else if (word.startsWith("-") && word !== "-") {
      if (!allowed(word)) return undefined;
      if (cli.values.test(word)) {
        const value = args[++i];
        if (value === undefined) return undefined;
        options.set(word, value);
      } else options.set(word, "");
    } else operands.push(word);
  }
  if (named.some((option) => options.get(option) === UNKNOWN) || operands.includes(UNKNOWN)) return undefined;
  return { options, operands };
}

/** The value of the first of `names` given. */
function optionOf(options: Map<string, string>, names: readonly string[]): string | undefined {
  for (const name of names) if (options.has(name)) return options.get(name);
  return undefined;
}

/** git options before the subcommand that change nothing about what a push publishes. */
const PLAIN_GIT_GLOBAL = /^(--no-pager|-P|--no-optional-locks|--paginate|-p|--no-replace-objects|--literal-pathspecs)$/;

/**
 * A reversible, team-visible step on what this task made (grades.ts rows the judge decides):
 * pushing its own branch, opening a pull request from it into the default branch, editing or
 * marking ready a pull request it opened. Its grade's action, with what it says; undefined for
 * anything else (a default branch, another remote's or repository's, a force, a tag, a name
 * known only when it runs), which stays a danger.
 */
export function teamStep(command: RiskCommand, view: CommandView, facts: RiskFacts): { action: string; kind: string } | undefined {
  if (command.words.includes(UNKNOWN)) return undefined;
  const graded = (action: string, what: string) => (judgeDecides(action) ? { action, kind: `${what} (reversible, seen by the team: ${gradeOf(action)!.says})` } : undefined);
  if (view.program === "git" && view.positionals[0] === "push") {
    const dir = gitDirectory(command, view);
    const git = dir === undefined ? undefined : facts.git(dir);
    if (!git) return undefined;
    // Global options before `push`: `-C dir` (read by gitDirectory) and the ones that only change output.
    const at = view.args.indexOf("push");
    const before = view.args.slice(0, at);
    if (before.some((word, index) => word !== "-C" && before[index - 1] !== "-C" && !PLAIN_GIT_GLOBAL.test(word))) return undefined;
    const rest = view.args.slice(at + 1);
    if (rest.some((word) => word.startsWith("-") && !PLAIN_PUSH.test(word))) return undefined;
    const [remote, ...refspecs] = rest.filter((word) => !word.startsWith("-"));
    // A remote by name (`origin`), never a URL or a path.
    if (remote !== undefined && !/^[\w.-]+$/.test(remote)) return undefined;
    const branches = refspecs.length === 0 ? [git.current] : refspecs.map((spelled) => {
      if (spelled.startsWith("+") || spelled.includes("refs/tags/")) return undefined;
      const refspec = normalRefspec(spelled, git.current);
      if (refspec.includes(":")) return undefined;
      return refspec === "HEAD" ? git.current : refspec;
    });
    if (branches.length === 0 || !branches.every((branch) => ownBranch(branch, git, facts))) return undefined;
    return graded("push-branch", `pushes ${branches.join(", ")}, this task's own branch`);
  }
  const cli = REVIEW_CLIS.find((candidate) => candidate.program === view.program && view.positionals[0] === candidate.noun);
  if (cli) {
    const sub = view.positionals[1];
    const at = view.args.indexOf(sub ?? "");
    if (at < 0 || view.args.slice(0, at).some((word) => word !== cli.noun)) return undefined;
    const read = reviewWords(cli, view.args.slice(at + 1));
    if (!read || optionOf(read.options, cli.repo) !== undefined) return undefined;
    const git = command.cwd === undefined ? undefined : facts.git(command.cwd);
    if (!git) return undefined;
    const head = optionOf(read.options, cli.head);
    const base = optionOf(read.options, cli.base);
    if (sub === cli.open) {
      if (read.operands.length) return undefined;
      const from = head ?? git.current;
      // Into the default branch (or the repository's default when none is named), from this task's own branch.
      if (base !== undefined && !git.defaults.includes(base)) return undefined;
      if (from?.includes(":") || !ownBranch(from, git, facts)) return undefined;
      return graded("pr-open", `opens a pull request from ${from}, this task's own branch, into ${base ?? "the default branch"}`);
    }
    if (sub !== undefined && cli.update.includes(sub)) {
      if (head !== undefined || base !== undefined || read.operands.length > 1) return undefined;
      const [operand] = read.operands;
      if (operand === undefined) return ownBranch(git.current, git, facts) ? graded("pr-update", `changes the pull request of ${git.current}, this task's own branch`) : undefined;
      const number = operand.replace(/^[#!]/, "");
      if (/^\d+$/.test(number) && facts.created?.pullRequest?.(number) === true) return graded("pr-update", `changes pull request #${number}, which this task opened`);
      if (ownBranch(operand, git, facts)) return graded("pr-update", `changes the pull request of ${operand}, this task's own branch`);
      return undefined;
    }
  }
  return undefined;
}

function isDefault(branch: string, git: GitFacts, facts: RiskFacts): boolean {
  return git.defaults.includes(branch) || facts.protectedTargets.some((glob) => globMatches(glob, branch));
}

/** The directory git works in: `-C dir` relative to where it runs. */
function gitDirectory(command: RiskCommand, view: CommandView): string | undefined {
  let dir = command.cwd;
  for (let i = 0; i < view.args.length; i++) {
    const word = view.args[i];
    if (word === "-C") {
      const next = view.args[i + 1];
      if (next === undefined || next === UNKNOWN) return undefined;
      if (dir === undefined && !isAbsolute(next)) return undefined;
      dir = isAbsolute(next) ? next : `${dir}/${next}`;
      i++;
    } else if (!word.startsWith("-")) break;
  }
  return dir;
}

/**
 * Configuration keys that only tune what git prints or how it names things:
 * setting them runs nothing. Any other key may name a program git runs
 * (`core.fsmonitor`, `core.hooksPath`, `core.sshCommand`, `alias.*`,
 * filters, credential helpers) or where it fetches from.
 */
const HARMLESS_GIT_KEYS =
  /^(user\.(name|email|signingkey)|color\..*|core\.(autocrlf|quotepath|ignorecase|filemode|eol|safecrlf|precomposeunicode|longpaths|compression|abbrev|whitespace)|init\.defaultbranch|pull\.(rebase|ff)|push\.(default|autosetupremote|followtags)|fetch\.(prune|prunetags|parallel)|advice\..*|merge\.(conflictstyle|ff|log)|rerere\.(enabled|autoupdate)|log\..*|diff\.(colormoved|algorithm|noprefix|renames|mnemonicprefix|relative|context|interhunkcontext)|status\..*|commit\.(verbose|cleanup|status)|branch\.[^.]+\.(remote|merge|rebase)|branch\.(autosetupmerge|autosetuprebase|sort)|tag\.sort|column\..*|gc\.auto|maintenance\.auto|help\.autocorrect|blame\..*|grep\..*|format\.(pretty|subjectprefix)|i18n\..*|protocol\.version|submodule\.recurse|checkout\.defaultremote)$/i;

/**
 * Options that make git run a program, or read a repository the line
 * names: the global ones before the subcommand, and `clone -c`/`--config`.
 * (`git commit -c <commit>` reuses a message: not configuration.)
 */
function gitOptionDanger(view: CommandView, hints?: readonly (string | undefined)[]): string | undefined {
  const args = view.args;
  let sub: string | undefined;
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (sub === undefined && !word.startsWith("-")) {
      sub = word;
      continue;
    }
    const global = sub === undefined;
    if (global && (word === "--config-env" || word.startsWith("--config-env=") || word === "--exec-path" || word.startsWith("--exec-path="))) return "makes git run a program the line names";
    if (global && (word === "--git-dir" || word.startsWith("--git-dir="))) return "points git at a repository whose config could run a program";
    if (global && word === "-C") {
      i++;
      continue;
    }
    if (!global && sub !== "clone" && sub !== UNKNOWN) continue;
    let setting: string | undefined;
    if (word === "-c" || (!global && word === "--config")) setting = args[++i];
    else if (!global && word.startsWith("--config=")) setting = word.slice("--config=".length);
    else if (global && /^-c.+=/.test(word)) setting = word.slice(2);
    if (setting === undefined) continue;
    // `-c user.name="$(…)"`: the key is as written, only its value is known when it runs.
    if (setting === UNKNOWN && args[i] === UNKNOWN) setting = /^[\w.-]+=/.exec(hints?.[i + 1] ?? "")?.[0] ?? UNKNOWN;
    if (setting === UNKNOWN) return "sets git configuration known only when it runs";
    const key = setting.split("=")[0];
    if (!HARMLESS_GIT_KEYS.test(key)) return `sets ${key}, which can make git run a program`;
  }
  return undefined;
}

/** `git config` writing a key that can run a program later. */
function gitConfigDanger(view: CommandView): string | undefined {
  if (view.positionals[0] !== "config") return undefined;
  const rest = view.positionals.slice(1);
  const reads = view.args.some((word) => /^(--get.*|--list|-l|--show-.*)$/.test(word)) || /^(get|list)$/.test(rest[0] ?? "");
  const verb = /^(set|unset|rename-section|remove-section|edit)$/.test(rest[0] ?? "") ? rest.shift() : undefined;
  const edits = verb !== undefined || view.args.some((word) => /^(--unset|--unset-all|--add|--replace-all|--rename-section|--remove-section|--edit|-e)$/.test(word)) || (!reads && rest.length >= 2);
  if (!edits) return undefined;
  if (verb === "edit" || view.args.some((word) => /^(--edit|-e|--rename-section|--remove-section)$/.test(word)) || /section$/.test(verb ?? "")) return "changes git's configuration, which can run code later";
  const key = rest[0];
  if (key === undefined || key === UNKNOWN || !HARMLESS_GIT_KEYS.test(key)) return "changes git's configuration, which can run code later";
  return undefined;
}

/** Throwing away history git could otherwise bring back. */
function gitHistoryDanger(view: CommandView): string | undefined {
  const [sub, next] = view.positionals;
  if (sub === "reflog" && /^(expire|delete)$/.test(next ?? "")) return "deletes git history the reflog could bring back";
  if (sub === "gc" && view.args.some((word) => /^--prune(=now|=all)?$/.test(word) || word === "--aggressive")) return "deletes git history the reflog could bring back";
  if (sub === "prune" || (sub === "filter-branch") || (sub === "filter-repo")) return "deletes or rewrites git history";
  return undefined;
}

/** Discarding work in a checkout that is not the chat's. */
function gitDiscardDanger(command: RiskCommand, view: CommandView, facts: RiskFacts): string | undefined {
  const [sub] = view.positionals;
  if (sub === "worktree" && view.positionals[1] === "remove") {
    // The worktree it removes, not where it runs.
    const target = view.positionals[2];
    const dir = gitDirectory(command, view);
    if (target === undefined || target === UNKNOWN) return "removes a worktree known only when it runs";
    const classification = facts.classify(target, dir);
    if (!classification) return `removes a worktree outside this chat's workspace (${target})`;
    // Its own workspace, or a worktree this task added itself.
    if (facts.inWorkspace(classification.path) || facts.created?.worktree(classification.path) || facts.created?.has(classification.path)) return undefined;
    return `removes a worktree outside this chat's workspace (${target})`;
  }
  if (sub === "branch" && view.args.some((word) => /^-[A-Za-z]*D$/.test(word) || word === "--delete" || word === "-d")) {
    // Deleting branches this task created undoes its own work.
    const names = view.positionals.slice(1).filter((word) => word !== UNKNOWN);
    const own = (name: string) => facts.created?.branch(name) === true || facts.workspaceBranches?.().includes(name) === true;
    if (names.length && names.length === view.positionals.length - 1 && names.every(own)) return undefined;
    // Anyone else's branch, in any checkout: its owner decides.
    const others = view.positionals.slice(1).filter((name) => name === UNKNOWN || !own(name));
    return others.includes(UNKNOWN) ? "deletes a branch known only when it runs" : `deletes ${others.join(", ")}, a branch this chat didn't create`;
  }
  const discards =
    (sub === "reset" && view.args.some((word) => word === "--hard" || word === "--merge" || word === "--keep")) ||
    (sub === "clean" && view.args.some((word) => /^-[A-Za-z]*f/.test(word) || word === "--force")) ||
    (sub === "checkout" && (view.args.includes("--") || view.args.includes(".") || view.args.some((word) => /^-[A-Za-z]*f/.test(word)))) ||
    (sub === "restore" && !view.args.includes("--staged")) ||
    (sub === "switch" && view.args.some((word) => word === "--discard-changes" || word === "-f" || word === "--force")) ||
    (sub === "branch" && view.args.some((word) => /^-[A-Za-z]*D/.test(word)));
  if (!discards) return undefined;
  const dir = gitDirectory(command, view);
  if (dir === undefined) return "discards work in a checkout known only when it runs";
  if (facts.inWorkspace(dir)) return undefined;
  // A checkout this task made (a repro worktree, a clone in /tmp) is its own work.
  const classification = facts.classify(dir, undefined);
  if (classification && (facts.created?.has(classification.path) || facts.created?.worktree(classification.path))) return undefined;
  return "discards work in a checkout outside this chat's workspace";
}

const SHELLS = /^(sh|bash|zsh|dash|ksh|fish)$/;

/**
 * A command line inside a word: what `bash -c '…'`, `eval '…'` or `ssh host
 * '…'` runs. Read as an opaque line: dangerous if anything in it could be.
 */
function innerLine(view: CommandView): { line: string; where?: string } | undefined {
  if (SHELLS.test(view.program)) {
    const at = view.args.findIndex((word) => /^-[a-z]*c[a-z]*$/.test(word));
    const line = at >= 0 ? view.args[at + 1] : undefined;
    return line === undefined ? undefined : { line };
  }
  if (view.program === "eval" || view.program === "watch") return { line: view.positionals.join(" ") };
  if (view.program === "ssh") {
    const host = sshHostIndex(view.words);
    if (host === undefined) return undefined;
    const remote = view.words.slice(host + 1);
    return remote.length ? { line: remote.join(" "), where: view.words[host] } : undefined;
  }
  if (view.program === "script") {
    const at = view.args.indexOf("-c");
    if (at >= 0 && view.args[at + 1] !== undefined) return { line: view.args[at + 1] };
  }
  if (view.program === "env") {
    const at = view.args.findIndex((word) => word === "-S" || word.startsWith("--split-string"));
    if (at >= 0) return { line: view.args[at].includes("=") ? view.args[at].split("=").slice(1).join("=") : (view.args[at + 1] ?? UNKNOWN) };
  }
  return undefined;
}

const XARGS_VALUES = new Set(["-I", "-J", "-L", "-n", "-P", "-s", "-E", "-d", "-a", "-R", "-S", "--max-args", "--max-procs", "--delimiter", "--arg-file", "--eof", "--replace", "--max-lines", "--max-chars"]);

/**
 * The command a runner runs with the rest of its words: `exec npm
 * publish`, `command npm publish`, `xargs rm -rf` (with words known only
 * when it runs), `env`, `nohup` or `timeout` with options `unwrap` does not
 * read, `script file cmd`. Undefined when it runs nothing (`command -v`).
 */
function runsNext(view: CommandView): string[] | undefined {
  const args = view.args;
  const after = (start: number) => {
    const rest = args.slice(start);
    return rest.length ? rest : undefined;
  };
  switch (view.program) {
    case "exec": {
      let i = 0;
      while (i < args.length && /^-[cl]+$|^-a$/.test(args[i])) i += args[i] === "-a" ? 2 : 1;
      return after(i);
    }
    case "command":
    case "builtin": {
      if (args.some((word) => /^-[A-Za-z]*[vV]/.test(word))) return undefined;
      let i = 0;
      while (i < args.length && /^-p$/.test(args[i])) i++;
      return after(i);
    }
    case "xargs": {
      let i = 0;
      for (; i < args.length && args[i].startsWith("-"); i++) {
        if (XARGS_VALUES.has(args[i])) i++;
      }
      const rest = after(i);
      // Its input adds words.
      return rest ? [...rest, UNKNOWN] : undefined;
    }
    case "script": {
      let i = 0;
      for (; i < args.length && args[i].startsWith("-"); i++) if (/^-[Ft]$/.test(args[i])) i++;
      // `script [file [command …]]`
      return after(i + 1);
    }
    case "nohup":
    case "time":
    case "nice":
    case "timeout":
    case "caffeinate":
    case "env": {
      const unwrapped = unwrap(view.words);
      if (unwrapped) return unwrapped.words;
      // Options `unwrap` does not read: the first word that is no option, value or duration.
      const at = args.findIndex((word, i) => !word.startsWith("-") && !/^\d+(\.\d+)?[smhd]?$/.test(word) && !/^[A-Za-z_]\w*=/.test(word) && !/^-[nsktw]$/.test(args[i - 1] ?? ""));
      return at >= 0 ? after(at) : undefined;
    }
  }
  return undefined;
}

/** The tier of one command seen through its wrappers. */
/**
 * The tier of one command seen through its wrappers, held to provenance: a
 * command the catalogue reads as destroying or stopping something (a
 * simulator, a container, a repository, a release, …) that this chat's ledger
 * doesn't hold, and whose kind no place tells the owner of (catalogue.ts
 * `TOLD_BY_PLACE`), may be another party's: a danger, whatever else says.
 */
export function assessCommand(command: RiskCommand, facts: RiskFacts): Assessment {
  // Code read from its input, known only when it runs: never local work.
  if (command.stdinCode) return { tier: "danger", kind: "runs code known only when it runs (from its input)", key: "stdin-code" };
  const assessment = assessWords(command, facts);
  if (assessment.tier === "danger" || command.words.length === 0 || command.words[0] === UNKNOWN) return assessment;
  const undo = facts.created?.destroys?.(command);
  if (!undo) return genericDestroy(command, assessment, facts);
  if (undo.own) return assessment;
  const foreign = undo.kinds.filter((kind) => !(kind in TOLD_BY_PLACE));
  if (foreign.length === 0) return assessment;
  const kind = `${undo.op === "stop" ? "stops" : "deletes"} ${foreign.map((item) => (item === "repo" ? "a repository" : item === "pr" ? "a pull request" : `a ${item}`)).join(" or ")} this chat didn't create or start`;
  return { tier: "danger", kind, key: `foreign-${foreign[0]}` };
}

/**
 * A program no catalogue row reads, given a destroy verb as its subcommand
 * (catalogue.ts `genericDestroyer`): what it destroys has an owner Cypher
 * can't tell, unless every operand is in the ledger or a path in the chat's
 * own folders. Inspections, interpreters and the project's own scripts stay
 * as they are.
 */
function genericDestroy(command: RiskCommand, assessment: Assessment, facts: RiskFacts): Assessment {
  if (assessment.tier === "inspect") return assessment;
  const program = basename(command.words[0]);
  if (INTERPRETERS.test(program) || SHELLS.test(program) || facts.programIsLocal?.(command.words)) return assessment;
  // The project's own script (`./scripts/dev.sh stop`): its effect is the project's, for the model.
  if (command.words[0].includes("/")) {
    const script = facts.classify(command.words[0], command.cwd);
    if (script && disposable(script.path, facts)) return assessment;
  }
  const found = genericDestroyer(command.words);
  if (!found) return assessment;
  const own = (word: string) => {
    if (word === UNKNOWN) return false;
    if (facts.created?.named?.(word)) return true;
    if (!word.includes("/") && !word.startsWith(".")) return false;
    const classification = facts.classify(word, command.cwd);
    return classification !== undefined && disposable(classification.path, facts);
  };
  if (found.operands.length && found.operands.every(own)) return assessment;
  const said = command.words.slice(0, command.words.indexOf(found.verb) + 1).map((word) => (word === command.words[0] ? program : word)).join(" ");
  return { tier: "danger", kind: `destroys or stops something whose owner Cypher can't tell (\`${said}\`)`, key: `destroys-${program}` };
}

function assessWords(command: RiskCommand, facts: RiskFacts): Assessment {
  const words = command.words;
  if (words.length === 0) return { tier: "inspect" };
  const view = viewOf(words);
  if (words[0] === UNKNOWN) return { tier: "local", kind: "runs a program known only when it runs" };
  // Undoing what this task made or started, or what the lifecycle of what it did implies (a merged pull
  // request's head branch): its own work, for the reviewing model to decide (created.ts, lifecycle.ts).
  if (command.writes.length === 0) {
    const undo = facts.created?.destroys?.(command);
    if (undo?.own) return { tier: "local", kind: `${undo.op === "stop" ? "stops" : "undoes"} what this task did (${undo.why.join("; ")})` };
  }
  const inner = innerLine(view);
  if (inner) {
    // What ssh runs is the other machine's: its variables and files are not this one's secrets.
    const assessment: Assessment = inner.line.includes(UNKNOWN) ? { tier: "danger", kind: "runs a command known only when it runs" } : assessOpaque(inner.line, facts, inner.where !== undefined);
    if (assessment.tier === "danger") {
      const kind = inner.where ? `on ${inner.where}: ${assessment.kind}` : assessment.kind!;
      return assessment.floor ? { ...assessment, kind } : { tier: "danger", kind, key: kind };
    }
  }
  // Options that run a program on this computer (`ssh -o ProxyCommand=…`, `-F file`, `rsync -e`).
  // A word known only when it runs could be one too: no rule covers that command (core.ts), but it is no danger in itself.
  const vouch = ruleCannotVouch(words.filter((word) => word !== UNKNOWN));
  if (vouch) return { tier: "danger", kind: `gives ${view.program} an option that can run a program or write a file on this computer`, key: "ssh-options" };
  const next = runsNext(view);
  if (next) {
    const assessment = assessCommand({ ...command, words: next, reads: [], writes: [], hints: undefined }, facts);
    if (assessment.tier === "danger") return assessment;
    const redirected = redirectRisk(command, facts);
    if (redirected?.tier === "danger") return redirected;
    return { tier: "local", kind: assessment.kind ?? redirected?.kind ?? `runs ${basename(next[0] ?? "a program")} through ${view.program}`, ...(assessment.once || redirected?.once ? { once: true as const } : {}), ...(assessment.concern || redirected?.concern ? { concern: assessment.concern ?? redirected?.concern } : {}), ...(assessment.data ? { data: assessment.data } : {}) };
  }
  // What `find -exec` runs is a command of its own, on the files `find`
  // starts from (`find build -exec rm {} +` deletes in `build`).
  let ran: Assessment | undefined;
  for (const run of findRuns(view) ?? []) {
    const starts = run.some((word) => word.includes("{}")) ? findStarts(view) : [""];
    for (const start of starts) {
      const assessment = assessCommand({ ...command, words: run.map((word) => word.split("{}").join(start)), reads: [], writes: [], hints: undefined }, facts);
      if (assessment.tier === "danger") return assessment;
      if (assessment.once) ran ??= assessment;
    }
  }
  const kill = killAssessment(view, facts);
  if (kill?.tier === "danger") return kill;
  // The checks that may also come out local (reading a secret, reading the
  // installed app's data): a danger wins, a local one stands when nothing
  // else is dangerous.
  const system = permissionSystemRisk(command, view, facts);
  if (system) return system;
  let soft: Assessment | undefined = ran;
  const installed = installedAppRisk(command, view, facts);
  for (const check of [pathRisk(command, view, facts), installed]) {
    if (check?.tier === "danger") return check;
    soft ??= check;
  }
  // A secret that is also the installed app's data (`~/.cypher/orgs/…`) is both: once, and only on a consent naming it (core.ts).
  if (soft && soft !== installed && installed?.concern === "installed-app") soft = { ...soft, concern: "installed-app", data: installed.data };
  // A reversible, team-visible step on this task's own branch or pull request (grades.ts): the judge's, not a danger.
  const team = teamStep(command, view, facts);
  // The project's own tools resolve from where the command runs (`cd apps/edge && npx wrangler`).
  const here: RiskFacts = command.cwd === undefined ? facts : { ...facts, projectTool: (name, cwd) => facts.projectTool(name, cwd ?? command.cwd) };
  const danger =
    destroyDanger(command, view, facts) ??
    copyDanger(command, view, facts) ??
    uploadDanger(view) ??
    protectedTarget(view, facts) ??
    (view.program === "git"
      ? (gitOptionDanger(view, command.hints) ?? gitConfigDanger(view) ?? (team ? undefined : gitPushDanger(command, view, facts)) ?? gitDiscardDanger(command, view, facts) ?? gitHistoryDanger(view))
      : undefined) ??
    (team ? undefined : DANGER.map((entry) => dangerMatches(entry, view, here)).find((kind) => kind !== undefined));
  if (team && !danger) return { tier: "local", kind: team.kind, key: team.action, once: true };
  if (danger) {
    // Reading a secret or the environment on this machine is the model's to
    // approve, once; writing or signing in with one is not (a secret that
    // leaves the machine is the floor, `exfiltrates`).
    if (SECRET_READ_KINDS.has(danger) && !view.positionals.some((word) => SECRET_WRITE_VERB.test(word))) return { tier: "local", kind: danger, once: true, concern: "secret" };
    return { tier: "danger", kind: danger, key: dangerKey(danger) };
  }
  if (kill) return kill;
  if (soft) return soft;
  const entry = INSPECT.find((candidate) => inspectMatches(candidate, view));
  if (entry) return { tier: "inspect", ...(entry.outside ? { outside: true } : {}) };
  // The user added these because the sandbox cannot run them.
  if (facts.inspect.some((pattern) => !words.includes(UNKNOWN) && patternMatches(pattern, words))) return { tier: "inspect", outside: true };
  return { tier: "local" };
}

/**
 * A path to one of the permission system's own files written anywhere in a
 * command: as an operand, inside an option's value, or inside a script
 * (`python3 -c "open('~/.cypher/pi-runtime/agent/permissions.json','w')"`).
 */
const SYSTEM_FILE =
  /[~$\w./@{}-]*(?:permissions\.json|permission-audit\.jsonl|settings\.json|judge-health\.json|cypher-permissions[\w.-]*|sessions\/[\w./-]+)/g;

/**
 * Changing the permission system itself: its policy files, the settings that
 * say which extensions load, its extension's code, its audit log and the
 * session files an approval is restored from. The always-ask floor: no rule,
 * grant or reviewing model ever approves one. Reading them is ordinary work.
 */
function permissionSystemRisk(command: RiskCommand, view: CommandView, facts: RiskFacts): Assessment | undefined {
  const system = facts.permissionSystem;
  if (!system) return undefined;
  const home = facts.classify("~", undefined)?.path;
  const candidates = new Set<string>();
  for (const word of [...command.words, ...command.reads, ...command.writes, ...(command.carried ?? [])]) {
    if (word === UNKNOWN) continue;
    if (pathWords([word]).length || command.reads.includes(word) || command.writes.includes(word)) candidates.add(word);
    for (const match of word.matchAll(SYSTEM_FILE)) {
      const path = match[0];
      candidates.add(home === undefined ? path : path.replace(/^(\$HOME|\$\{HOME\})/, home));
    }
  }
  for (const candidate of candidates) {
    const classification = facts.classify(candidate, command.cwd);
    if (!classification || !system(classification.path)) continue;
    if (pathEffect(command, view, candidate) === "write" || command.words.some((word) => word !== UNKNOWN && word.includes(candidate) && WRITE_HINT.test(word))) {
      return { tier: "danger", kind: `changes the permission system itself (${candidate})`, key: "permission-system", floor: true };
    }
  }
  return undefined;
}

/** Pack kinds (packs.ts) that only read a secret here: the model may approve one, once. */
const SECRET_READ_KINDS: ReadonlySet<string> = new Set(["reads a secret", "prints the environment, which holds secrets"]);

/** Words that make a secret command more than a read: it signs in, stores or deletes. */
const SECRET_WRITE_VERB =
  /^(login|logout|signin|signout|add|add-.*|set|set-.*|delete|delete-.*|remove|rm|unlock|import|configure|write|store|create|rotate|edit|unset|inject|approve|reject|fill|refresh|switch|setup-git)$/i;

/** Programs that can carry what they are given off this machine. */
const SENDERS =
  /^(curl|wget|http|https|xh|nc|ncat|netcat|socat|telnet|ftp|ssh|scp|sftp|rsync|mosh|mail|mailx|sendmail|mutt|neomutt|msmtp|slack|slackcat|telegram-send|signal-cli|discord-send|ntfy|apprise|pushover|twilio|gh|glab|aws|gcloud|az|op|vault)$/;

/** What a variable's name holds anywhere in it when its value is a secret (`DB_PASSWORD`, `clientSecret`, `SESSION_COOKIE`). */
const SECRET_NAME = /secret|passw|apikey|api_key|credential|private_key|cookie/;

/** Words beside `token` that make it a count, not a credential (`TOKEN_LIMIT`; `MAX_TOKENS` is no token at all). */
const COUNT_WORDS: ReadonlySet<string> = new Set([
  "max", "min", "num", "n", "count", "counts", "limit", "limits", "budget", "len", "length", "size", "total", "per", "rate", "used", "usage", "window", "cost", "price", "avg", "mean",
]);

/**
 * Whether a variable's name says it holds a secret. `token` counts as a
 * word of its own, read word by word: `HF_TOKEN`, `authToken` and `MYTOKEN`
 * hold one; `MAX_TOKENS`, `num_tokens`, `TOKEN_LIMIT` and
 * `TOKENIZERS_PARALLELISM` hold none. So do `OPENAI_API_KEY` and
 * `privateKey`.
 */
export function secretName(name: string): boolean {
  if (SECRET_NAME.test(name.toLowerCase())) return true;
  const words = name.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase().split(/_+/).filter(Boolean);
  return words.some((word, index) => {
    if (word === "key") return /^(api|private)$/.test(words[index - 1] ?? "");
    return word.endsWith("token") && !words.some((other) => COUNT_WORDS.has(other));
  });
}

/** A secret variable written in a word the line's shell does not expand (`bash -c 'curl -H "$TOKEN"'`). */
function namesSecret(word: string): boolean {
  for (const match of word.matchAll(/\$\{?[#!]?([A-Za-z_][A-Za-z0-9_]*)/g)) if (secretName(match[1])) return true;
  return false;
}

/**
 * Programs that touch a file without reading what it holds: listing a key's
 * directory, testing for it, changing its mode, deleting it, or handing it
 * to the ssh agent sends nothing anywhere.
 */
const TOUCHES_ONLY: ReadonlySet<string> = new Set([...NAME_ONLY, "chmod", "chown", "chgrp", "chflags", "xattr", "touch", "rm", "rmdir", "unlink", "ssh-add", "ssh-keygen"]);

/** Options whose value is a key or certificate a connection is made with: used, not read out. */
const USES_KEY = /^(-i|-E|--identity|--key|--cert|--cacert|--capath|--client-cert|--pubkey|--proxy-cert|--proxy-key|--proxy-cacert)$/;

/** Options of the downloaders whose value is a file they write. */
const WRITES_OUTPUT = /^(-o|-O|--output|--output-document|--output-file|--output-dir|-D|--dump-header|-c|--cookie-jar|--trace|--trace-ascii|--stderr|-P|--directory-prefix|-a|--append-output)$/;

/** `security` subcommands that print a secret from the keychain. */
const KEYCHAIN_READS = /^(find-generic-password|find-internet-password|dump-keychain|export|find-key|unlock-keychain)$/;

function present(path: string): boolean {
  try {
    statSync(path);
    return true;
  } catch {
    return false;
  }
}

/** Options of curl and wget whose value is a file on this computer (alone, or last in a bundle: `-fsSLo out`). */
const TAKES_FILE =
  /^(-[A-Za-z]*[oOTKEbcDPai]|--output|--output-document|--output-file|--output-dir|--upload-file|--config|--cert|--key|--cacert|--capath|--cookie|--cookie-jar|--dump-header|--netrc-file|--trace|--trace-ascii|--stderr|--input-file|--post-file|--body-file|--load-cookies|--save-cookies|--ca-certificate|--certificate|--private-key|--directory-prefix|--append-output)$/;

/**
 * Whether a word could name a file on this computer: no URL, for a copy no
 * `host:path`, for curl and wget only an option's file or `@file` (a bare
 * word is a URL: `curl example.com/ca.pem`), and not the endpoint of `gh
 * api`.
 */
function localOperand(view: CommandView, word: string, before = ""): boolean {
  if (/^[a-z][\w+.-]*:\/\//i.test(word)) return false;
  if (/^(scp|rsync|sftp)$/.test(view.program) && remoteOperand(word) !== undefined) return false;
  if (/^(curl|wget|http|https|xh|aria2c)$/.test(view.program) && !TAKES_FILE.test(before) && !/^[^=@\s]*=?@/.test(word)) return false;
  if (/^(gh|glab)$/.test(view.program) && view.positionals[0] === "api" && word === view.positionals[1]) return false;
  return true;
}

/**
 * The words of a command that name files it could read and pass on, as
 * written; an unknown word gives what it surely starts with. ssh names none
 * (its options are used here, its host and command are the other
 * machine's); a copy names its local sources when it uploads; a download's
 * output file is written, not read.
 */
function fileWords(command: RiskCommand, view: CommandView): string[] {
  if (TOUCHES_ONLY.has(view.program) || /^(ssh|mosh)$/.test(view.program)) return [];
  if (view.program === "scp" || view.program === "rsync") {
    const copy = copyOperands(view.words);
    if (copy) return remoteOperand(copy.dest) ? copy.sources.filter((word) => !remoteOperand(word)) : [];
  }
  const out: string[] = [];
  // `aws s3 cp s3://bucket/key file`: copied from a URL, the last word is a file it writes.
  const url = (word: string) => /^[a-z][\w+.-]*:\/\//i.test(word);
  const last = view.positionals.at(-1);
  const written = last !== undefined && last !== UNKNOWN && !url(last) && view.positionals.slice(0, -1).some((word) => word !== UNKNOWN && url(word)) ? view.args.lastIndexOf(last) : -1;
  for (const [at, word] of view.args.entries()) {
    const before = view.args[at - 1] ?? "";
    if (USES_KEY.test(before) || /IdentityFile/i.test(before)) continue;
    if (DOWNLOADERS.has(view.program) && WRITES_OUTPUT.test(before)) continue;
    if (at === written && !before.startsWith("-")) continue;
    if (word === UNKNOWN) {
      // `~/.ssh/id_*`: the directory it surely points into, and the name it starts.
      const hint = command.hints?.[at + 1];
      if (!hint || hint.startsWith("-") || !localOperand(view, hint, before)) continue;
      out.push(hint.includes("/") ? hint.slice(0, hint.lastIndexOf("/") + 1) : "./");
      if (!hint.endsWith("/")) out.push(hint);
      continue;
    }
    if (word.startsWith("-") || !localOperand(view, word, before)) continue;
    out.push(word);
  }
  return out;
}

/**
 * The always-ask floor for a whole line: a secret or credential leaving this
 * machine. A line that both reaches a secret (a sensitive file that is
 * there, the keychain, the environment, a variable named like a credential)
 * and runs a program that can send what it is given always asks the user,
 * whatever the owner wrote and whatever the model says. A command that only
 * prints a secret here does not: the model may approve that once
 * (`pathRisk`).
 *
 * Only what the line can really read counts. A URL, a `host:path`, an ssh
 * host or the command it runs there, an option's value such as
 * `BatchMode=yes` and a file a download writes are no files this machine
 * reads; a bare name that is not there (and that no other command of the
 * line makes) holds nothing to send.
 */
export function exfiltrates(commands: readonly RiskCommand[], facts: RiskFacts, expands: readonly string[] = []): string | undefined {
  let sender: string | undefined;
  let secret: string | undefined;
  /** A name some other command of the line also uses: it may make the file this one sends. */
  const elsewhere = (word: string, self: RiskCommand) => commands.some((other) => other !== self && (other.words.includes(word) || other.writes.includes(word)));
  const holdsSecret = (word: string, command: RiskCommand): boolean => {
    const classification = facts.classify(word, command.cwd);
    if (classification?.zone !== "sensitive") return false;
    // Named in full (`~/.aws/credentials`), it is the secret whether or not it
    // can be seen from here; a bare name must be a file that is there.
    return isAbsolute(word) || word.startsWith("~") || present(classification.path) || elsewhere(word, command);
  };
  for (const command of commands) {
    // What a redirection feeds a command is a file it reads, whatever the command.
    for (const path of command.reads) if (path !== UNKNOWN && holdsSecret(path, command)) secret ??= path;
    const words = command.words;
    if (words.length === 0) continue;
    const view = viewOf(words);
    if (SENDERS.test(view.program)) {
      // A hosting CLI only sends what a writing verb carries.
      const sends = !/^(gh|glab)$/.test(view.program) || view.positionals.some((word) => /^(api|gist|secret|variable|release|pr|issue|repo)$/.test(word));
      if (sends) sender ??= view.program;
    }
    for (const word of fileWords(command, view)) if (holdsSecret(word, command)) secret ??= word;
    // The keychain and the environment hold secrets whatever file they name.
    if (view.program === "security" && view.positionals.some((word) => word === UNKNOWN || KEYCHAIN_READS.test(word))) secret ??= "the keychain";
    if (view.program === "printenv" && (view.positionals.length === 0 || view.positionals.some((word) => word === UNKNOWN || secretName(word)))) secret ??= "the environment";
    if (view.program === "env" && view.positionals.length === 0) secret ??= "the environment";
    // `$TOKEN` in a word this shell leaves as written, for a program that expands it here
    // (`bash -c '…'`). What ssh runs on the other machine reads that machine's variables.
    const remote = /^(ssh|mosh)$/.test(view.program) ? sshHostIndex(words) : undefined;
    if (words.some((word, at) => word !== UNKNOWN && (remote === undefined || at <= remote) && namesSecret(word))) secret ??= "a value in this line";
  }
  // A variable named like a credential that the line's own shell expands for its commands.
  if (expands.some(secretName)) secret ??= "a value in this line";
  if (sender === undefined || secret === undefined) return undefined;
  return `sends a secret off this machine (${secret}) through ${sender}`;
}

/** The files a download writes: `-o`/`-O` values, a redirection, or the URL's file name (`curl -O`, `wget`). */
function downloadedFiles(command: { words: readonly string[]; writes?: readonly string[] }): string[] {
  const words = command.words;
  const name = basename(words[0] ?? "");
  const files = [...(command.writes ?? [])];
  const urls = words.slice(1).filter((word) => /^[a-z][\w+.-]*:\/\//i.test(word) || word === UNKNOWN);
  const fromUrl = () => urls.map((url) => (url === UNKNOWN ? UNKNOWN : basename(url.replace(/[?#].*$/, ""))));
  for (let i = 1; i < words.length; i++) {
    const word = words[i];
    if (name === "curl" && (word === "-o" || word === "--output")) files.push(words[++i] ?? UNKNOWN);
    else if (name === "curl" && /^-[A-Za-z]*o$/.test(word) && word.length > 2) files.push(words[++i] ?? UNKNOWN);
    else if (name === "curl" && (/^-[A-Za-z]*O[A-Za-z]*$/.test(word) || word === "--remote-name" || word === "--remote-name-all")) files.push(...fromUrl());
    else if (name === "wget" && (word === "-O" || word === "--output-document")) files.push(words[++i] ?? UNKNOWN);
    else if (name === "wget" && word.startsWith("--output-document=")) files.push(word.slice("--output-document=".length));
  }
  if (name === "wget" && !words.some((word) => word === "-O" || word.startsWith("--output-document"))) files.push(...fromUrl());
  return files;
}

/**
 * A line that downloads code and runs it: after a download, an interpreter
 * reading its input (`curl … | sh`) or a downloaded file (`sh i.sh`), or a
 * downloaded file run as a program (`./i.sh`, `source i.sh`).
 */
export function downloadsAndRuns(commands: readonly { words: readonly string[]; writes?: readonly string[] }[]): boolean {
  let downloaded = false;
  const written = new Set<string>();
  for (const command of commands) {
    const program = command.words[0] ?? "";
    const name = basename(program);
    if (DOWNLOADERS.has(name)) {
      downloaded = true;
      for (const file of downloadedFiles(command)) written.add(file === UNKNOWN ? UNKNOWN : basename(file));
      continue;
    }
    if (!downloaded) continue;
    const isWritten = (word: string) => word === UNKNOWN || written.has(UNKNOWN) || written.has(basename(word));
    if (program !== UNKNOWN && program.includes("/") && isWritten(program)) return true;
    if (program !== UNKNOWN && written.has(name)) return true;
    const operands = command.words.slice(1).filter((word) => !word.startsWith("-"));
    if (name === "source" || name === ".") {
      if (operands.some(isWritten)) return true;
      continue;
    }
    if (!INTERPRETERS.test(name)) continue;
    if (operands.length === 0 || operands[0] === UNKNOWN || operands.some((word) => isWritten(word) || word === "/dev/stdin")) return true;
  }
  return false;
}

/**
 * A line the reader cannot parse (opaque): never `inspect`. It is `danger`
 * when a command in it could be one: each `;`, `&`, `|`, `(`, backtick or
 * newline starts a command, and so does a quoted string (what `bash -c` or
 * `ssh host` would run). Words with `$` are known only when it runs.
 *
 * A secret leaving the machine is the floor here too: a credential's
 * variable or a secret file anywhere in a line that runs a program that
 * sends. Not for `remote`, the line ssh runs on another machine: its
 * variables and files are that machine's.
 */
export function assessOpaque(line: string, facts: RiskFacts, remote = false, cwd?: string): Assessment {
  // Its words with `$` are unknown: look for the installed app in its text
  // instead. Only a line that writes where it names it is dangerous; one
  // that reads it is local work like any other read.
  const home = facts.installedApp ? facts.classify("~", undefined)?.path : undefined;
  const installed = installedMention(line, facts, home);
  if (installed && mentionWrites(home === undefined ? line : line.split(home).join("~"), installed)) {
    const kind = `changes the installed Cypher app or its data (${installed})`;
    return { tier: "danger", kind, key: "installed Cypher app" };
  }
  const segments: string[][] = [];
  const pieces = line.split(/;|&&|\|\||\||&|\(|\)|`|\$\(|\n|<|>/);
  for (const piece of pieces) {
    segments.push(tokens(piece));
    for (const quoted of piece.matchAll(/'([^']*)'|"([^"]*)"/g)) segments.push(tokens(quoted[1] ?? quoted[2] ?? ""));
  }
  const all = segments.flat();
  // A secret leaving the machine is the always-ask floor here too: a
  // credential's variable anywhere in a line that runs a program that sends.
  const sender = remote ? undefined : all.find((word) => SENDERS.test(basename(word)));
  if (sender) {
    const file = all.find((word, at) => {
      if (word === UNKNOWN || word.startsWith("-") || USES_KEY.test(all[at - 1] ?? "") || /IdentityFile/i.test(all[at - 1] ?? "")) return false;
      // The word, or the file a sender reads it as (`@.env`, `key=@file`), from where the line runs.
      const names = [word, word.replace(/^[^=@]*=?@/, "")].filter((name, index, list) => name && list.indexOf(name) === index);
      return names.some((name) => {
        const classification = facts.classify(name, cwd);
        return classification?.zone === "sensitive" && present(classification.path);
      });
    });
    const secret = file ?? (namesSecret(line) ? "a value in this line" : undefined);
    if (secret) return { tier: "danger", kind: `may send a secret off this machine (${secret}) through ${basename(sender)}`, key: "secret-off-machine", floor: true };
  }
  if (all.some((word) => DOWNLOADERS.has(basename(word))) && all.some((word) => INTERPRETERS.test(basename(word)))) {
    return { tier: "danger", kind: "may download code and run it", key: "download-run" };
  }
  for (const segment of segments) {
    let start = 0;
    // Assignments and the words of compound commands (`{ npm publish; }`, `if x; then …`).
    for (;;) {
      const word = segment[start];
      if (word === undefined) break;
      if (/^[A-Za-z_]\w*=/.test(word) || OPAQUE_KEYWORDS.has(word)) start++;
      else if (word === "function") start += 2;
      else if (/^[A-Za-z_][\w-]*\(\)$/.test(word)) start++;
      else break;
    }
    for (let at = start; at < segment.length; at++) {
      const words = segment.slice(at);
      // Wrappers and runners hand their words to the next program.
      const assessment = assessCommand({ words, cwd: undefined, reads: [], writes: [] }, facts);
      if (assessment.tier === "danger") return assessment;
      if (!/^(sudo|doas|nohup|time|timeout|nice|env|exec|command|xargs|caffeinate|watch|-c|-lc|-ic)$/.test(segment[at]) && !/^\d+[smhd]?$/.test(segment[at])) break;
    }
  }
  return { tier: "local", kind: "a command line rules can't read" };
}

const OPAQUE_KEYWORDS: ReadonlySet<string> = new Set(["{", "}", "!", "if", "then", "elif", "else", "fi", "while", "until", "do", "done", "for", "case", "esac", "select", "coproc", "time", "[[", "]]"]);

/**
 * Whether a line a rule can't read writes where it names `mention`: a
 * redirection onto it, a program that changes or deletes what it names just
 * before it, or a script opening it for writing. A line this can't place
 * counts as writing.
 */
function mentionWrites(line: string, mention: string): boolean {
  const at = line.indexOf(mention);
  if (at < 0) return true;
  const before = line.slice(Math.max(0, at - 60), at);
  const after = line.slice(at + mention.length, at + mention.length + 60);
  if (/(>>?|\btee\b|-o|--output[= ]?)\s*["'$]?[\w/~.$-]*$/.test(before)) return true;
  if (/\b(cp|mv|ln|install|rsync|ditto|dd|truncate|chmod|chown|chgrp|chflags|xattr|sed|rm|rmdir|unlink|shred|trash|mkdir|touch|git)\s+(?:(?:-\S+|["'$]?[\w/~.$-]+["']?)\s+)*["'$]?[\w/~.$-]*$/.test(before)) return true;
  if (/^[\w/.$-]*["']\s*,\s*["'][waxr]\+?/.test(after)) return true;
  return WRITE_HINT.test(after);
}

function tokens(text: string): string[] {
  return text
    .split(/\s+/)
    .map((word) => word.replace(/^['"]+|['"]+$/g, ""))
    .filter(Boolean)
    .map((word) => (word.includes("$") ? UNKNOWN : word));
}
