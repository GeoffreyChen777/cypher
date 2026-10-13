/**
 * Command rules: which commands run outside the sandbox (`allow`), always
 * ask first (`ask`), or never run (`deny`).
 *
 * A pattern is a list of words matched against a simple command's words
 * (shell.ts): `*` and `?` glob within one word, `**` stands for any number of
 * words. Its first word names the program and is literal; shells,
 * interpreters and wrappers cannot be one (a rule on them would cover any
 * command). The engine validates the same way
 * (`crates/engine/src/pi/permissions.rs`); keep the lists in step.
 */

import { createHash } from "node:crypto";
import { accessSync, constants, readFileSync, realpathSync, statSync } from "node:fs";
import { basename, delimiter, isAbsolute, join, resolve } from "node:path";

export type CommandDecision = "allow" | "ask" | "deny";

export interface CommandRule {
  match: string[];
  decision: CommandDecision;
  /** What the rule is for, in the words of whoever approved it. */
  label?: string;
  /** The program's contents, for a program that lives in a writable place. */
  sha256?: string;
  createdAt?: string;
}

export const MAX_PATTERN_WORDS = 32;
const MAX_WORD_CHARS = 512;

/** Programs a pattern cannot start with: each can run any other command. */
export const BANNED_PROGRAMS: ReadonlySet<string> = new Set([
  "sh", "bash", "zsh", "dash", "ksh", "csh", "tcsh", "fish", "nu", "pwsh", "powershell",
  "env", "sudo", "doas", "su", "xargs", "eval", "exec", "command", "builtin", "nohup", "time", "timeout", "nice", "watch", "script", "parallel", "caffeinate",
  "python", "python2", "python3", "node", "deno", "bun", "bunx", "npx", "pnpx", "uvx", "pipx", "ruby", "perl", "php", "lua", "tclsh", "Rscript", "osascript", "expect",
  "awk", "gawk", "sed", "find",
]);

/**
 * Builtins that only change directory or print: they never need the
 * sandbox's leave, so a line may hold them beside escalated commands.
 */
export const INERT_PROGRAMS: ReadonlySet<string> = new Set(["cd", "pushd", "popd", "pwd", "true", "false", ":", "echo", "printf", "sleep"]);

/** An error for an unusable pattern, else undefined. */
export function patternError(pattern: unknown): string | undefined {
  if (!Array.isArray(pattern) || pattern.length === 0) return "A pattern is a non-empty list of words.";
  if (pattern.length > MAX_PATTERN_WORDS) return `A pattern has at most ${MAX_PATTERN_WORDS} words.`;
  for (const word of pattern) {
    if (typeof word !== "string" || word.length === 0 || word.length > MAX_WORD_CHARS || /[\0\n\r]/.test(word)) return "Each pattern word is a non-empty line of text.";
  }
  const program = pattern[0] as string;
  if (program === "**" || /[*?[]/.test(program)) return "A pattern starts with the literal program name.";
  const name = basename(program);
  if (BANNED_PROGRAMS.has(name)) return `\`${name}\` can run any command, so it cannot start a pattern; name the program it runs instead.`;
  if (INERT_PROGRAMS.has(name)) return `\`${name}\` never needs to leave the sandbox.`;
  return undefined;
}

export function isCommandRule(value: unknown): value is CommandRule {
  if (typeof value !== "object" || value === null) return false;
  const rule = value as Partial<CommandRule>;
  return (
    (rule.decision === "allow" || rule.decision === "ask" || rule.decision === "deny") &&
    patternError(rule.match) === undefined &&
    (rule.label === undefined || typeof rule.label === "string") &&
    (rule.sha256 === undefined || (typeof rule.sha256 === "string" && /^[0-9a-f]{64}$/.test(rule.sha256)))
  );
}

function wordMatches(glob: string, word: string): boolean {
  if (!/[*?]/.test(glob)) return glob === word;
  const source = glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[\\s\\S]*").replace(/\?/g, "[\\s\\S]");
  return new RegExp(`^${source}$`).test(word);
}

function restMatches(pattern: readonly string[], words: readonly string[]): boolean {
  if (pattern.length === 0) return words.length === 0;
  const [head, ...tail] = pattern;
  if (head === "**") {
    for (let skip = 0; skip <= words.length; skip++) if (restMatches(tail, words.slice(skip))) return true;
    return false;
  }
  return words.length > 0 && wordMatches(head, words[0]) && restMatches(tail, words.slice(1));
}

/**
 * Whether `pattern` matches a simple command. The program matches by name
 * (`git` matches `/usr/bin/git`); a pattern naming a path matches that path.
 */
export function patternMatches(pattern: readonly string[], words: readonly string[]): boolean {
  if (words.length === 0) return false;
  const [program, ...rest] = pattern;
  const run = words[0];
  const programOk = program.includes("/") ? program === run : basename(run) === program;
  return programOk && restMatches(rest, words.slice(1));
}

/** A pattern as people read it. */
export function describePattern(pattern: readonly string[]): string {
  return pattern.map((word) => (word === "**" ? "…" : /[\s'"]/.test(word) ? JSON.stringify(word) : word)).join(" ");
}

/**
 * The file a command's program resolves to, as bash would find it: a path
 * relative to `cwd`, else the first executable on `PATH`. Undefined for a
 * builtin or a program that is not there.
 */
export function resolveProgram(program: string, cwd: string, path = process.env.PATH ?? ""): string | undefined {
  const candidates = program.includes("/") ? [isAbsolute(program) ? program : resolve(cwd, program)] : path.split(delimiter).filter(Boolean).map((dir) => join(dir, program));
  for (const candidate of candidates) {
    try {
      if (!statSync(candidate).isFile()) continue;
      accessSync(candidate, constants.X_OK);
      return realpathSync.native(candidate);
    } catch {
      // Not here.
    }
  }
  return undefined;
}

export function fileSha256(path: string): string | undefined {
  try {
    return createHash("sha256").update(readFileSync(path)).digest("hex");
  } catch {
    return undefined;
  }
}
