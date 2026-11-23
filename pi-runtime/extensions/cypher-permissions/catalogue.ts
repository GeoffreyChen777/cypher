/**
 * The creator/destroyer catalogue: per program and subcommand, which kind of
 * entity it creates, destroys or stops, and where the entity's name comes
 * from — an operand, an option's value, or the command's own output (`xcrun
 * simctl create` prints the new simulator's UDID). One table (`CATALOGUE`)
 * and one interpreter; a new tool is a new row, not new code.
 *
 * Creating is two steps around the run (created.ts):
 * - `planCreations`, before the line runs: every entity its commands may
 *   create, each with how it will be confirmed, and whether it is there
 *   already (a path, a local branch or tag): what was there before is never
 *   the task's;
 * - `settleCreations`, after: which of them the line surely made.
 *
 * How a creation is confirmed (`Confirm`):
 * - `observed`: a path, worktree or local ref that was not there before and
 *   is there after;
 * - `succeeded`: the line exited 0 and every command in it ran in the line's
 *   own shell joined by `&&` (so each one succeeded), and the creator is the
 *   system's program;
 * - `output`: `succeeded`, the creator is the line's only command, and its
 *   output's last line has the row's shape. Text that `echo`, `printf`,
 *   `cat` or any other command prints can never mint an entry: with another
 *   command in the line nothing is read from the output at all;
 * - `ran`: the line ran (a dev instance names its data directories before it
 *   makes them, in the background).
 *
 * Destroying (`targetsOf`): the entities a command deletes or stops, for
 * the ledger to tell whether this chat made them.
 */

import { basename, isAbsolute, join, resolve } from "node:path";

import type { LedgerAction, LedgerEntity, LedgerKind } from "./ledger.ts";
import { UNKNOWN } from "./shell.ts";
import type { Joint } from "./shell.ts";

/** What a row's command does: creates, destroys or stops its entity, or starts or merges one (lifecycle.ts antecedents). */
export type Op = "create" | "destroy" | "stop" | "start" | "merge";

/** Where an entity's name comes from. */
export type IdSource =
  /** The nth operand after the subcommand (from 0). */
  | { from: "operand"; at: number }
  /** Every operand. */
  | { from: "operands" }
  /** The last operand (a copy's destination). */
  | { from: "last" }
  /** An option's value. */
  | { from: "option"; names: readonly string[] }
  /** The last line of the creator's own output: the group `name` is the name; groups `repo`, `version` fill those fields. */
  | { from: "output"; pattern: RegExp }
  /** `git clone URL [DIR]`: DIR, else the repository's name. */
  | { from: "clone-target" }
  /** `git push REMOTE REFSPEC…`: the refs it creates or deletes on the remote. */
  | { from: "refspecs" };

export interface CatalogueRow {
  /** Program names (basename of the first word); `*`: any program, when no row names it. */
  programs: readonly string[];
  /** The subcommand path after the program's global options. */
  sub: readonly string[];
  op: Op;
  kind: LedgerKind;
  id: IdSource;
  /** Options that take a value (so it is no operand). */
  values?: readonly string[];
  /** One of these options must be there (`-d` for `git branch -d`). */
  requires?: readonly string[];
  /** None of these may be there (`--rm` for `docker run --name x`). */
  forbids?: readonly string[];
  /** The entity lives in the repository the command runs in (branches, tags). */
  inRepo?: boolean;
  /** Options whose value fills a field of the entity. */
  fields?: Readonly<Record<string, "repo" | "head">>;
}

/** Per program: options before the subcommand that take a value, `-C`-like options that move it, and ones that point it at another machine. */
const GLOBALS: Readonly<Record<string, { values: readonly string[]; cwd?: string; foreign?: readonly string[] }>> = {
  git: { values: ["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path", "--config-env"], cwd: "-C" },
  docker: { values: ["--config", "-c", "--context", "-H", "--host", "-l", "--log-level", "--tlscacert", "--tlscert", "--tlskey"], foreign: ["-c", "--context", "-H", "--host"] },
  podman: { values: ["--connection", "-c", "--url", "--identity", "--root", "--runroot"], foreign: ["--connection", "-c", "--url", "--remote", "-r"] },
  gh: { values: [] },
  xcrun: { values: ["--sdk", "--toolchain"] },
  npm: { values: ["--registry", "-w", "--workspace", "--prefix"] },
  multipass: { values: [] },
};

const BRANCH_LISTING = ["-d", "-D", "-m", "-M", "-c", "-C", "--delete", "--move", "--copy", "--list", "-l", "-a", "-r", "--all", "--remotes", "--show-current", "--contains", "--no-contains", "--merged", "--no-merged", "--points-at", "-v", "-vv", "--verbose", "--format", "--sort", "-u", "--set-upstream-to", "--unset-upstream", "--edit-description"];
const TAG_LISTING = ["-d", "--delete", "-l", "--list", "-v", "--verify", "-n", "--contains", "--no-contains", "--points-at", "--merged", "--no-merged", "--sort", "--format"];
const TAG_VALUES = ["-m", "-F", "-u", "--message", "--file", "--local-user", "--cleanup"];
const DOCKER_RUN_VALUES = [
  "--name", "-e", "--env", "--env-file", "-p", "--publish", "-v", "--volume", "--mount", "-w", "--workdir", "-u", "--user", "--network", "--net", "--entrypoint", "-l", "--label",
  "--platform", "--restart", "--memory", "-m", "--cpus", "-h", "--hostname", "--add-host", "--device", "--cap-add", "--cap-drop", "--security-opt", "--ulimit", "--pull", "--log-driver", "--log-opt", "--shm-size", "--tmpfs", "--expose", "--gpus", "--ipc", "--pid", "--dns",
];
const UUID_LINE = /^(?<name>[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12})$/;
const CONTAINER_LINE = /^(?<name>[0-9a-f]{64})$/;
const GITHUB_HOSTED = "https://[A-Za-z0-9.-]+/";

/** The catalogue. Rows of one program are tried longest `sub` first; every row that matches applies. */
export const CATALOGUE: readonly CatalogueRow[] = [
  // Files and directories.
  { programs: ["mkdir"], sub: [], op: "create", kind: "path", id: { from: "operands" }, values: ["-m", "--mode", "--context"] },
  { programs: ["install"], sub: [], op: "create", kind: "path", id: { from: "operands" }, values: ["-m", "--mode", "-o", "--owner", "-g", "--group"], requires: ["-d", "--directory"] },
  { programs: ["touch"], sub: [], op: "create", kind: "path", id: { from: "operands" }, values: ["-t", "-d", "-r", "--reference", "--date"] },
  { programs: ["tee"], sub: [], op: "create", kind: "path", id: { from: "operands" } },
  { programs: ["cp", "mv", "ditto", "ln"], sub: [], op: "create", kind: "path", id: { from: "last" }, values: ["-S", "--suffix"] },
  { programs: ["rsync"], sub: [], op: "create", kind: "path", id: { from: "last" }, values: ["-e", "--rsh", "--exclude", "--include", "--filter", "-f", "--rsync-path", "--exclude-from", "--include-from", "--files-from", "--log-file", "--partial-dir", "--temp-dir", "-T", "--backup-dir", "--suffix"] },
  { programs: ["mktemp"], sub: [], op: "create", kind: "path", id: { from: "output", pattern: /^(?<name>\/\S+)$/ }, values: ["-p", "--tmpdir", "--suffix", "-t"] },
  // Files a program writes where an option or a subcommand says: confirmed only by being there after and not before.
  { programs: ["*"], sub: [], op: "create", kind: "path", id: { from: "option", names: ["-o", "--output", "--out", "--output-file", "--output-document", "--out-file", "--outfile"] }, values: ["-o", "--output", "--out", "--output-file", "--output-document", "--out-file", "--outfile"] },
  { programs: ["xcrun"], sub: ["simctl", "io"], op: "create", kind: "path", id: { from: "last" }, values: ["--type", "--display", "--mask", "--codec"] },
  { programs: ["screencapture"], sub: [], op: "create", kind: "path", id: { from: "operands" }, values: ["-t", "-T", "-R", "-l", "-D", "-V", "-P"] },
  { programs: ["rm", "unlink", "trash", "shred"], sub: [], op: "destroy", kind: "path", id: { from: "operands" }, values: ["-n", "--iterations", "-s", "--size"] },
  { programs: ["rmdir"], sub: [], op: "destroy", kind: "path", id: { from: "operands" } },
  // git: checkouts, branches, tags, and the remote's refs.
  { programs: ["git"], sub: ["worktree", "add"], op: "create", kind: "worktree", id: { from: "operand", at: 0 }, values: ["-b", "-B", "--reason", "--orphan"] },
  { programs: ["git"], sub: ["worktree", "add"], op: "create", kind: "path", id: { from: "operand", at: 0 }, values: ["-b", "-B", "--reason", "--orphan"] },
  { programs: ["git"], sub: ["worktree", "add"], op: "create", kind: "branch", id: { from: "option", names: ["-b", "-B", "--orphan"] }, values: ["-b", "-B", "--reason", "--orphan"], inRepo: true },
  { programs: ["git"], sub: ["worktree", "remove"], op: "destroy", kind: "worktree", id: { from: "operand", at: 0 } },
  { programs: ["git"], sub: ["clone"], op: "create", kind: "path", id: { from: "clone-target" }, values: ["-b", "--branch", "-o", "--origin", "--depth", "--reference", "--separate-git-dir", "-c", "--config", "--filter", "-u", "--upload-pack", "-j", "--jobs", "--template", "--shallow-since", "--shallow-exclude"] },
  { programs: ["git"], sub: ["branch"], op: "create", kind: "branch", id: { from: "operand", at: 0 }, forbids: BRANCH_LISTING, inRepo: true },
  { programs: ["git"], sub: ["branch"], op: "destroy", kind: "branch", id: { from: "operands" }, requires: ["-d", "-D", "--delete"], forbids: ["-r", "--remotes", "-a", "--all"], inRepo: true },
  { programs: ["git"], sub: ["checkout"], op: "create", kind: "branch", id: { from: "option", names: ["-b", "-B", "--orphan"] }, values: ["-b", "-B", "--orphan"], inRepo: true },
  { programs: ["git"], sub: ["switch"], op: "create", kind: "branch", id: { from: "option", names: ["-c", "-C", "--create", "--force-create", "--orphan"] }, values: ["-c", "-C", "--create", "--force-create", "--orphan"], inRepo: true },
  { programs: ["git"], sub: ["tag"], op: "create", kind: "tag", id: { from: "operand", at: 0 }, forbids: TAG_LISTING, values: TAG_VALUES, inRepo: true },
  { programs: ["git"], sub: ["tag"], op: "destroy", kind: "tag", id: { from: "operands" }, requires: ["-d", "--delete"], inRepo: true },
  { programs: ["git"], sub: ["push"], op: "create", kind: "branch", id: { from: "refspecs" }, values: ["-o", "--push-option", "--repo", "--receive-pack", "--exec"], inRepo: true },
  // Hosted repositories, pull requests, releases.
  { programs: ["gh"], sub: ["repo", "create"], op: "create", kind: "repo", id: { from: "output", pattern: new RegExp(`^${GITHUB_HOSTED}(?<name>[\\w.-]+/[\\w.-]+?)(?:\\.git)?/?$`) } },
  { programs: ["gh"], sub: ["repo", "delete"], op: "destroy", kind: "repo", id: { from: "operand", at: 0 } },
  { programs: ["gh"], sub: ["pr", "create"], op: "create", kind: "pr", id: { from: "output", pattern: new RegExp(`^${GITHUB_HOSTED}(?<repo>[\\w.-]+/[\\w.-]+)/pull/(?<name>\\d+)$`) }, values: ["-t", "--title", "-b", "--body", "-F", "--body-file", "-B", "--base", "-H", "--head", "-R", "--repo", "-a", "--assignee", "-l", "--label", "-m", "--milestone", "-r", "--reviewer", "-p", "--project", "-T", "--template"], fields: { "-H": "head", "--head": "head" } },
  { programs: ["gh"], sub: ["pr", "merge"], op: "merge", kind: "pr", id: { from: "operand", at: 0 }, values: ["-b", "--body", "-F", "--body-file", "-t", "--subject", "-A", "--author-email", "--match-head-commit", "-R", "--repo"], fields: { "-R": "repo", "--repo": "repo" } },
  { programs: ["gh"], sub: ["pr", "close"], op: "destroy", kind: "pr", id: { from: "operand", at: 0 }, values: ["-c", "--comment", "-R", "--repo"], fields: { "-R": "repo", "--repo": "repo" } },
  { programs: ["gh"], sub: ["release", "create"], op: "create", kind: "release", id: { from: "output", pattern: new RegExp(`^${GITHUB_HOSTED}(?<repo>[\\w.-]+/[\\w.-]+)/releases/tag/(?<name>\\S+)$`) }, values: ["-t", "--title", "-n", "--notes", "-F", "--notes-file", "--target", "-R", "--repo", "--notes-start-tag", "--discussion-category"] },
  { programs: ["gh"], sub: ["release", "delete"], op: "destroy", kind: "release", id: { from: "operand", at: 0 }, values: ["-R", "--repo"], fields: { "-R": "repo", "--repo": "repo" } },
  // Simulators.
  { programs: ["xcrun"], sub: ["simctl", "create"], op: "create", kind: "simulator", id: { from: "output", pattern: UUID_LINE } },
  { programs: ["xcrun"], sub: ["simctl", "clone"], op: "create", kind: "simulator", id: { from: "output", pattern: UUID_LINE } },
  { programs: ["xcrun"], sub: ["simctl", "delete"], op: "destroy", kind: "simulator", id: { from: "operands" } },
  { programs: ["xcrun"], sub: ["simctl", "erase"], op: "destroy", kind: "simulator", id: { from: "operands" } },
  { programs: ["xcrun"], sub: ["simctl", "boot"], op: "start", kind: "simulator", id: { from: "operands" } },
  { programs: ["xcrun"], sub: ["simctl", "shutdown"], op: "stop", kind: "simulator", id: { from: "operands" } },
  // Containers.
  ...["docker", "podman"].flatMap((program): CatalogueRow[] => [
    ...[["run"], ["container", "run"]].map((sub): CatalogueRow => ({ programs: [program], sub, op: "create", kind: "container", id: { from: "output", pattern: CONTAINER_LINE }, values: DOCKER_RUN_VALUES, requires: ["-d", "--detach"] })),
    ...[["create"], ["container", "create"]].map((sub): CatalogueRow => ({ programs: [program], sub, op: "create", kind: "container", id: { from: "output", pattern: CONTAINER_LINE }, values: DOCKER_RUN_VALUES })),
    ...[["run"], ["create"], ["container", "run"], ["container", "create"]].map((sub): CatalogueRow => ({ programs: [program], sub, op: "create", kind: "container", id: { from: "option", names: ["--name"] }, values: DOCKER_RUN_VALUES, forbids: ["--rm"] })),
    ...[["start"], ["container", "start"]].map((sub): CatalogueRow => ({ programs: [program], sub, op: "start", kind: "container", id: { from: "operands" } })),
    ...[["rm"], ["container", "rm"]].map((sub): CatalogueRow => ({ programs: [program], sub, op: "destroy", kind: "container", id: { from: "operands" }, values: ["-t", "--time"] })),
    ...[["stop"], ["kill"], ["container", "stop"], ["container", "kill"]].map((sub): CatalogueRow => ({ programs: [program], sub, op: "stop", kind: "container", id: { from: "operands" }, values: ["-t", "--time", "-s", "--signal"] })),
  ]),
  // Packages.
  { programs: ["npm"], sub: ["publish"], op: "create", kind: "package", id: { from: "output", pattern: /^\+ (?<name>@?[^@\s]+)@(?<version>\S+)$/ }, values: ["--tag", "--access", "--otp"] },
  { programs: ["npm"], sub: ["unpublish"], op: "destroy", kind: "package", id: { from: "operand", at: 0 }, values: ["--otp"] },
  // Machines.
  { programs: ["multipass"], sub: ["launch"], op: "create", kind: "host", id: { from: "option", names: ["-n", "--name"] }, values: ["-n", "--name", "-c", "--cpus", "-m", "--memory", "-d", "--disk", "--cloud-init", "--network", "--mount", "--timeout"] },
  { programs: ["multipass"], sub: ["delete"], op: "destroy", kind: "host", id: { from: "operands" } },
  { programs: ["multipass"], sub: ["start"], op: "start", kind: "host", id: { from: "operands" } },
  { programs: ["multipass"], sub: ["stop"], op: "stop", kind: "host", id: { from: "operands" } },
  // Processes: this chat's are told by the marker its commands carry (core.ts `ownsProcesses`), not by the ledger.
  { programs: ["kill"], sub: [], op: "stop", kind: "process", id: { from: "operands" }, values: ["-s", "-n"] },
];

/**
 * Variables that name a dev instance's data directories when a command
 * carries them (`CYPHER_DEV_INSTANCE=probe scripts/dev-engine.sh`): the
 * directories the instance makes, as paths.
 */
export const ENV_CREATORS: readonly { name: RegExp; derive: "dev-instance" | "dir-and-log" }[] = [
  { name: /^CYPHER_DEV_INSTANCE$/, derive: "dev-instance" },
  { name: /^(CYPHER_DATA_DIR|CYPHER_ENGINE_DATA_DIR|CYPHER_CAPTURE_DIR)$/, derive: "dir-and-log" },
];

/** How each kind's creation is confirmed, when its name is not read from the output. */
const CONFIRM: Readonly<Record<LedgerKind, "observed" | "succeeded">> = {
  path: "observed", worktree: "observed", branch: "observed", tag: "observed",
  repo: "succeeded", pr: "succeeded", release: "succeeded", process: "succeeded", simulator: "succeeded", host: "succeeded", package: "succeeded", container: "succeeded", image: "succeeded", resource: "succeeded",
};

export type Confirm = "observed" | "succeeded" | "output" | "ran";

/** One command of a line, as core.ts reads it (shell.ts with `evaluate`). */
export interface CatalogueCommand {
  words: readonly string[];
  cwd: string | undefined;
  writes: readonly string[];
  env?: readonly string[];
  envValues?: readonly string[];
  carried?: readonly string[];
  joint?: Joint;
  topLevel?: boolean;
}

/** What the catalogue needs from the machine. */
export interface CatalogueFacts {
  home: string;
  /** A path as the permission engine canonicalizes it; undefined when that is not known. */
  canonical(path: string, cwd: string | undefined): string | undefined;
  /** Whether a path may ever be recorded: not sensitive, protected, the permission system's or the installed app's, not a whole home folder. */
  recordable(path: string): boolean;
  exists(path: string): boolean;
  /** The repository (its shared git directory) a directory is in. */
  repository?(dir: string | undefined): string | undefined;
  /** Whether a ref (`refs/heads/x`, `refs/tags/x`, `refs/remotes/origin/x`) exists in a repository. */
  refExists?(repo: string, ref: string): boolean;
  /** Whether a command's program is the system's (not one in a place the agent can write). */
  trusted?(program: string, cwd: string | undefined): boolean;
  /** Whether the ledger already holds an entity (a push needs the branch to be this chat's). */
  ledgered?(entity: LedgerEntity): boolean;
}

/** An entity a line may create. */
export interface Candidate {
  entity: LedgerEntity;
  /** What the command does to it, when not creating it (starting, merging). */
  action?: LedgerAction;
  /** Which command of the line creates it. */
  command: number;
  confirm: Confirm;
  /** For `output`: the shape of the line naming it, and the fields it fills. */
  pattern?: RegExp;
  /** It was there before the line ran: never the task's. */
  before?: boolean;
  /** Recorded only when the ledger holds this one (pushing a branch this chat made). */
  needs?: LedgerEntity;
  /** A ref to look for after the run (`observed` branches and tags). */
  ref?: { repo: string; ref: string };
}

/** A command read against the catalogue: its program, the directory git's `-C` moves it to, and the words after the global options. */
interface Read {
  program: string;
  dir: string | undefined;
  rest: readonly string[];
}

function readGlobals(command: CatalogueCommand): Read | undefined {
  const words = command.words;
  if (words.length === 0 || words[0] === UNKNOWN) return undefined;
  const program = basename(words[0]);
  const globals = GLOBALS[program];
  let dir = command.cwd;
  let i = 1;
  if (globals) {
    for (; i < words.length; i++) {
      const word = words[i];
      if (word === UNKNOWN) return undefined;
      if (!word.startsWith("-")) break;
      const name = word.includes("=") ? word.slice(0, word.indexOf("=")) : word;
      if (globals.foreign?.includes(name)) return undefined;
      if (globals.values.includes(word)) {
        const value = words[++i];
        if (word === globals.cwd) dir = value === undefined || value === UNKNOWN ? undefined : isAbsolute(value) ? value : dir === undefined ? undefined : resolve(dir, value);
      }
    }
  }
  return { program, dir, rest: words.slice(i) };
}

/** A command's words after its subcommand, split as the program reads them. */
interface Args {
  positionals: string[];
  options: Map<string, string | true>;
  /** Single-letter flags given together (`-dit`). */
  letters: Set<string>;
  /** Some word is known only when it runs. */
  unknown: boolean;
}

function splitArgs(words: readonly string[], values: readonly string[]): Args {
  const args: Args = { positionals: [], options: new Map(), letters: new Set(), unknown: false };
  let ended = false;
  for (let i = 0; i < words.length; i++) {
    const word = words[i];
    if (word === UNKNOWN) {
      args.unknown = true;
      args.positionals.push(UNKNOWN);
      continue;
    }
    if (ended || !word.startsWith("-") || word === "-") {
      args.positionals.push(word);
      continue;
    }
    if (word === "--") {
      ended = true;
      continue;
    }
    const eq = word.startsWith("--") ? word.indexOf("=") : -1;
    if (eq > 0) {
      args.options.set(word.slice(0, eq), word.slice(eq + 1));
      continue;
    }
    if (values.includes(word)) {
      const value = words[++i];
      if (value === undefined || value === UNKNOWN) args.unknown = true;
      args.options.set(word, value === undefined || value === UNKNOWN ? UNKNOWN : value);
      continue;
    }
    args.options.set(word, true);
    if (/^-[A-Za-z]+$/.test(word)) for (const letter of word.slice(1)) args.letters.add(letter);
  }
  return args;
}

function hasOption(args: Args, names: readonly string[]): boolean {
  return names.some((name) => args.options.has(name) || (/^-[A-Za-z]$/.test(name) && args.letters.has(name[1])));
}

/** The rows a command matches, with its arguments after each row's subcommand. */
export function rowsFor(command: CatalogueCommand): { row: CatalogueRow; args: Args; dir: string | undefined }[] {
  const read = readGlobals(command);
  if (!read) return [];
  const out: { row: CatalogueRow; args: Args; dir: string | undefined }[] = [];
  const named = CATALOGUE.filter((row) => row.programs.includes(read.program) && row.sub.every((part, index) => read.rest[index] === part));
  // Of a program's rows, only the longest subcommand path applies (`docker container rm`, not `docker container`); rows for any program, always.
  const longest = Math.max(-1, ...named.map((row) => row.sub.length));
  const rows = [...named.filter((row) => row.sub.length === longest), ...CATALOGUE.filter((row) => row.programs.includes("*"))];
  for (const row of rows) {
    // A row for any program reads all its words: the option may stand before a subcommand or after it.
    const args = splitArgs(row.programs.includes("*") ? command.words.slice(1) : read.rest.slice(row.sub.length), row.values ?? []);
    if (row.requires && !hasOption(args, row.requires)) continue;
    if (row.forbids && hasOption(args, row.forbids)) continue;
    out.push({ row, args, dir: read.dir });
  }
  return out;
}

/** Names a row's id source gives, from the arguments (none for `output`, which the run gives). */
function namesFrom(row: CatalogueRow, args: Args): string[] {
  const id = row.id;
  const known = (word: string | undefined): word is string => word !== undefined && word !== UNKNOWN && word !== "";
  switch (id.from) {
    case "operand": {
      const word = args.positionals[id.at];
      return known(word) ? [word] : [];
    }
    case "operands":
      return args.positionals.filter(known);
    case "last": {
      const word = args.positionals.length >= 2 ? args.positionals.at(-1) : undefined;
      // A remote destination (`host:path`) is no file here.
      return known(word) && !/^[^/]*:/.test(word) ? [word] : [];
    }
    case "option": {
      for (const name of id.names) {
        const value = args.options.get(name);
        if (typeof value === "string" && known(value)) return [value];
      }
      return [];
    }
    case "clone-target": {
      const [source, place] = args.positionals;
      if (known(place)) return [place];
      if (!known(source)) return [];
      const name = basename(source.replace(/\/+$/, "").replace(/\.git$/, "").replace(/^.*:/, ""));
      return name ? [name] : [];
    }
    case "output":
    case "refspecs":
      return [];
  }
}

const PATH_KINDS: ReadonlySet<LedgerKind> = new Set(["path", "worktree"]);

/** A dev instance's data directories: `~/.cypher-development/<mode>-<part>[-<name>]`. Never the whole folder: other chats keep theirs there. */
function devInstanceDirs(name: string, home: string): string[] {
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

/** A path-like entity, canonical, when it may be recorded at all. */
function pathEntity(kind: LedgerKind, word: string, cwd: string | undefined, facts: CatalogueFacts): LedgerEntity | undefined {
  if (word === UNKNOWN || word.startsWith("-")) return undefined;
  const path = facts.canonical(word, cwd);
  if (path === undefined || !facts.recordable(path)) return undefined;
  return { kind, name: path };
}

/** The refs a push names: what it creates on the remote (with the local ref it comes from) and what it deletes. */
function refspecs(args: Args): { remote: string; create: { name: string; source: string }[]; remove: string[] } | undefined {
  if (hasOption(args, ["--all", "--mirror", "--tags", "--follow-tags", "--prune"])) return undefined;
  const [remote, ...specs] = args.positionals;
  if (remote === undefined || remote === UNKNOWN || specs.includes(UNKNOWN)) return undefined;
  const strip = (ref: string) => ref.replace(/^\+/, "").replace(/^refs\/(heads|tags)\//, "");
  const deleting = hasOption(args, ["-d", "--delete"]);
  const create: { name: string; source: string }[] = [];
  const remove: string[] = [];
  for (const spec of specs) {
    if (deleting) remove.push(strip(spec));
    else if (spec.startsWith(":")) remove.push(strip(spec.slice(1)));
    else {
      const [source, target] = spec.includes(":") ? spec.split(":", 2) : [spec, spec];
      if (!source || !target || strip(source) === "HEAD" || source.includes("~") || source.includes("^")) continue;
      create.push({ name: strip(target), source: strip(source) });
    }
  }
  return { remote, create, remove };
}

/** Everything a line's commands may create, before it runs. */
export function planCreations(commands: readonly CatalogueCommand[], facts: CatalogueFacts): Candidate[] {
  const out: Candidate[] = [];
  const add = (candidate: Candidate) => {
    if (out.some((other) => other.action === candidate.action && JSON.stringify(other.entity) === JSON.stringify(candidate.entity))) return;
    out.push(candidate);
  };
  const observed = (entity: LedgerEntity, index: number, ref?: { repo: string; ref: string }): Candidate => {
    if (ref) return { entity, command: index, confirm: "observed", before: facts.refExists?.(ref.repo, ref.ref) !== false, ref };
    return { entity, command: index, confirm: "observed", before: facts.exists(entity.name) };
  };
  commands.forEach((command, index) => {
    // A redirection's file.
    for (const word of command.writes) {
      const entity = pathEntity("path", word, command.cwd, facts);
      if (entity) add(observed(entity, index));
    }
    // A dev instance names its own directories, whoever starts it.
    const assignments = (command.env ?? []).map((name, at) => `${name}=${command.envValues?.[at] ?? UNKNOWN}`);
    for (const word of [...command.words, ...(command.carried ?? []), ...assignments]) {
      const match = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(word);
      if (!match || match[2] === UNKNOWN || match[2].includes(UNKNOWN)) continue;
      const creator = ENV_CREATORS.find((item) => item.name.test(match[1]));
      if (!creator) continue;
      let dirs: string[];
      if (creator.derive === "dev-instance") {
        if (!/^[A-Za-z0-9._-]*$/.test(match[2])) continue;
        dirs = devInstanceDirs(match[2], facts.home);
      } else {
        const dir = facts.canonical(match[2], command.cwd);
        if (dir === undefined) continue;
        dirs = [dir, `${dir}.log`];
      }
      for (const dir of dirs) {
        if (!facts.recordable(dir)) continue;
        add({ entity: { kind: "path", name: dir }, command: index, confirm: "ran", before: facts.exists(dir) });
      }
    }
    for (const { row, args, dir } of rowsFor(command)) {
      if (row.op === "destroy" || row.op === "stop") continue;
      const action = row.op === "create" ? undefined : row.op;
      const repo = row.inRepo ? facts.repository?.(dir) : undefined;
      const fields: Partial<LedgerEntity> = {};
      for (const [option, field] of Object.entries(row.fields ?? {})) {
        const value = args.options.get(option);
        if (typeof value === "string" && value !== UNKNOWN) fields[field] = value;
      }
      if (row.id.from === "output") {
        add({ entity: { kind: row.kind, name: "", ...fields }, command: index, confirm: "output", pattern: row.id.pattern, ...(action ? { action } : {}) });
        continue;
      }
      if (row.id.from === "refspecs") {
        const push = refspecs(args);
        if (!push || repo === undefined) continue;
        for (const { name, source } of push.create) {
          // A push makes a remote branch this chat's only when the branch it pushes is.
          add({ entity: { kind: "branch", name, repo, remote: push.remote }, command: index, confirm: "succeeded", needs: { kind: "branch", name: source, repo }, before: facts.refExists?.(repo, `refs/remotes/${push.remote}/${name}`) !== false });
          add({ entity: { kind: "tag", name, repo, remote: push.remote }, command: index, confirm: "succeeded", needs: { kind: "tag", name: source, repo } });
        }
        continue;
      }
      for (const name of namesFrom(row, args)) {
        if (PATH_KINDS.has(row.kind) && CONFIRM[row.kind] === "observed") {
          const entity = pathEntity(row.kind, name, dir, facts);
          if (entity) add(observed(entity, index));
          continue;
        }
        if (row.inRepo && CONFIRM[row.kind] === "observed") {
          if (repo === undefined) continue;
          const entity: LedgerEntity = { kind: row.kind, name, repo, ...fields };
          add(observed(entity, index, { repo, ref: `refs/${row.kind === "tag" ? "tags" : "heads"}/${name}` }));
          continue;
        }
        // `observed` needs a path or a ref to look at; anything else is confirmed by the line's success.
        add({ entity: { kind: row.kind, name, ...fields }, command: index, confirm: "succeeded", ...(action ? { action } : {}) });
      }
    }
  });
  return out;
}

/** Whether every command of a line surely ran and succeeded, given its exit status. */
export function lineSucceeded(commands: readonly CatalogueCommand[], exitCode: number | undefined): boolean {
  if (exitCode !== 0 || commands.length === 0) return false;
  return commands.every((command, index) => command.topLevel === true && (index === commands.length - 1 ? command.joint === undefined : command.joint === "&&"));
}

/** The last non-empty line of a command's output. */
/** Commands that print nothing on stdout or stderr as written here (`export X=…`, `cd dir`, `mkdir -p dir`). */
const SILENT: ReadonlySet<string> = new Set(["export", "cd", "unset", "set", "true", ":", "mkdir", "sleep", "umask", "shopt"]);
/** Options that make a silent command print (`mkdir -v`, `set -x`, `set -o` alone lists options). */
const LOUD_OPTION = /^-(?:[a-z]*[vx][a-z]*|-verbose|-xtrace)$/i;

/**
 * Whether the line's output is the creator's own (`at`): every other command
 * prints nothing (an assignment, `export`, `cd`, `mkdir -p`), or takes the
 * creator's output through a pipe and only selects lines of it (`| tail -1`,
 * `| head -n 3`). An `echo`, a `printf`, a `sed` or any other command that
 * prints or rewrites may forge the name, so the output is then nobody's.
 */
export function loneOutput(commands: readonly CatalogueCommand[], at: number): boolean {
  let piped = false;
  for (const [index, command] of commands.entries()) {
    if (index === at) {
      piped = command.joint === "|";
      continue;
    }
    const program = command.words[0];
    if (index > at && piped) {
      // A selector of the creator's lines: `tail`/`head` with a line count and no file.
      const rest = command.words.slice(1);
      const selects = (program === "tail" || program === "head") && rest.every((word, i) => /^-(?:n)?\d+$/.test(word) || (word === "-n" && /^\d+$/.test(rest[i + 1] ?? "")) || (/^\d+$/.test(word) && rest[i - 1] === "-n"));
      if (!selects || command.writes.length) return false;
      piped = command.joint === "|";
      continue;
    }
    piped = false;
    // Its stdout or stderr redirected elsewhere doesn't make a command silent: only what it never prints.
    if (program === undefined) {
      if (command.joint === "|") return false;
      continue;
    }
    if (program === UNKNOWN || !SILENT.has(program) || command.words.slice(1).some((word) => word === UNKNOWN || LOUD_OPTION.test(word)) || command.joint === "|") return false;
    if ((program === "set" && command.words.length === 1) || (program === "cd" && command.words[1] === "-")) return false;
  }
  return true;
}

function lastLine(output: string): string | undefined {
  const lines = output.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  return lines.at(-1);
}

/** Which planned entities the line made, after it ran: its exit status and its output (the creator's own, when it is the only command). */
export function settleCreations(
  plan: readonly Candidate[],
  commands: readonly CatalogueCommand[],
  result: { exitCode?: number; output?: string },
  facts: CatalogueFacts,
): (LedgerEntity & { action?: LedgerAction })[] {
  const succeeded = lineSucceeded(commands, result.exitCode);
  const trusted = (index: number) => {
    const command = commands[index];
    return command !== undefined && command.words[0] !== undefined && command.words[0] !== UNKNOWN && facts.trusted?.(command.words[0], command.cwd) === true;
  };
  const out: (LedgerEntity & { action?: LedgerAction })[] = [];
  for (const candidate of plan) {
    if (candidate.before) continue;
    let entity: LedgerEntity | undefined = candidate.entity;
    switch (candidate.confirm) {
      case "ran":
        break;
      case "observed":
        if (candidate.ref) {
          if (facts.refExists?.(candidate.ref.repo, candidate.ref.ref) !== true) entity = undefined;
        } else if (!facts.exists(candidate.entity.name)) entity = undefined;
        break;
      case "succeeded":
        if (!succeeded || !trusted(candidate.command)) entity = undefined;
        break;
      case "output": {
        // Only the creator's own output: the line's other commands print nothing or only pick its lines
        // (`loneOutput`), and the line exited 0 (through `| tail -1` that is tail's status: a creator that
        // failed prints no line its pattern takes).
        if (result.exitCode !== 0 || !(succeeded || commands.length > 1) || !loneOutput(commands, candidate.command) || !trusted(candidate.command) || result.output === undefined || !candidate.pattern) {
          entity = undefined;
          break;
        }
        const line = lastLine(result.output);
        const match = line === undefined ? null : candidate.pattern.exec(line);
        const name = match?.groups?.name;
        if (!match || !name) {
          entity = undefined;
          break;
        }
        entity = { ...candidate.entity, name, ...(match.groups?.repo ? { repo: match.groups.repo } : {}), ...(match.groups?.version ? { version: match.groups.version } : {}) };
        if (entity.kind === "path") {
          const path = facts.canonical(name, commands[candidate.command].cwd);
          entity = path !== undefined && facts.recordable(path) && facts.exists(path) ? { kind: "path", name: path } : undefined;
        }
        break;
      }
    }
    if (entity && candidate.needs && facts.ledgered?.(candidate.needs) !== true) entity = undefined;
    const done = entity && candidate.action ? { ...entity, action: candidate.action } : entity;
    if (done && !out.some((other) => JSON.stringify(other) === JSON.stringify(done))) out.push(done);
  }
  return out;
}

/**
 * Kinds whose ownership is also told by where they are, so a destroyer of one
 * not in the ledger is not a danger by that alone (risk.ts decides by place):
 * every other kind is somebody's unless the ledger, a lifecycle follow-up or
 * the owner's consent says it is this task's.
 */
export const TOLD_BY_PLACE: Readonly<Partial<Record<LedgerKind, string>>> = {
  path: "the chat's project folders and scratch are its own (risk.ts `disposable`)",
  worktree: "a worktree inside the chat's workspace is its own",
  branch: "the branch checked out in the chat's workspace is its work; another checkout's is a danger already",
  tag: "a tag in the chat's own checkout is local; pushing or deleting one remotely is a danger already",
  process: "this chat's processes carry its marker (core.ts `ownsProcesses`)",
};

/**
 * Subcommand verbs that destroy or stop something, for programs no row
 * covers (`docker network rm`, `limactl delete`, `VBoxManage unregistervm`):
 * what such a command destroys has an owner Cypher can't tell (risk.ts).
 */
export const DESTROY_VERBS =
  /^(rm|rmi|rmdir|del|delete|remove|destroy|erase|prune|purge|unregister\w*|deregister|uninstall|kill|stop|drop|wipe|nuke|terminate|teardown|unpublish|yank|deprovision|decommission)$/i;

/**
 * Programs whose destroy verbs act on the project at hand (its dependencies,
 * its tasks) or that risk.ts reads by itself: no generic reading for them.
 */
export const PROJECT_VERBS: Readonly<Record<string, RegExp>> = {
  git: /.*/,
  npm: /^(uninstall|remove|rm|r|un|unlink|prune)$/,
  pnpm: /^(remove|rm|uninstall|un|prune)$/,
  yarn: /^(remove)$/,
  bun: /^(remove|rm)$/,
  cargo: /^(remove|rm)$/,
  uv: /^(remove)$/,
  poetry: /^(remove)$/,
  make: /.*/, just: /.*/, task: /.*/, rake: /.*/, ninja: /.*/,
};

/** A destroy verb as one of a command's first two words (after options), and the operands after it. */
export function genericDestroyer(words: readonly string[]): { verb: string; operands: string[] } | undefined {
  const program = basename(words[0] ?? "");
  if (!program || program === UNKNOWN) return undefined;
  const positionals: { word: string; at: number }[] = [];
  for (let i = 1; i < words.length && positionals.length < 2; i++) {
    const word = words[i];
    if (word === UNKNOWN) break;
    if (word.startsWith("-")) continue;
    positionals.push({ word, at: i });
  }
  for (const [index, { word, at }] of positionals.entries()) {
    // The second word only after a plain group name (`docker network rm`), never after a file or a script.
    if (index === 1 && !/^[a-z][a-z0-9-]*$/i.test(positionals[0].word)) break;
    if (!DESTROY_VERBS.test(word)) continue;
    if (PROJECT_VERBS[program]?.test(word)) return undefined;
    return { verb: word, operands: words.slice(at + 1).filter((item) => item === UNKNOWN || !item.startsWith("-")) };
  }
  return undefined;
}

/** What a command deletes or stops: per target, the entities it may be (any one of them). */
export interface Destruction {
  op: "destroy" | "stop";
  /** The kinds of entity its rows destroy or stop. */
  kinds: LedgerKind[];
  targets: LedgerEntity[][];
  /** Some target is known only when it runs. */
  unknown: boolean;
}

/** The entities a command destroys or stops, as the catalogue reads it; undefined when no row covers it. */
export function targetsOf(command: CatalogueCommand, facts: Pick<CatalogueFacts, "canonical" | "repository">): Destruction | undefined {
  let found: Destruction | undefined;
  for (const { row, args, dir } of rowsFor(command)) {
    const op = row.op === "destroy" || row.op === "stop" ? row.op : row.op === "create" && row.id.from === "refspecs" ? "destroy" : undefined;
    if (op === undefined) continue;
    found ??= { op, kinds: [], targets: [], unknown: false };
    if (!found.kinds.includes(row.kind)) found.kinds.push(row.kind);
    if (args.unknown) found.unknown = true;
    if (row.id.from === "refspecs") {
      const push = refspecs(args);
      const repo = facts.repository?.(dir);
      if (!push) continue;
      for (const name of push.remove) found.targets.push([{ kind: "branch", name, remote: push.remote, ...(repo ? { repo } : {}) }, { kind: "tag", name, remote: push.remote, ...(repo ? { repo } : {}) }]);
      if (!push.remove.length) found = undefined;
      continue;
    }
    const repo = row.inRepo ? facts.repository?.(dir) : undefined;
    const fields: Partial<LedgerEntity> = {};
    for (const [option, field] of Object.entries(row.fields ?? {})) {
      const value = args.options.get(option);
      if (typeof value === "string" && value !== UNKNOWN) fields[field] = value;
    }
    const names = namesFrom(row, args);
    if (names.length === 0) found.unknown = true;
    for (const name of names) {
      if (PATH_KINDS.has(row.kind)) {
        const path = facts.canonical(name, dir);
        if (path === undefined) found.unknown = true;
        // A worktree is a path too: `rm -rf` of one this chat added is its own.
        else found.targets.push(row.kind === "path" ? [{ kind: "path", name: path }, { kind: "worktree", name: path }] : [{ kind: "worktree", name: path }, { kind: "path", name: path }]);
        continue;
      }
      found.targets.push([{ kind: row.kind, name, ...(repo ? { repo } : {}), ...fields }]);
    }
  }
  return found;
}
