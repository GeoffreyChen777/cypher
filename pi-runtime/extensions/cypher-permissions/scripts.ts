/**
 * Project scripts a command runs (final pass, design §7.3.13): what `bash scripts/clean.sh`, `./tools/reset`,
 * `make clean`, `npm run clean` or `just clean` does is what the lines of that script, recipe or package script do.
 * One table, `SCRIPT_RUNNERS`: a program, how it finds the script (a file operand, a make target's recipe, a package
 * manager's script, a justfile recipe) and where it runs. Read from the files on disk (bounded); a script that
 * can't be read stays opaque, as before. core.ts reads each line it finds as its own line (denials, the floor's
 * other-task row), so the opaque command is no way around them.
 */

import { existsSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, isAbsolute, join } from "node:path";

/** The most lines one script, recipe or package script contributes, and the most bytes read of a file. */
const MAX_LINES = 64;
const MAX_BYTES = 256 * 1024;

export interface ProjectScript {
  /** What runs it, for the dialog (`make clean`, `npm run clean`, `bash scripts/clean.sh`). */
  by: string;
  /** Its shell lines, in order. */
  lines: string[];
  /** The folder its lines run in. */
  cwd: string;
  /** The file its lines are in (a script, a makefile, `package.json`), when it is one. */
  file?: string;
}

/** Reads a file's text (from disk; a line's own earlier writes first: `projectScripts`'s `read`). */
export type ReadFile = (path: string) => string | undefined;

type Reader = (words: readonly string[], cwd: string, read: ReadFile) => ProjectScript[] | undefined;

export function readText(path: string): string | undefined {
  try {
    if (!existsSync(path) || !statSync(path).isFile() || statSync(path).size > MAX_BYTES) return undefined;
    return readFileSync(path, "utf8");
  } catch {
    return undefined;
  }
}

/** A shell script's lines: comments and blank lines out, backslash continuations joined. */
export function shellLines(text: string): string[] {
  const out: string[] = [];
  let pending = "";
  for (const raw of text.split("\n")) {
    const line = raw.replace(/\r$/, "");
    if (line.endsWith("\\")) {
      pending += `${line.slice(0, -1)} `;
      continue;
    }
    const whole = (pending + line).trim();
    pending = "";
    if (!whole || whole.startsWith("#")) continue;
    out.push(whole);
    if (out.length >= MAX_LINES) break;
  }
  return out;
}

/** A file a shell runs: `bash FILE`, `sh -e FILE`, `source FILE` (no `-c`, which is inline code the reader reads). */
const shellFile: Reader = (words, cwd, read) => {
  const args = words.slice(1);
  if (args.some((word) => word === "-c" || /^-[a-zA-Z]*c$/.test(word) || word === "-s")) return undefined;
  const file = args.find((word) => !word.startsWith("-") && !word.startsWith("+"));
  if (!file) return undefined;
  const path = isAbsolute(file) ? file : join(cwd, file);
  const text = read(path);
  return text === undefined ? undefined : [{ by: words.slice(0, 2).join(" "), lines: shellLines(text), cwd, file: path }];
};

/** A makefile's recipe for each target (prerequisites' recipes first, one level), or the first target's. */
const makeRecipe: Reader = (words, cwd, read) => {
  let dir = cwd;
  let file: string | undefined;
  const targets: string[] = [];
  for (let i = 1; i < words.length; i++) {
    const word = words[i];
    if (word === "-C" || word === "--directory") dir = join(dir, words[++i] ?? "");
    else if (word === "-f" || word === "--file" || word === "--makefile") file = words[++i];
    else if (/^-C./.test(word)) dir = join(dir, word.slice(2));
    else if (word.startsWith("-") || word.includes("=")) continue;
    else targets.push(word);
  }
  // `-f -` / `-f /dev/stdin`: the makefile is the command's input (iteration 2), which the caller's reader gives.
  const path = file === "-" || file === "/dev/stdin" ? "/dev/stdin" : file ? (isAbsolute(file) ? file : join(dir, file)) : ["GNUmakefile", "makefile", "Makefile"].map((name) => join(dir, name)).find((candidate) => read(candidate) !== undefined);
  const text = path ? read(path) : undefined;
  if (text === undefined) return undefined;
  const rules = new Map<string, { prerequisites: string[]; recipe: string[] }>();
  let order: string[] = [];
  let current: { prerequisites: string[]; recipe: string[] }[] = [];
  for (const raw of text.split("\n")) {
    if (raw.startsWith("\t")) {
      const line = raw.slice(1).replace(/^[@+-]+/, "").trim();
      if (line && !line.startsWith("#")) for (const rule of current) if (rule.recipe.length < MAX_LINES) rule.recipe.push(line);
      continue;
    }
    const rule = /^([^:#=\s][^:#=]*?)\s*::?(?!=)\s*([^;#]*)/.exec(raw);
    if (!rule) {
      if (raw.trim() && !raw.startsWith(" ")) current = [];
      continue;
    }
    current = [];
    for (const name of rule[1].trim().split(/\s+/)) {
      const entry = rules.get(name) ?? { prerequisites: [], recipe: [] };
      entry.prerequisites.push(...rule[2].trim().split(/\s+/).filter(Boolean));
      rules.set(name, entry);
      current.push(entry);
      if (!name.startsWith(".") && !name.includes("%")) order.push(name);
    }
  }
  order = [...new Set(order)];
  const wanted = targets.length ? targets : order.slice(0, 1);
  const out: ProjectScript[] = [];
  for (const target of wanted) {
    const rule = rules.get(target);
    if (!rule) continue;
    const lines = [...rule.prerequisites.flatMap((name) => rules.get(name)?.recipe ?? []), ...rule.recipe].slice(0, MAX_LINES);
    out.push({ by: `make ${target}`, lines, cwd: dir, ...(path !== "/dev/stdin" ? { file: path } : {}) });
  }
  return out;
};

/** Package managers' lifecycle words that run a script of that name without `run`. */
const LIFECYCLE = new Set(["test", "start", "stop", "restart", "t", "tst"]);

/** A package manager's script (`npm run X`, `npm test`, `pnpm X`, `yarn X`, `bun run X`) with its pre/post scripts. */
const packageScript: Reader = (words, cwd, read) => {
  const program = basename(words[0] ?? "");
  let dir = cwd;
  const positionals: string[] = [];
  for (let i = 1; i < words.length; i++) {
    const word = words[i];
    if (word === "--prefix" || word === "-C" || word === "--dir" || word === "--cwd") dir = join(dir, words[++i] ?? "");
    else if (word === "--") break;
    else if (!word.startsWith("-")) positionals.push(word);
  }
  let name: string | undefined;
  if (positionals[0] === "run" || positionals[0] === "run-script") name = positionals[1];
  else if (positionals[0] && (LIFECYCLE.has(positionals[0]) || program !== "npm")) name = positionals[0] === "t" || positionals[0] === "tst" ? "test" : positionals[0];
  if (!name) return undefined;
  const text = read(join(dir, "package.json"));
  if (text === undefined) return undefined;
  let scripts: Record<string, unknown> = {};
  try {
    scripts = (JSON.parse(text) as { scripts?: Record<string, unknown> }).scripts ?? {};
  } catch {
    return undefined;
  }
  const lines = [`pre${name}`, name, `post${name}`].map((key) => scripts[key]).filter((value): value is string => typeof value === "string");
  return lines.length ? [{ by: `${program} run ${name}`, lines, cwd: dir, file: join(dir, "package.json") }] : undefined;
};

/** A justfile recipe (`just X`; the first recipe without one): its indented lines. */
const justRecipe: Reader = (words, cwd, read) => {
  const text = ["justfile", "Justfile", ".justfile"].map((name) => read(join(cwd, name))).find((value) => value !== undefined);
  if (text === undefined) return undefined;
  const wanted = words.slice(1).find((word) => !word.startsWith("-"));
  const recipes = new Map<string, string[]>();
  let current: string[] | undefined;
  let first: string | undefined;
  for (const raw of text.split("\n")) {
    const head = /^@?([A-Za-z_][\w-]*)(?:\s+[^:]*)?:(?!=)/.exec(raw);
    if (head && !/^\s/.test(raw)) {
      current = [];
      recipes.set(head[1], current);
      first ??= head[1];
      continue;
    }
    if (current && /^\s+\S/.test(raw)) current.push(raw.trim().replace(/^[@-]+/, ""));
    else if (raw.trim()) current = undefined;
  }
  const name = wanted ?? first;
  const lines = name ? recipes.get(name) : undefined;
  return lines?.length ? [{ by: `just ${name}`, lines: lines.slice(0, MAX_LINES), cwd }] : undefined;
};

/** A file run by its path (`./scripts/clean.sh`, `tools/reset`) whose first line names a shell. */
const pathScript: Reader = (words, cwd, read) => {
  const program = words[0] ?? "";
  if (!program.includes("/")) return undefined;
  const path = isAbsolute(program) ? program : join(cwd, program);
  const text = read(path);
  if (text === undefined || !/^#!\s*(?:\/usr\/bin\/env\s+(?:-S\s+)?)?(?:\/[\w/]*\/)?(sh|bash|zsh|dash|ksh)\b/.test(text)) return undefined;
  return [{ by: program, lines: shellLines(text), cwd, file: path }];
};

/** How each runner finds the script it runs (data). */
export const SCRIPT_RUNNERS: readonly { programs: readonly string[]; read: Reader }[] = [
  { programs: ["bash", "sh", "zsh", "dash", "ksh", "source", "."], read: shellFile },
  { programs: ["make", "gmake"], read: makeRecipe },
  { programs: ["npm", "pnpm", "yarn", "bun"], read: packageScript },
  { programs: ["just"], read: justRecipe },
];

/** The project scripts a command (unwrapped words, its folder) runs, read from disk; empty when it runs none. */
export function projectScripts(words: readonly string[], cwd: string | undefined, read: ReadFile = readText, system: (path: string) => boolean = () => false): ProjectScript[] {
  if (!words.length || cwd === undefined) return [];
  const program = basename(words[0]);
  const row = SCRIPT_RUNNERS.find((candidate) => candidate.programs.includes(program));
  // The system's own scripts (`/Applications/Xcode.app/…/simctl`, `/usr/bin/…`: a place the agent can't write) are
  // programs like any other, not this project's code: not read (final pass).
  const guarded: ReadFile = (path) => (path !== "/dev/stdin" && system(isAbsolute(path) ? path : join(cwd, path)) ? undefined : read(path));
  const found = row ? row.read(words, cwd, guarded) : pathScript(words, cwd, guarded);
  return (found ?? []).filter((script) => script.lines.length > 0).map((script) => ({ ...script, cwd: script.cwd || dirname(cwd) }));
}

/**
 * Code a command sets up to run later (final pass, data). `HOOK_TRIGGERS`: the git hooks a git subcommand runs, so a
 * hook this line writes and then triggers (`printf … > .git/hooks/pre-commit && git commit`) is read as the lines it
 * runs. `scheduledLines`: a crontab's entries without their schedule fields. What can't be read this way is marked for
 * the judge (`unreadParts`), never approved by default.
 */
export const HOOK_TRIGGERS: Readonly<Record<string, readonly string[]>> = {
  commit: ["pre-commit", "prepare-commit-msg", "commit-msg", "post-commit"],
  push: ["pre-push"],
  merge: ["pre-merge-commit", "post-merge"],
  pull: ["post-merge"],
  checkout: ["post-checkout"],
  switch: ["post-checkout"],
  rebase: ["pre-rebase", "post-rewrite"],
  am: ["applypatch-msg", "pre-applypatch", "post-applypatch"],
};

/** The hooks a git command runs (`HOOK_TRIGGERS`), by its subcommand; none with `--no-verify`/`-n` for commit and push. */
export function triggeredHooks(words: readonly string[]): string[] {
  if (basename(words[0] ?? "") !== "git") return [];
  let i = 1;
  while (i < words.length && words[i].startsWith("-")) i += /^-(C|c)$|^--(git-dir|work-tree)$/.test(words[i]) ? 2 : 1;
  const sub = words[i];
  const hooks = sub ? HOOK_TRIGGERS[sub] : undefined;
  if (!hooks) return [];
  if ((sub === "commit" || sub === "push") && words.some((word) => word === "--no-verify" || (sub === "commit" && word === "-n"))) return [];
  return [...hooks];
}

/** A crontab's command lines: each entry without its five schedule fields (or its `@reboot`-style macro); comments and settings out. */
export function scheduledLines(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#") || /^[A-Za-z_][A-Za-z0-9_]*\s*=/.test(line)) continue;
    const macro = /^@\w+\s+(.+)$/.exec(line);
    const fields = /^(?:\S+\s+){5}(.+)$/.exec(line);
    const command = macro?.[1] ?? fields?.[1];
    if (command) out.push(command);
  }
  return out.slice(0, MAX_LINES);
}

/**
 * What of a line the code can't read (final pass, data; evidence for the judge): a sourced file's variables, code set
 * up to run later that isn't read here (a schedule, a launch agent, a startup file, a hook not written by this line).
 */
export const UNREAD_FORMS: readonly { what: (match: RegExpMatchArray) => string; pattern: RegExp }[] = [
  { pattern: /(?:^|[;&|({]\s*|\b(?:then|do)\s+)(?:source|\.)\s+([^\s;&|)]+)/g, what: (m) => `the variables ${m[1]} sets (a sourced file is not read)` },
  { pattern: /(?:^|[;&|({]\s*)(crontab)(?:\s+(?!-[lr]\b)[^;&|]*)?(?=$|[;&|)])/g, what: () => "code set up to run later (a crontab)" },
  { pattern: /(?:^|[;&|({]\s*)(at|batch)\s+[^;&|]*/g, what: (m) => `code set up to run later (${m[1]})` },
  // Written there (a redirect, a copier's destination), or switched on: a read of the folder is no setup.
  { pattern: /(?:>>?\s*|\b(?:tee(?:\s+-a)?|cp|mv|ln|install|chmod)\s+(?:[^;&|]*\s)?)['"]?([^\s'";&|]*\.git\/hooks\/[\w.-]+)|\bgit\s+config\b[^;&|]*core\.hooksPath/g, what: (m) => `code set up to run later (a git hook: ${m[1] ?? "core.hooksPath"})` },
  { pattern: /(?:>>?\s*|\b(?:tee(?:\s+-a)?|cp|mv|ln|install)\s+(?:[^;&|]*\s)?)['"]?([^\s'";&|]*Library\/Launch(?:Agents|Daemons)\/[^\s'";&|]*)|\blaunchctl\s+(?:load|bootstrap|enable)\b/g, what: (m) => `code set up to run later (a launch agent${m[1] ? `: ${m[1]}` : ""})` },
  { pattern: /(?:>>?\s*|\b(?:tee(?:\s+-a)?|cp|mv|ln|install)\s+(?:[^;&|]*\s)?)['"]?([^\s'";&|]*\.config\/(?:systemd\/user|autostart)\/[^\s'";&|]*)|\bsystemctl\s+--user\s+(?:enable|start)\b/g, what: (m) => `code set up to run later (${m[1] ?? "a systemd user unit"})` },
];

/** The parts of a line the code can't read (`UNREAD_FORMS`), in words. Writes into startup files are the tables' own concern. */
export function unreadParts(line: string): string[] {
  const out: string[] = [];
  for (const row of UNREAD_FORMS) for (const match of line.matchAll(row.pattern)) {
    const what = row.what(match);
    if (!out.includes(what)) out.push(what);
  }
  return out.slice(0, 6);
}
