/**
 * Runtime operands and their possible set (round 6, step 2; design §7.3.12): what a command destroys when its
 * operands are known only when it runs — handed by a pipe (`… | xargs git branch -D`, `parallel`), by a
 * substitution (`docker rm $(docker ps -q --filter name=pg-)`), by `find … -exec CMD {} +`, or a pattern the
 * program matches itself (`pkill -f 'vite --port'`). The set is the PRODUCER's: the kinds the consumer's operand
 * position holds (schema.ts `typedTargets`, `RUNTIME_OPERANDS`), narrowed by the name tests the producer and
 * the filters between apply (`TEST_SOURCES`: `grep P`, `--filter name=P`, `git tag -l P`, `refs/heads/P`, a
 * listing's operand, a port). A test the reader can't follow (`grep -v`, a program's own output) narrows
 * nothing: the set stays every entity of the kind. core.ts `otherTaskOf` meets the set with other chats'
 * ledgers; nothing here asks on its own.
 */

import { basename } from "node:path";

import type { EffectClass } from "./effects.ts";
import type { EntityKind } from "./entities.ts";
import { typedTargets } from "./schema.ts";
import { UNKNOWN } from "./shell.ts";

/** The word that stands for a runtime operand while the consumer is read by the tables. */
export const RUNTIME = "\u0002runtime\u0002";

/** A name test from a producer or a filter: a regex on the producer's output line, or a glob on the name. */
export type NameTest = { regex: RegExp } | { glob: string } | { contains: string };

/** A runtime operand: the kinds it may be, and the tests every name it takes passes. */
export interface RuntimeOperand {
  kinds: EntityKind[];
  tests: NameTest[];
  /** The consumer, for the dialog. */
  by: string;
  /** What it does to them, when its row says (`GROUP_DESTROYERS`): a group's members deleted are a `change`. */
  effects?: EffectClass[];
  /** The group itself as a place, when its row says (`group: "root"`: the folder whose configuration declares it). */
  root?: string;
}

/**
 * Commands that destroy a project's resources as a group (final pass, data): what they destroy is every entity of
 * the kinds their options name whose name carries the project's (`docker compose down -v` removes the volumes
 * `<project>_*`, the same volume a denied `docker volume rm atlas_pgdata` named), or, when the members are declared
 * in a file the line doesn't show (`kubectl delete -f`, `terraform destroy`), every entity of the kinds. `kinds`:
 * always; `options`: more kinds with an option (`-v` the volumes, `--rmi` the images); `project`: how the group's
 * name is found (compose: `-p`, else the compose file's folder, else the folder it runs in).
 */
export const GROUP_DESTROYERS: readonly { program: readonly string[]; sub: readonly string[]; option?: RegExp; kinds: readonly EntityKind[]; options?: readonly { option: RegExp; kinds: readonly EntityKind[] }[]; project?: "compose"; group?: "root"; typed?: boolean }[] = [
  // Its networks are `resource`s as its volumes are (schema.ts): only `-v` reaches that kind, so a denied volume isn't met by a plain `down`.
  { program: ["docker", "podman"], sub: ["compose", "down"], kinds: ["container"], options: [{ option: /^(-v|--volumes)$/, kinds: ["resource"] }, { option: /^--rmi(=.*)?$/, kinds: ["image"] }], project: "compose" },
  { program: ["docker-compose", "podman-compose"], sub: ["down"], kinds: ["container"], options: [{ option: /^(-v|--volumes)$/, kinds: ["resource"] }, { option: /^--rmi(=.*)?$/, kinds: ["image"] }], project: "compose" },
  { program: ["docker", "podman"], sub: ["compose", "rm"], kinds: ["container"], options: [{ option: /^(-v|--volumes)$/, kinds: ["resource"] }], project: "compose" },
  { program: ["docker-compose", "podman-compose"], sub: ["rm"], kinds: ["container"], options: [{ option: /^(-v|--volumes)$/, kinds: ["resource"] }], project: "compose" },
  { program: ["kubectl", "oc"], sub: ["delete"], option: /^(-f|--filename|-k|--kustomize)(=.*)?$/, kinds: ["namespace", "resource"] },
  // Iteration 3: a delete by selector or of all (`kubectl delete ns -l preview=true`, `--all`): every entity of the type.
  { program: ["kubectl", "oc"], sub: ["delete"], option: /^(-l|--selector|--all|--field-selector)(=.*)?$/, kinds: ["resource"], typed: true },
  { program: ["terraform", "tofu"], sub: ["destroy"], kinds: ["resource"], group: "root" },
  // Final pass: the flag form of the same destroy (`terraform apply -destroy`, `plan -destroy` only plans).
  { program: ["terraform", "tofu"], sub: ["apply"], option: /^-?-destroy$/, kinds: ["resource"], group: "root" },
];

/** A compose project's name (`-p`, `--project-name`, else the compose file's folder, else `cwd`), as compose normalizes it. */
function composeProject(words: readonly string[], cwd: string | undefined): string | undefined {
  let name: string | undefined;
  let file: string | undefined;
  for (const [i, word] of words.entries()) {
    if (word === "-p" || word === "--project-name") name = words[i + 1];
    else if (word.startsWith("--project-name=")) name = word.slice("--project-name=".length);
    else if (word === "-f" || word === "--file") file = words[i + 1];
  }
  if (name === UNKNOWN || file === UNKNOWN) return undefined;
  name ??= file?.includes("/") ? basename(file.replace(/\/[^/]*$/, "")) : cwd !== undefined ? basename(cwd) : undefined;
  return name?.toLowerCase().replace(/[^a-z0-9_-]/g, "") || undefined;
}

/** The group a command destroys (`GROUP_DESTROYERS`), as a runtime operand; undefined when it is none. */
export function groupOperand(inner: readonly string[], cwd: string | undefined): RuntimeOperand | undefined {
  const program = basename(inner[0] ?? "");
  const positionals = inner.slice(1).filter((word, i, all) => !word.startsWith("-") && !/^(-p|--project-name|-f|--file|-n|--namespace)$/.test(all[i - 1] ?? ""));
  const row = GROUP_DESTROYERS.find((candidate) => candidate.program.includes(program) && candidate.sub.every((word, at) => positionals[at] === word) && (!candidate.option || inner.some((word) => candidate.option!.test(word))));
  if (!row) return undefined;
  const kinds = new Set<EntityKind>(row.kinds);
  // A typed delete's resource type word (`ns`, `namespace`): its own kind (schema.ts `KUBE_TYPES`).
  if (row.typed && /^(ns|namespace|namespaces)$/.test(positionals[1] ?? "")) {
    kinds.clear();
    kinds.add("namespace");
  }
  for (const extra of row.options ?? []) if (inner.some((word) => extra.option.test(word))) for (const kind of extra.kinds) kinds.add(kind);
  const tests: NameTest[] = [];
  if (row.project === "compose") {
    const project = composeProject(inner, cwd);
    if (!project) return undefined;
    tests.push({ regex: new RegExp(`^${project.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}[-_]`) });
  }
  // A configuration root (`terraform -chdir=DIR`, else the folder it runs in): the group as one entity, so the same
  // destroy in another spelling (`apply -destroy` after a denied `destroy`) meets the denial by that place.
  const chdir = inner.find((word) => word.startsWith("-chdir="))?.slice("-chdir=".length);
  const root = row.group === "root" ? (chdir ?? cwd) : undefined;
  return { kinds: [...kinds], tests, by: [inner[0], ...row.sub].join(" "), effects: ["change"], ...(root !== undefined ? { root } : {}) };
}

/**
 * Operand kinds of destroyers the schema table has no row for: program, subcommand words (all present in
 * order among the positionals' first words), the kinds a runtime operand there takes. Data, not cases.
 */
export const RUNTIME_OPERANDS: readonly { program: string; sub?: readonly string[]; option?: RegExp; word?: RegExp; kinds: readonly EntityKind[] }[] = [
  { program: "git", sub: ["branch"], option: /^-(d|D|-delete)$/, kinds: ["branch"] },
  { program: "git", sub: ["tag"], option: /^-(d|-delete)$/, kinds: ["tag"] },
  { program: "git", sub: ["push"], option: /^(-d|--delete)$/, kinds: ["branch", "tag"] },
  // Final pass: a refspec with an empty source deletes its destination (`git push origin :refs/tags/{}`), whatever
  // spells it; `word`: the runtime operand's word. The ref's prefix says its kind.
  // Iteration 3: `update-ref -d` deletes the ref it is handed (`… | xargs -n1 git update-ref -d`).
  { program: "git", sub: ["update-ref"], option: /^(-d|--delete)$/, kinds: ["branch", "tag"] },
  { program: "git", sub: ["push"], word: /^\+?:refs\/tags\//, kinds: ["tag"] },
  { program: "git", sub: ["push"], word: /^\+?:refs\/heads\//, kinds: ["branch"] },
  { program: "git", sub: ["push"], word: /^\+?:(?!refs\/)/, kinds: ["branch", "tag"] },
  { program: "git", sub: ["worktree", "remove"], kinds: ["worktree", "path"] },
  { program: "gh", sub: ["pr", "close"], kinds: ["pr"] },
  { program: "gh", sub: ["release", "delete"], kinds: ["release", "tag"] },
  { program: "glab", sub: ["mr", "close"], kinds: ["pr"] },
  { program: "kill", kinds: ["process"] },
  { program: "rm", kinds: ["path", "worktree"] },
  { program: "rmdir", kinds: ["path"] },
  { program: "unlink", kinds: ["path"] },
  { program: "shred", kinds: ["path"] },
  { program: "truncate", kinds: ["path"] },
  { program: "trash", kinds: ["path"] },
];

/**
 * Commands whose operand is a quantifier, not a name (data, round 6 step 2): they destroy every entity of the kinds
 * (narrowed by their own `--filter`): `docker container prune`, `docker system prune`, `xcrun simctl erase all`.
 * `sub`: the subcommand words in order among the positionals; `all`: a positional that must be there.
 */
export const QUANTIFIED: readonly { program: string; sub: readonly string[]; all?: string; kinds: readonly EntityKind[] }[] = [
  { program: "tmux", sub: ["kill-server"], kinds: ["resource"] },
  { program: "docker", sub: ["container", "prune"], kinds: ["container"] },
  { program: "docker", sub: ["image", "prune"], kinds: ["image"] },
  { program: "docker", sub: ["volume", "prune"], kinds: ["resource"] },
  { program: "docker", sub: ["network", "prune"], kinds: ["resource"] },
  { program: "docker", sub: ["system", "prune"], kinds: ["container", "image", "resource"] },
  { program: "podman", sub: ["container", "prune"], kinds: ["container"] },
  { program: "podman", sub: ["system", "prune"], kinds: ["container", "image", "resource"] },
  { program: "xcrun", sub: ["simctl", "erase"], all: "all", kinds: ["simulator"] },
  { program: "xcrun", sub: ["simctl", "delete"], all: "all", kinds: ["simulator"] },
  { program: "simctl", sub: ["erase"], all: "all", kinds: ["simulator"] },
  { program: "simctl", sub: ["delete"], all: "all", kinds: ["simulator"] },
];

/** Programs that destroy what matches a pattern they are given (their operand is a test, not a name). */
const PATTERN_KILLERS: ReadonlySet<string> = new Set(["pkill", "killall"]);

/** Programs that hand their input lines to a command as operands, and their options that take a value. */
const FEEDERS: Readonly<Record<string, ReadonlySet<string>>> = {
  xargs: new Set(["-I", "-i", "-n", "-L", "-P", "-s", "-d", "-E", "-a", "--max-args", "--max-procs", "--delimiter", "--arg-file", "--replace"]),
  parallel: new Set(["-j", "--jobs", "-I", "--colsep", "-n", "-N", "--delay", "--timeout", "--joblog", "-S"]),
};

/** The consumer's words with its runtime operands as `RUNTIME`, or undefined when it has none. */
export function consumerWords(words: readonly string[], piped: boolean): string[] | undefined {
  const program = basename(words[0] ?? "");
  const takes = FEEDERS[program];
  if (takes && piped) {
    let i = 1;
    let token: string | undefined;
    for (; i < words.length; i++) {
      const word = words[i];
      if (!word.startsWith("-")) break;
      if (word === "-0" || word === "--null" || word === "-r" || word === "-t") continue;
      if (/^-I.+/.test(word)) token = word.slice(2);
      else if (/^--replace=/.test(word)) token = word.slice("--replace=".length);
      else if (takes.has(word)) {
        if (word === "-I" || word === "--replace") token = words[i + 1];
        i++;
      }
    }
    let inner = words.slice(i);
    // `parallel 'cmd {}'`: one quoted command line.
    if (inner.length === 1 && /\s/.test(inner[0])) inner = inner[0].split(/\s+/).filter(Boolean);
    if (!inner.length) return undefined;
    const marker = token ?? (program === "parallel" ? "{}" : undefined);
    if (marker && inner.some((word) => word.includes(marker))) return inner.map((word) => word.replaceAll(marker, RUNTIME));
    return [...inner, RUNTIME];
  }
  // `find ROOT … -exec CMD {} +` / `-execdir`, `-ok`.
  if (program === "find" || program === "gfind") {
    const at = words.findIndex((word) => /^-(exec|execdir|ok|okdir)$/.test(word));
    if (at < 0) return words.includes("-delete") ? ["rm", RUNTIME] : undefined;
    const end = words.findIndex((word, index) => index > at && (word === ";" || word === "\\;" || word === "+"));
    const inner = words.slice(at + 1, end < 0 ? undefined : end);
    return inner.some((word) => word.includes("{}")) ? inner.map((word) => word.replaceAll("{}", RUNTIME)) : undefined;
  }
  if (!words.slice(1).includes(UNKNOWN)) return undefined;
  return words.map((word, index) => (index > 0 && word === UNKNOWN ? RUNTIME : word));
}

/** The kinds a `RUNTIME` word stands for in these (unwrapped) words: the schema's, else `RUNTIME_OPERANDS`. */
export function runtimeKinds(words: readonly string[]): EntityKind[] {
  const kinds = new Set<EntityKind>();
  const typed = typedTargets(words);
  // Only what destroys: a delete or a stop (a read or an install hands nothing away).
  const destroying = Boolean(typed?.effects.some((effect) => effect === "change" || effect === "stop"));
  if (typed && destroying) for (const target of typed.targets) if (target.name === RUNTIME || target.name.includes(RUNTIME)) kinds.add(target.kind);
  // A runtime word where the schema wants a resource type (`xargs kubectl delete` handed `namespace/x`): any kind it names.
  if (typed && destroying && !kinds.size && words.includes(RUNTIME) && /^(kubectl|oc)$/.test(basename(words[0] ?? ""))) for (const kind of ["namespace", "resource"] as const) kinds.add(kind);
  const program = basename(words[0] ?? "");
  const positionals = words.slice(1).filter((word) => !word.startsWith("-"));
  for (const row of RUNTIME_OPERANDS) {
    if (row.program !== program) continue;
    if (row.sub && !row.sub.every((word, index) => positionals[index] === word)) continue;
    if (row.option && !words.some((word) => row.option!.test(word))) continue;
    if (row.word) {
      if (words.some((word) => word.includes(RUNTIME) && row.word!.test(word))) for (const kind of row.kinds) kinds.add(kind);
      continue;
    }
    if (words.includes(RUNTIME)) for (const kind of row.kinds) kinds.add(kind);
  }
  return [...kinds];
}

/** A shell glob as a regex over a whole name. */
function globRegex(glob: string): RegExp {
  let out = "";
  for (const char of glob) out += char === "*" ? ".*" : char === "?" ? "." : char.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`^${out}$`);
}

function regexOf(text: string): RegExp | undefined {
  try {
    return new RegExp(text);
  } catch {
    return undefined;
  }
}

/**
 * Where producers and filters state what they pass on, as name tests (data): a filter's pattern operand, a
 * listing's name filter, a ref prefix, a port. Each row reads one command's words; none narrows when the
 * value is known only when it runs.
 */
export const TEST_SOURCES: readonly { programs: readonly string[]; read: (words: readonly string[]) => NameTest[] | undefined }[] = [
  {
    programs: ["grep", "egrep", "fgrep", "rg"],
    read: (words) => {
      // `-v` passes what doesn't match: no narrowing.
      if (words.some((word) => /^-[a-zA-Z]*v/.test(word) || word === "--invert-match")) return [];
      const e = words.findIndex((word) => word === "-e");
      const pattern = e > 0 ? words[e + 1] : words.slice(1).find((word) => !word.startsWith("-"));
      if (!pattern || pattern === UNKNOWN) return [];
      const fixed = basename(words[0]) === "fgrep" || words.some((word) => /^-[a-zA-Z]*F/.test(word));
      const regex = fixed ? undefined : regexOf(pattern);
      return [regex ? { regex } : { contains: pattern }];
    },
  },
  {
    programs: ["awk", "gawk", "mawk", "nawk"],
    read: (words) => {
      const program = words.slice(1).find((word) => !word.startsWith("-"));
      const match = program && program !== UNKNOWN ? /^\s*\/((?:[^/\\]|\\.)+)\//.exec(program) : null;
      const regex = match ? regexOf(match[1].replace(/\\\//g, "/")) : undefined;
      return regex ? [{ regex }] : [];
    },
  },
  {
    // `docker ps --filter name=P` (a regex on the name), `docker images REPO` (its tags).
    programs: ["docker", "podman"],
    read: (words) => {
      const out: NameTest[] = [];
      for (const [index, word] of words.entries()) {
        const value = word === "--filter" || word === "-f" ? words[index + 1] : word.startsWith("--filter=") ? word.slice(9) : undefined;
        const name = value && value !== UNKNOWN ? /^(?:name|reference)=(.+)$/.exec(value)?.[1] : undefined;
        if (name) {
          const regex = regexOf(name);
          if (regex) out.push({ regex });
        }
      }
      const positionals = words.slice(1).filter((word, index, all) => !word.startsWith("-") && !/^(--filter|-f|--format)$/.test(all[index - 1] ?? ""));
      if (/^(images|image)$/.test(positionals[0] ?? "")) {
        const repo = positionals[positionals[0] === "image" ? 2 : 1];
        if (repo && repo !== UNKNOWN && (positionals[0] === "images" || positionals[1] === "ls")) out.push({ glob: `${repo}*` });
      }
      return out;
    },
  },
  {
    // `git tag -l P`, `git branch --list P`, `git for-each-ref refs/heads/P`.
    programs: ["git"],
    read: (words) => {
      const out: NameTest[] = [];
      const sub = words.slice(1).find((word) => !word.startsWith("-"));
      const operands = words.slice(words.indexOf(sub ?? "") + 1).filter((word) => !word.startsWith("-") && word !== UNKNOWN);
      if (sub === "tag" || sub === "branch") {
        if (words.some((word) => /^(-l|--list)$/.test(word)) && operands.length) out.push(...operands.map((glob) => ({ glob })));
      } else if (sub === "for-each-ref") {
        for (const ref of operands) {
          const name = /^refs\/(?:heads|tags|remotes\/[^/]+)\/(.*)$/.exec(ref)?.[1];
          // git matches a literal pattern completely or up to a slash (`refs/heads/feat` is feat and feat/…; iteration 2).
          if (name !== undefined && /[*?[]/.test(name)) out.push({ glob: name });
          else if (name !== undefined) out.push(name === "" || name.endsWith("/") ? { glob: `${name}*` } : { regex: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(/.*)?$`) });
        }
      }
      return out;
    },
  },
  {
    // `lsof -i :PORT`, `-ti :PORT`, `fuser PORT/tcp`: processes on that port.
    programs: ["lsof", "fuser"],
    read: (words) => words.slice(1).flatMap((word) => (/^(?:tcp:|udp:)?:?(\d{2,5})(?:\/(tcp|udp))?$/.test(word) && /\d/.test(word) && !/^-/.test(word) ? [{ contains: /(\d{2,5})/.exec(word)![1] }] : [])),
  },
  {
    // `pgrep P`, `pkill -f P`: the pattern the processes match.
    programs: ["pgrep", "pkill", "killall"],
    read: (words) => {
      const pattern = words.slice(1).find((word, index, all) => !word.startsWith("-") && !/^(-u|-U|-g|-G|-P|-s|-t|--signal)$/.test(all[index - 1] ?? ""));
      if (!pattern || pattern === UNKNOWN) return [];
      const regex = regexOf(pattern);
      return regex ? [{ regex }] : [{ contains: pattern }];
    },
  },
];

/** The tests one producer or filter applies. */
export function testsOf(words: readonly string[]): NameTest[] {
  const program = basename(words[0] ?? "");
  const row = TEST_SOURCES.find((candidate) => candidate.programs.includes(program));
  return row?.read(words) ?? [];
}

/** Whether a name passes a test: as itself, or as a listing prints it (`worktree PATH`, `namespace/NAME`, `refs/heads/NAME`), or by what else it is known by (`also`). */
export function passes(test: NameTest, kind: string, name: string, also: readonly string[] = []): boolean {
  // `also`: what else the entity is known by (a process's command line and ports; core.ts `entityForms`).
  const forms = [name, `${kind} ${name}`, `${kind}/${name}`, ...(kind === "branch" ? [`refs/heads/${name}`] : kind === "tag" ? [`refs/tags/${name}`] : []), basename(name), ...also];
  if ("glob" in test) return forms.some((form) => globRegex(test.glob).test(form));
  if ("contains" in test) return forms.some((form) => form.includes(test.contains));
  return forms.some((form) => test.regex.test(form));
}

/** One command of a line as flow.ts reads it. */
export interface FlowCommand {
  words: readonly string[];
  joint?: string;
}

/**
 * The runtime operands of `commands[index]` (unwrapped words `inner`), with the tests of the commands that
 * produce them: the pipeline before it and the substitutions it runs (the commands right before it that no
 * `&&`, `||`, `;` or `&` separates from it).
 */
export function runtimeOperand(commands: readonly FlowCommand[], index: number, inner: readonly string[]): RuntimeOperand | undefined {
  const program = basename(inner[0] ?? "");
  const piped = commands[index - 1]?.joint === "|";
  const tests: NameTest[] = [];
  let kinds: EntityKind[];
  let stops = false;
  const positionals = inner.slice(1).filter((word) => !word.startsWith("-"));
  const quantified = QUANTIFIED.find((row) => row.program === program && row.sub.every((word, at) => positionals[at] === word) && (!row.all || positionals.slice(row.sub.length).includes(row.all)));
  if (quantified) {
    kinds = [...quantified.kinds];
    tests.push(...testsOf(inner));
  } else if (PATTERN_KILLERS.has(program) || (program === "fuser" && inner.some((word) => /^-[a-zA-Z]*k/.test(word)))) {
    // `pkill -f P`, `killall N`, and `fuser -k PORT/tcp` (iteration 3): what matches, or holds the port, is stopped.
    kinds = ["process"];
    tests.push(...testsOf(inner));
    stops = true;
  } else {
    const consumer = consumerWords(inner, piped);
    if (!consumer) return undefined;
    kinds = runtimeKinds(consumer);
    // What `find` hands on is under its roots: path kinds read it by its globs (core.ts), not by tests.
  }
  if (!kinds.length) return undefined;
  for (let at = index - 1; at >= 0; at--) {
    const before = commands[at];
    if (before.joint !== undefined && before.joint !== "|") break;
    tests.push(...testsOf(before.words));
  }
  return { kinds, tests, by: inner.slice(0, 3).join(" "), ...(stops ? { effects: ["stop"] as EffectClass[] } : {}) };
}
