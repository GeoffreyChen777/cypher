/**
 * Which entities a command acts on, from a table of tools: per program and
 * subcommand, what it does (the action) and which operands and options
 * carry which kind of entity (entities.ts). The explicit-request check
 * (explicit.ts) reads every dangerous command through this table; a
 * command no row covers can't be checked against the owner's words, so it
 * asks.
 *
 * A row says it as data. The few shapes a table can't say (a push's
 * refspecs, the package a manifest names, a host inside `user@host:path`
 * or a URL) are named `reader`s, read in explicit.ts where the facts they
 * need are; a new tool is a new row, not new code.
 */

import { basename } from "node:path";

import type { Entity, EntityKind } from "./entities.ts";
import { UNKNOWN } from "./shell.ts";

export type Action =
  | "create"
  | "delete"
  | "merge"
  | "close"
  | "edit"
  | "comment"
  | "review"
  | "ready"
  | "reopen"
  | "push"
  | "publish"
  | "unpublish"
  | "release"
  | "install"
  | "reach"
  | "send"
  | "stop"
  | "erase"
  | "apply"
  | "rename"
  | "archive";

/** An entity field an option sets: on the row's entities, or a new entity of its own. */
type OptionSpec = { field: "repo" | "head" | "base" | "scope" | "version" } | { kind: EntityKind };

export interface ToolRow {
  /** Program names (basename of the first word). */
  programs: readonly string[];
  /** The subcommand path after the program (`["pr", "merge"]`); aliases are rows of their own. */
  sub: readonly string[];
  action: Action;
  /** What the operands after `sub` name, in order. */
  operands?: readonly EntityKind[];
  /** The last operand kind repeats for every further operand. */
  rest?: boolean;
  /** Some operand must be there; without one the command acts on something the line doesn't name. */
  needs?: string;
  /** Operands to skip before the entities (`kubectl delete deployment x`: the type). */
  typed?: boolean;
  /** Options that carry entities or their fields. */
  options?: Readonly<Record<string, OptionSpec>>;
  /** With no operand, the entity it acts on is implied (`gh pr create`: a new pull request). */
  implicit?: EntityKind;
  /** A shape read by explicit.ts. */
  reader?: "refspecs" | "manifest" | "ssh" | "copy" | "urls";
  /** Only the entity operands and options must be known before it runs (titles and bodies may not). */
  loose?: boolean;
  /** The row applies only when one of these options is given (`git branch -D`, not `git branch new`). */
  when?: readonly string[];
}

/** Per program: the options that take a value (so it is no operand). */
const VALUE_OPTIONS: Readonly<Record<string, readonly string[]>> = {
  gh: [
    "-R", "--repo", "-B", "--base", "-H", "--head", "-t", "--title", "-b", "--body", "-F", "--body-file", "-a", "--assignee", "-l", "--label",
    "-m", "--milestone", "-r", "--reviewer", "-p", "--project", "-T", "--template", "--subject", "-A", "--author-email", "--match-head-commit",
    "-n", "--notes", "--notes-file", "--target", "--notes-start-tag", "--discussion-category", "-c", "--comment", "--add-label", "--remove-label",
    "--add-reviewer", "--remove-reviewer", "--add-assignee", "--remove-assignee", "--add-project", "--remove-project", "--hostname", "-q", "--jq",
    "--json", "-e", "-d", "--description", "-h", "--homepage", "--source", "--remote", "-g", "--gitignore", "--license",
  ],
  glab: ["-R", "--repo", "-b", "--target-branch", "-s", "--source-branch", "-H", "--head", "--target-project", "-r", "--reviewer", "-t", "--title", "-d", "--description", "-l", "--label", "-a", "--assignee", "-m", "--milestone", "-n", "--name", "-N", "--notes", "-r", "--ref"],
  docker: ["-f", "--filter", "--format", "-t", "--time", "-s", "--signal", "--context", "-H", "--host", "--platform"],
  podman: ["-f", "--filter", "--format", "-t", "--time", "-s", "--signal", "--connection", "--platform"],
  kubectl: ["-n", "--namespace", "--context", "--cluster", "-f", "--filename", "-l", "--selector", "-o", "--output", "--replicas", "--grace-period", "--timeout", "-c", "--container", "--kubeconfig"],
  helm: ["-n", "--namespace", "--kube-context", "-f", "--values", "--set", "--set-string", "--version", "--timeout", "-o", "--output", "--kubeconfig", "--repo"],
  xcrun: [],
  npm: ["--tag", "--access", "--otp", "--registry", "-w", "--workspace"],
  pnpm: ["--tag", "--access", "--otp", "--registry", "--filter", "-F"],
  yarn: ["--tag", "--access", "--otp", "--registry"],
  bun: ["--tag", "--access", "--otp", "--registry"],
  cargo: ["--registry", "--token", "-p", "--package", "--manifest-path", "--index", "--vers", "--version", "--features", "-F", "--target", "-j", "--jobs"],
};

const PR_OPTIONS = { "-R": { field: "repo" }, "--repo": { field: "repo" } } as const;
const GH_PR_ACTIONS: readonly Action[] = ["merge", "close", "comment", "review", "edit", "ready", "reopen"];
const INSTALLERS = ["brew", "port", "pipx", "gem", "apt", "apt-get", "dnf", "yum", "pacman", "snap"];

/** The table. Rows of one program are tried longest `sub` first. */
export const TOOLS: readonly ToolRow[] = [
  // GitHub.
  { programs: ["gh"], sub: ["pr", "create"], action: "create", implicit: "pr", loose: true, options: { ...PR_OPTIONS, "-B": { field: "base" }, "--base": { field: "base" }, "-H": { field: "head" }, "--head": { field: "head" } } },
  ...GH_PR_ACTIONS.map((action): ToolRow => ({ programs: ["gh"], sub: ["pr", action], action, operands: ["pr"], loose: true, needs: "it acts on the current branch's pull request, whose number the line doesn't name", options: PR_OPTIONS })),
  ...(["create", "edit", "upload"] as const).map((sub): ToolRow => ({ programs: ["gh"], sub: ["release", sub], action: "release", operands: ["release"], loose: true, needs: "the release's tag is known only when it runs", options: PR_OPTIONS })),
  { programs: ["gh"], sub: ["release", "delete"], action: "delete", operands: ["release"], loose: true, needs: "the release's tag is known only when it runs", options: PR_OPTIONS },
  { programs: ["gh"], sub: ["repo", "create"], action: "create", operands: ["repo"], loose: true, needs: "the new repository's name is known only when it runs" },
  { programs: ["gh"], sub: ["repo", "delete"], action: "delete", operands: ["repo"], loose: true, needs: "it deletes the current repository, which the line doesn't name" },
  { programs: ["gh"], sub: ["repo", "edit"], action: "edit", operands: ["repo"], loose: true, needs: "it edits the current repository, which the line doesn't name" },
  { programs: ["gh"], sub: ["repo", "rename"], action: "rename", operands: ["repo"], loose: true, options: PR_OPTIONS, needs: "it renames the current repository, which the line doesn't name" },
  { programs: ["gh"], sub: ["repo", "archive"], action: "archive", operands: ["repo"], loose: true, options: PR_OPTIONS, needs: "it archives the current repository, which the line doesn't name" },
  // GitLab.
  { programs: ["glab"], sub: ["mr", "create"], action: "create", implicit: "pr", loose: true, options: { ...PR_OPTIONS, "-b": { field: "base" }, "--target-branch": { field: "base" }, "-s": { field: "head" }, "--source-branch": { field: "head" }, "-H": { kind: "repo" }, "--head": { kind: "repo" } } },
  ...(["merge", "close", "note", "approve", "update", "reopen"] as const).map((sub): ToolRow => ({
    programs: ["glab"], sub: ["mr", sub], action: sub === "note" ? "comment" : sub === "approve" ? "review" : sub === "update" ? "edit" : (sub as Action), operands: ["pr"], loose: true, needs: "it acts on the current branch's merge request, whose number the line doesn't name", options: PR_OPTIONS,
  })),
  { programs: ["glab"], sub: ["release", "create"], action: "release", operands: ["release"], loose: true, needs: "the release's tag is known only when it runs", options: PR_OPTIONS },
  // git: a push's refspecs need git's facts.
  { programs: ["git"], sub: ["push"], action: "push", reader: "refspecs" },
  // Registries.
  { programs: ["npm", "pnpm", "yarn", "bun"], sub: ["publish"], action: "publish", reader: "manifest" },
  { programs: ["cargo"], sub: ["publish"], action: "publish", reader: "manifest" },
  { programs: ["npm"], sub: ["unpublish"], action: "unpublish", operands: ["package"], needs: "it unpublishes the package here, which the line doesn't name" },
  { programs: ["npm"], sub: ["deprecate"], action: "edit", operands: ["package"], needs: "the package is known only when it runs" },
  { programs: ["cargo"], sub: ["yank"], action: "unpublish", operands: ["package"], options: { "--vers": { field: "version" }, "--version": { field: "version" } }, needs: "it yanks the package here, which the line doesn't name" },
  // Installs for the whole machine.
  ...INSTALLERS.flatMap((program) => ["install", "reinstall", "upgrade"].map((sub): ToolRow => ({ programs: [program], sub: [sub], action: "install", operands: ["package"], rest: true, needs: "Cypher can't tell which packages it installs" }))),
  ...["npm", "pnpm", "bun"].flatMap((program) => ["i", "install", "add"].map((sub): ToolRow => ({ programs: [program], sub: [sub], action: "install", operands: ["package"], rest: true, needs: "Cypher can't tell which packages it installs" }))),
  { programs: ["yarn"], sub: ["global", "add"], action: "install", operands: ["package"], rest: true, needs: "Cypher can't tell which packages it installs" },
  { programs: ["cargo", "go"], sub: ["install"], action: "install", operands: ["package"], rest: true, needs: "Cypher can't tell which packages it installs" },
  { programs: ["uv"], sub: ["tool", "install"], action: "install", operands: ["package"], rest: true, needs: "Cypher can't tell which packages it installs" },
  { programs: ["pip", "pip3"], sub: ["install"], action: "install", operands: ["package"], rest: true, needs: "Cypher can't tell which packages it installs" },
  // Containers.
  ...["docker", "podman"].flatMap((program): ToolRow[] => [
    { programs: [program], sub: ["push"], action: "push", operands: ["image"], needs: "the image is known only when it runs" },
    { programs: [program], sub: ["image", "push"], action: "push", operands: ["image"], needs: "the image is known only when it runs" },
    { programs: [program], sub: ["rmi"], action: "delete", operands: ["image"], rest: true, needs: "Cypher can't tell which images it deletes" },
    { programs: [program], sub: ["image", "rm"], action: "delete", operands: ["image"], rest: true, needs: "Cypher can't tell which images it deletes" },
    { programs: [program], sub: ["rm"], action: "delete", operands: ["container"], rest: true, needs: "Cypher can't tell which containers it deletes" },
    { programs: [program], sub: ["container", "rm"], action: "delete", operands: ["container"], rest: true, needs: "Cypher can't tell which containers it deletes" },
    { programs: [program], sub: ["stop"], action: "stop", operands: ["container"], rest: true, needs: "Cypher can't tell which containers it stops" },
    { programs: [program], sub: ["kill"], action: "stop", operands: ["container"], rest: true, needs: "Cypher can't tell which containers it stops" },
    { programs: [program], sub: ["container", "stop"], action: "stop", operands: ["container"], rest: true, needs: "Cypher can't tell which containers it stops" },
    { programs: [program], sub: ["container", "kill"], action: "stop", operands: ["container"], rest: true, needs: "Cypher can't tell which containers it stops" },
  ]),
  // Simulators.
  ...(["delete", "erase", "shutdown"] as const).map((sub): ToolRow => ({
    programs: ["xcrun"], sub: ["simctl", sub], action: sub === "delete" ? "delete" : sub === "erase" ? "erase" : "stop", operands: ["simulator"], rest: true, needs: "Cypher can't tell which simulators it acts on",
  })),
  // Clusters.
  ...(["delete", "scale", "rollout"] as const).map((sub): ToolRow => ({
    programs: ["kubectl"], sub: [sub], action: sub === "delete" ? "delete" : "apply", operands: ["resource"], rest: true, typed: true, needs: "Cypher can't tell which resources it acts on",
    options: { "-n": { field: "scope" }, "--namespace": { field: "scope" } },
  })),
  ...(["install", "upgrade", "uninstall", "rollback"] as const).map((sub): ToolRow => ({
    programs: ["helm"], sub: [sub], action: sub === "uninstall" ? "delete" : "apply", operands: ["resource"], needs: "Cypher can't tell which release it acts on",
    options: { "-n": { field: "scope" }, "--namespace": { field: "scope" } },
  })),
  // Checkouts, branches and tags.
  { programs: ["git"], sub: ["worktree", "remove"], action: "delete", operands: ["worktree"], needs: "Cypher can't tell which worktree it removes" },
  { programs: ["git"], sub: ["branch"], when: ["-d", "-D", "--delete"], action: "delete", operands: ["branch"], rest: true, needs: "Cypher can't tell which branches it deletes" },
  { programs: ["git"], sub: ["tag"], when: ["-d", "--delete"], action: "delete", operands: ["tag"], rest: true, needs: "Cypher can't tell which tags it deletes" },
  // Files and processes. What this task made or started is local work before
  // this table is read (provenance: catalogue.ts, risk.ts `facts.created`, the
  // process marker); the rest is a danger the owner has to name.
  { programs: ["rm", "rmdir", "trash", "unlink", "shred"], sub: [], action: "delete", operands: ["path"], rest: true, needs: "Cypher can't tell which files it deletes" },
  { programs: ["kill"], sub: [], action: "stop", operands: ["process"], rest: true, needs: "Cypher can't tell which processes it stops" },
  { programs: ["pkill", "killall"], sub: [], action: "stop", operands: ["process"], rest: true, needs: "Cypher can't tell which processes it stops" },
  // Machines.
  ...(["delete", "stop"] as const).map((sub): ToolRow => ({ programs: ["multipass"], sub: [sub], action: sub, operands: ["host"], rest: true, needs: "Cypher can't tell which machines it acts on" })),
  // Other hosts.
  { programs: ["ssh", "mosh", "sftp"], sub: [], action: "reach", reader: "ssh" },
  { programs: ["scp", "rsync"], sub: [], action: "reach", reader: "copy" },
  { programs: ["curl", "wget", "http", "https", "xh"], sub: [], action: "send", reader: "urls" },
];

/** What a dialog calls an entity of each kind. */
const NOUNS: Readonly<Record<EntityKind, string>> = {
  repo: "repository", remote: "remote", branch: "branch", pr: "pull request", tag: "tag", release: "release's tag", path: "path", worktree: "worktree",
  process: "process", simulator: "simulator", host: "host", package: "package", container: "container", image: "image", resource: "resource",
};

/** The words of a command split as its program reads them. */
export interface ToolArgs {
  positionals: string[];
  options: Map<string, string>;
}

/** Operands and options, given which options take a value. */
export function splitArgs(args: readonly string[], values: readonly string[] = []): ToolArgs {
  const takes = new Set(values);
  const positionals: string[] = [];
  const options = new Map<string, string>();
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (word === "--") {
      positionals.push(...args.slice(i + 1));
      break;
    }
    if (word !== UNKNOWN && word.startsWith("-") && word !== "-") {
      const eq = word.indexOf("=");
      if (word.startsWith("--") && eq > 0) options.set(word.slice(0, eq), word.slice(eq + 1));
      else if (takes.has(word)) options.set(word, args[++i] ?? UNKNOWN);
      else options.set(word, "");
      continue;
    }
    positionals.push(word);
  }
  return { positionals, options };
}

/** What one command does, by the table. */
export interface ToolRead {
  row: ToolRow;
  program: string;
  action: Action;
  /** The entities its operands and options name (empty for a reader row: explicit.ts reads them). */
  entities: Entity[];
  args: ToolArgs;
}

const ROWS_BY_PROGRAM = new Map<string, ToolRow[]>();
for (const row of TOOLS) {
  for (const program of row.programs) {
    const rows = ROWS_BY_PROGRAM.get(program) ?? [];
    rows.push(row);
    ROWS_BY_PROGRAM.set(program, rows);
  }
}
for (const rows of ROWS_BY_PROGRAM.values()) rows.sort((a, b) => b.sub.length - a.sub.length);

/** The programs the table knows. */
export function knownProgram(program: string): boolean {
  return ROWS_BY_PROGRAM.has(program);
}

/**
 * What a command acts on, by the table: undefined when no row covers it,
 * `{unknown}` when a row does but an entity is known only when it runs.
 */
export function readTool(words: readonly string[]): ToolRead | { unknown: string } | undefined {
  const program = basename(words[0] ?? "");
  const rows = ROWS_BY_PROGRAM.get(program);
  if (!rows) return undefined;
  const args = splitArgs(words.slice(1), VALUE_OPTIONS[program] ?? []);
  const given = (names: readonly string[]) => names.some((name) => args.options.has(name) || (/^-[A-Za-z]$/.test(name) && [...args.options.keys()].some((option) => /^-[A-Za-z]+$/.test(option) && option.includes(name[1]))));
  const row = rows.find((candidate) => candidate.sub.every((word, index) => args.positionals[index] === word) && (!candidate.when || given(candidate.when)));
  if (!row) {
    if (args.positionals.slice(0, 2).includes(UNKNOWN)) return { unknown: `what \`${program}\` does is known only when it runs` };
    return undefined;
  }
  if (!row.loose && !row.reader && words.includes(UNKNOWN)) return { unknown: `\`${program}\` is given words known only when it runs` };
  const read: ToolRead = { row, program, action: row.action, entities: [], args };
  if (row.reader) return read;
  let operands = args.positionals.slice(row.sub.length);
  let type: string | undefined;
  if (row.typed && operands[0] !== undefined) {
    // `kubectl delete deployment web`, `kubectl delete deployment/web`.
    if (operands[0].includes("/")) type = undefined;
    else [type, ...operands] = operands;
  }
  const kinds = row.operands ?? [];
  if (kinds.length && operands.length === 0 && !row.implicit) return { unknown: row.needs ?? `Cypher can't tell what \`${program} ${row.sub.join(" ")}\` acts on` };
  for (const [index, word] of operands.entries()) {
    const kind = index < kinds.length ? kinds[index] : row.rest ? kinds.at(-1) : undefined;
    if (!kind) break;
    if (word === UNKNOWN) return { unknown: `the ${NOUNS[kind]} is known only when it runs` };
    let name = word;
    let entityType = type;
    if (kind === "resource" && row.typed && word.includes("/")) [entityType, name] = [word.slice(0, word.indexOf("/")), word.slice(word.indexOf("/") + 1)];
    if (kind === "resource" && program === "helm") entityType = "helm-release";
    read.entities.push({ kind, name, ...(entityType ? { type: entityType } : {}) });
  }
  if (read.entities.length === 0 && row.implicit) read.entities.push({ kind: row.implicit, name: "" });
  for (const [option, value] of args.options) {
    const spec = row.options?.[option];
    if (!spec) continue;
    if (value === UNKNOWN) return { unknown: `\`${option}\` is known only when it runs` };
    if ("kind" in spec) read.entities.push({ kind: spec.kind, name: value });
    else for (const entity of read.entities) entity[spec.field] = value;
  }
  return read;
}

/**
 * What a command run on another machine changes there (data): `ssh host 'sudo rm -rf /var/log/*'` deletes,
 * `systemctl restart` applies, `tee`/`>` writes. Naming the host consents to reaching it, not to this
 * (explicit.ts `uncovered`, host case). Rows: the action (lexicon.ts key) and the programs, optionally only
 * with one of the subcommand words. Strongest first: a line that deletes and writes is a delete.
 */
export const REMOTE_EFFECTS: readonly { action: string; programs: readonly string[]; subs?: RegExp; flags?: RegExp }[] = [
  { action: "delete", programs: ["rm", "rmdir", "unlink", "shred", "truncate", "wipefs", "srm", "trash"] },
  { action: "delete", programs: ["find"], flags: /^-delete$/ },
  { action: "delete", programs: ["journalctl"], flags: /^--vacuum/ },
  { action: "delete", programs: ["docker", "podman", "crictl"], subs: /^(rm|rmi|prune)$/ },
  { action: "delete", programs: ["apt", "apt-get", "yum", "dnf", "pacman", "brew", "pip", "pip3", "npm"], subs: /^(remove|purge|autoremove|uninstall|erase|-R.*)$/ },
  { action: "delete", programs: ["git"], subs: /^(clean|reset)$/ },
  { action: "delete", programs: ["dd", "mkfs", "mkfs.ext4", "mkfs.xfs", "fdisk", "parted"] },
  { action: "stop", programs: ["kill", "pkill", "killall", "reboot", "shutdown", "halt", "poweroff"] },
  { action: "stop", programs: ["systemctl", "service", "supervisorctl", "pm2", "docker", "podman", "launchctl"], subs: /^(stop|kill|disable|mask|unload|bootout|down)$/ },
  { action: "apply", programs: ["systemctl", "service", "supervisorctl", "pm2", "docker", "podman", "nginx", "launchctl", "kubectl", "helm"], subs: /^(restart|reload|start|enable|up|apply|rollout|upgrade|install|scale)$/ },
  { action: "apply", programs: ["apt", "apt-get", "yum", "dnf", "pacman", "brew", "pip", "pip3", "npm", "cargo", "snap"], subs: /^(install|upgrade|update|-S.*)$/ },
  { action: "write", programs: ["mv", "cp", "tee", "chmod", "chown", "chgrp", "ln", "touch", "mkdir", "install", "rsync", "scp", "crontab", "patch", "useradd", "usermod", "passwd", "iptables", "ufw"] },
  { action: "write", programs: ["sed", "perl"], flags: /^-[A-Za-z]*i/ },
];

const REMOTE_ORDER = ["delete", "stop", "apply", "write"];

/** Wrappers that run the next word as the command (`sudo -u www rm …`). */
const REMOTE_WRAPPERS = /^(sudo|doas|env|nohup|nice|ionice|timeout|time|command|exec|stdbuf|xargs)$/;

/** What a remote command (the words after the host) changes, or undefined when it only reads or can't be told. */
export function remoteEffect(words: readonly string[]): { action: string; says: string } | undefined {
  const script = words.filter((word) => word !== UNKNOWN).join(" ");
  if (!script.trim()) return undefined;
  let best: { action: string; says: string } | undefined;
  const consider = (action: string, says: string) => {
    if (!best || REMOTE_ORDER.indexOf(action) < REMOTE_ORDER.indexOf(best.action)) best = { action, says };
  };
  // A redirect into a file (not a descriptor, not /dev/null) writes it.
  for (const match of script.matchAll(/(?<![0-9&])>>?\s*([^\s&|;>]+)/g)) if (match[1] !== "/dev/null" && !match[1].startsWith("&")) consider("write", `> ${match[1]}`);
  for (const segment of script.split(/\s*(?:;|&&|\|\||\||\n|\$\(|`)\s*/)) {
    let parts = segment.trim().split(/\s+/).map((word) => word.replace(/^['"(]+|['")]+$/g, "")).filter(Boolean);
    // Through wrappers and their options (`sudo -u www`, `timeout 5`, `env A=b`).
    while (parts.length) {
      const name = basename(parts[0]);
      if (REMOTE_WRAPPERS.test(name)) {
        parts = parts.slice(1);
        while (parts.length && (parts[0].startsWith("-") || /^[A-Za-z_][A-Za-z0-9_]*=/.test(parts[0]) || /^\d+[smh]?$/.test(parts[0]))) {
          const option = parts[0];
          parts = parts.slice(1);
          if (/^-(u|g|C|n|k|s)$/.test(option) && parts.length) parts = parts.slice(1);
        }
        continue;
      }
      if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(parts[0])) {
        parts = parts.slice(1);
        continue;
      }
      break;
    }
    if (!parts.length) continue;
    const program = basename(parts[0]);
    // `bash -c '…'`: its script is read as the remote command's.
    if (/^(sh|bash|zsh|dash)$/.test(program) && parts.some((word) => /^-[a-z]*c$/.test(word))) {
      const inner = remoteEffect(parts.slice(parts.findIndex((word) => /^-[a-z]*c$/.test(word)) + 1));
      if (inner) consider(inner.action, inner.says);
      continue;
    }
    const rest = parts.slice(1);
    const subs = rest.filter((word) => !word.startsWith("-") || /^-[RS]/.test(word));
    for (const row of REMOTE_EFFECTS) {
      if (!row.programs.includes(program)) continue;
      if (row.subs && !subs.slice(0, 2).some((word) => row.subs!.test(word))) continue;
      if (row.flags && !rest.some((word) => row.flags!.test(word))) continue;
      consider(row.action, parts.slice(0, 4).join(" "));
    }
  }
  return best;
}
