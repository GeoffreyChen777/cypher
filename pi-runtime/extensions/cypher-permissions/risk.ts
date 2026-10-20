/**
 * Risk tiers for commands that would leave the sandbox (design:
 * docs/design/permission-friction.md §3). A tier is a property of the
 * effect, not of the program:
 *
 * - `inspect` (R0): changes nothing and moves no data off the machine. Runs
 *   outside the sandbox without asking.
 * - `local` (R1): changes only this machine's, the user's, undoable things.
 *   The reviewing model decides; failing that the user, once per kind.
 * - `danger` (R2): irreversible, reaches other people or shared systems,
 *   persists beyond the task, or exposes a secret. Always a human.
 *
 * The classifier is deterministic and conservative: a word known only when
 * the line runs counts as whatever would make the command dangerous, and a
 * line the reader cannot parse is never `inspect`. Program knowledge lives
 * in packs.ts; this file holds the checks that need the machine (paths,
 * processes, the repository) and the ones that span packs.
 */

import { basename, isAbsolute } from "node:path";

import { patternMatches } from "./commands.ts";
import { DANGER, dangerMatches, INSPECT, inspectMatches, viewOf } from "./packs.ts";
import type { CommandView, PackFacts } from "./packs.ts";
import type { Classification } from "./paths.ts";
import { UNKNOWN } from "./shell.ts";

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
}

/** What `kill`, `pkill` and `killall` would stop. */
export type KillTarget = { pids: number[] } | { pgrep: string[] };

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
  /** The repository at a directory (undefined: none, or not known). */
  git(dir: string): GitFacts | undefined;
  /** A directory inside the chat's workspace. */
  inWorkspace(dir: string): boolean;
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

function protectedTarget(view: CommandView, facts: RiskFacts): string | undefined {
  if (!REMOTE.has(view.program) && view.program !== "git") return undefined;
  if (view.program === "git" && view.positionals[0] !== "push") return undefined;
  for (const word of view.args) {
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

function pathDanger(command: RiskCommand, view: CommandView, facts: RiskFacts): string | undefined {
  for (const path of command.reads) {
    const classification = facts.classify(path, command.cwd);
    if (!classification || classification.zone === "sensitive") return `reads ${classification ? "a secret" : `${path}, in a directory known only when it runs`}`;
  }
  for (const path of command.writes) {
    const classification = facts.classify(path, command.cwd);
    if (!classification) return `writes ${path}, in a directory known only when it runs`;
    const why = writeDanger(classification);
    if (why) return why;
  }
  const words = view.args;
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (!pathWords([word]).length) continue;
    // ssh's identity file: the key is used, not read out.
    if (/^(ssh|scp|sftp|rsync|git|mosh)$/.test(view.program) && (words[i - 1] === "-i" || /IdentityFile/i.test(words[i - 1] ?? ""))) continue;
    const classification = facts.classify(word, command.cwd);
    if (!classification) continue;
    if (classification.zone === "sensitive" && !NAME_ONLY.has(view.program)) return `reads or writes a secret (${word})`;
    if (WRITERS.has(view.program) && view.program !== "git") {
      const why = writeDanger(classification);
      if (why) return why;
    }
  }
  return undefined;
}

function writeDanger(classification: Classification): string | undefined {
  if (classification.zone === "sensitive") return "writes a secret";
  if (classification.guard === "self") return "changes Cypher's own data";
  if (classification.guard === "protected") return "changes a file that runs code later";
  return undefined;
}

/** Deleting or moving files outside the workspace, scratch space and caches. */
function destroyDanger(command: RiskCommand, view: CommandView, facts: RiskFacts): string | undefined {
  let operands: string[] = [];
  if (DESTROYERS.has(view.program)) operands = view.args.filter((word) => word === UNKNOWN || !word.startsWith("-"));
  else if (view.program === "rsync" && view.args.some((word) => word === UNKNOWN || word.startsWith("--delete") || word === "--remove-source-files")) operands = view.positionals.filter((word) => !/^[\w.-]*@?[\w.-]+:/.test(word));
  else if (view.program === "find" && view.args.some((word) => word === UNKNOWN || word === "-delete" || word === "-exec" || word === "-execdir" || word === "-ok")) {
    for (const word of view.args) {
      if (word.startsWith("-") || word === "!" || word === "(") break;
      operands.push(word);
    }
    if (operands.length === 0) operands = ["."];
  } else if (/^(chmod|chown|chgrp)$/.test(view.program) && view.args.some((word) => word === UNKNOWN || /^-[A-Za-z]*R/.test(word))) {
    operands = view.positionals.slice(1);
  } else return undefined;
  for (const operand of operands) {
    if (operand === UNKNOWN) return `${verb(view.program)} files named only when it runs`;
    if (command.cwd === undefined && !isAbsolute(operand) && !operand.startsWith("~")) return `${verb(view.program)} files in a directory known only when it runs`;
    const classification = facts.classify(operand, command.cwd);
    if (!classification || !facts.disposable(classification.path)) return `${verb(view.program)} files outside the workspace (${operand})`;
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
  const owned = facts.ownsProcesses(target);
  if (owned === true) return { tier: "inspect", kind: "stops processes this chat started", outside: true };
  if (owned === false) return { tier: "danger", kind: "stops processes this chat did not start" };
  // macOS hides the environment of its own binaries; nothing matched a name.
  return { tier: "danger", kind: "stops processes it cannot tell are this chat's" };
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
  for (const refspec of refspecs) {
    if (refspec.startsWith("+")) return "force-pushes a branch";
    const [source, target = source] = refspec.split(":");
    if (source === "") return "deletes a remote branch";
    const name = target.replace(/^refs\/heads\//, "");
    if (target.startsWith("refs/tags/") || (source.startsWith("refs/tags/") && !refspec.includes(":"))) return "pushes a tag (publishes a release)";
    const branch = name === "HEAD" ? git?.current : name;
    if (!branch || !git) return "pushes to a branch it cannot tell";
    if (!refspec.includes(":") && !git.isBranch(branch) && name !== "HEAD") return `pushes ${branch}, which is no local branch (a tag publishes a release)`;
    if (isDefault(branch, git, facts)) return `publishes to ${branch}`;
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

/** Discarding work in a checkout that is not the chat's. */
function gitDiscardDanger(command: RiskCommand, view: CommandView, facts: RiskFacts): string | undefined {
  const [sub] = view.positionals;
  const discards =
    (sub === "reset" && view.args.some((word) => word === "--hard" || word === "--merge" || word === "--keep")) ||
    (sub === "clean" && view.args.some((word) => /^-[A-Za-z]*f/.test(word) || word === "--force")) ||
    (sub === "checkout" && (view.args.includes("--") || view.args.includes(".") || view.args.some((word) => /^-[A-Za-z]*f/.test(word)))) ||
    (sub === "restore" && !view.args.includes("--staged")) ||
    (sub === "switch" && view.args.some((word) => word === "--discard-changes" || word === "-f" || word === "--force")) ||
    (sub === "worktree" && view.positionals[1] === "remove") ||
    (sub === "branch" && view.args.some((word) => /^-[A-Za-z]*D/.test(word)));
  if (!discards) return undefined;
  const dir = gitDirectory(command, view);
  if (dir !== undefined && facts.inWorkspace(dir)) return undefined;
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
  if (view.program === "ssh" && view.positionals.length > 1) return { line: view.positionals.slice(1).join(" "), where: view.positionals[0] };
  return undefined;
}

/** The tier of one command seen through its wrappers. */
export function assessCommand(command: RiskCommand, facts: RiskFacts): Assessment {
  const words = command.words;
  if (words.length === 0) return { tier: "inspect" };
  const view = viewOf(words);
  if (words[0] === UNKNOWN) return { tier: "local", kind: "runs a program known only when it runs" };
  const inner = innerLine(view);
  if (inner) {
    const assessment = inner.line.includes(UNKNOWN) ? { tier: "danger" as const, kind: "runs a command known only when it runs" } : assessOpaque(inner.line, facts);
    if (assessment.tier === "danger") {
      const kind = inner.where ? `on ${inner.where}: ${assessment.kind}` : assessment.kind!;
      return { tier: "danger", kind, key: kind };
    }
  }
  const kill = killAssessment(view, facts);
  if (kill?.tier === "danger") return kill;
  const danger =
    pathDanger(command, view, facts) ??
    destroyDanger(command, view, facts) ??
    protectedTarget(view, facts) ??
    (view.program === "git" ? (gitPushDanger(command, view, facts) ?? gitDiscardDanger(command, view, facts)) : undefined) ??
    DANGER.map((entry) => dangerMatches(entry, view, facts)).find((kind) => kind !== undefined);
  if (danger) return { tier: "danger", kind: danger, key: dangerKey(danger) };
  if (kill) return kill;
  const entry = INSPECT.find((candidate) => inspectMatches(candidate, view));
  if (entry) return { tier: "inspect", ...(entry.outside ? { outside: true } : {}) };
  // The user added these because the sandbox cannot run them.
  if (facts.inspect.some((pattern) => !words.includes(UNKNOWN) && patternMatches(pattern, words))) return { tier: "inspect", outside: true };
  return { tier: "local" };
}

/** A line that downloads code and runs it: a downloader beside an interpreter reading its input or a file the download wrote. */
export function downloadsAndRuns(commands: readonly { words: readonly string[] }[]): boolean {
  const downloads = commands.filter((command) => DOWNLOADERS.has(basename(command.words[0] ?? "")));
  if (downloads.length === 0) return false;
  const written = new Set(downloads.flatMap((command) => command.words.filter((_, i) => /^(-o|--output|-O)$/.test(command.words[i - 1] ?? ""))));
  return commands.some((command) => {
    const name = basename(command.words[0] ?? "");
    if (!INTERPRETERS.test(name)) return false;
    const operands = command.words.slice(1).filter((word) => !word.startsWith("-"));
    return operands.length === 0 || operands[0] === UNKNOWN || operands.some((word) => written.has(word) || word === "/dev/stdin");
  });
}

/**
 * A line the reader cannot parse (opaque): never `inspect`. It is `danger`
 * when a command in it could be one: each `;`, `&`, `|`, `(`, backtick or
 * newline starts a command, and so does a quoted string (what `bash -c` or
 * `ssh host` would run). Words with `$` are known only when it runs.
 */
export function assessOpaque(line: string, facts: RiskFacts): Assessment {
  const segments: string[][] = [];
  const pieces = line.split(/;|&&|\|\||\||&|\(|\)|`|\$\(|\n|<|>/);
  for (const piece of pieces) {
    segments.push(tokens(piece));
    for (const quoted of piece.matchAll(/'([^']*)'|"([^"]*)"/g)) segments.push(tokens(quoted[1] ?? quoted[2] ?? ""));
  }
  const all = segments.flat();
  if (all.some((word) => DOWNLOADERS.has(basename(word))) && all.some((word) => INTERPRETERS.test(basename(word)))) {
    return { tier: "danger", kind: "may download code and run it", key: "download-run" };
  }
  for (const segment of segments) {
    let start = 0;
    while (start < segment.length && /^[A-Za-z_]\w*=/.test(segment[start])) start++;
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

function tokens(text: string): string[] {
  return text
    .split(/\s+/)
    .map((word) => word.replace(/^['"]+|['"]+$/g, ""))
    .filter(Boolean)
    .map((word) => (word.includes("$") ? UNKNOWN : word));
}
