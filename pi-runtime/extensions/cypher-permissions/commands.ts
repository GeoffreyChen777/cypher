/**
 * Command rules: which commands run outside the sandbox (`allow`), always
 * ask first (`ask`), or never run (`deny`).
 *
 * A pattern is a list of words matched against a simple command's words
 * (shell.ts): `*` and `?` glob within one word, `**` stands for any number of
 * words. Its first word names the program and is literal; shells,
 * interpreters, wrappers and the builtins that change the shell cannot be
 * one (a rule on them would cover any command). The engine validates the
 * same way (`crates/engine/src/pi/permissions.rs`); keep the lists in step.
 */

import { createHash } from "node:crypto";
import { accessSync, constants, readFileSync, realpathSync, statSync } from "node:fs";
import { basename, delimiter, isAbsolute, join, resolve } from "node:path";

import { UNKNOWN } from "./shell.ts";

export type CommandDecision = "allow" | "ask" | "deny";

export interface CommandRule {
  match: string[];
  decision: CommandDecision;
  /** What the rule is for, in the words of whoever approved it. */
  label?: string;
  /** The program's contents, for a program that lives in a writable place. */
  sha256?: string;
  createdAt?: string;
  /** The project it applies in (`projectIdentity` in core.ts); every project when absent. */
  project?: string;
  /** Variables an `allow` may see assigned in front of the command (`NAME=value cmd`). */
  env?: string[];
  /** Set by the user in Settings: the rule may vouch for a dangerous command (risk.ts). */
  acknowledgeDanger?: boolean;
}

export const MAX_PATTERN_WORDS = 32;
const MAX_WORD_CHARS = 512;

/**
 * Programs a pattern cannot start with: each can run any other command, or
 * (the builtins) change the variables and lookups of the commands after it.
 */
export const BANNED_PROGRAMS: ReadonlySet<string> = new Set([
  "sh", "bash", "zsh", "dash", "ksh", "csh", "tcsh", "fish", "nu", "pwsh", "powershell",
  "env", "sudo", "doas", "su", "xargs", "eval", "exec", "command", "builtin", "nohup", "time", "timeout", "nice", "watch", "script", "parallel", "caffeinate",
  "python", "python2", "python3", "node", "deno", "bun", "bunx", "npx", "pnpx", "uvx", "pipx", "ruby", "perl", "php", "lua", "tclsh", "Rscript", "osascript", "expect",
  "awk", "gawk", "sed", "find",
  "source", ".", "trap", "export", "declare", "typeset", "readonly", "local", "unset", "set", "shopt", "enable", "hash", "alias", "read", "mapfile", "readarray", "let", "getopts",
]);

/**
 * Builtins that only change directory, test or print: they never need the
 * sandbox's leave, so a line may hold them beside escalated commands.
 */
export const INERT_PROGRAMS: ReadonlySet<string> = new Set([
  "cd", "pushd", "popd", "pwd", "true", "false", ":", "echo", "printf", "sleep", "test", "[", "exit", "return", "break", "continue", "wait", "shift",
]);

/** `set` options that change only how the shell reports and stops (`set -euo pipefail`). */
const QUIET_SET_OPTIONS = new Set(["pipefail", "errexit", "nounset", "xtrace", "verbose", "noclobber"]);

/**
 * Whether a command never needs the sandbox's leave: a bare redirection, an
 * inert builtin (not `printf -v`, which sets a variable), or `set` with
 * options that change nothing the commands after it run with.
 */
export function isInert(words: readonly string[]): boolean {
  if (words.length === 0) return true;
  const [program, ...rest] = words;
  if (program === "printf") return !rest.some((word) => word === UNKNOWN || word.startsWith("-v"));
  if (program === "set") {
    for (let i = 0; i < rest.length; i++) {
      const word = rest[i];
      if (/^[-+][euxvC]+$/.test(word)) continue;
      if ((word === "-o" || word === "+o") && QUIET_SET_OPTIONS.has(rest[i + 1] ?? "")) {
        i++;
        continue;
      }
      if (/^[-+][euxvC]*o$/.test(word) && QUIET_SET_OPTIONS.has(rest[i + 1] ?? "")) {
        i++;
        continue;
      }
      return false;
    }
    return true;
  }
  return INERT_PROGRAMS.has(program);
}

// --- Filters -------------------------------------------------------------------

/** How a filter's options read: flags, options that take a value, and long options. */
interface FilterOptions {
  flags: string;
  values?: string;
  long?: RegExp;
  /** `-5` for `-n 5` (head, tail). */
  count?: boolean;
}

/** One option word at `args[at]`: the index of its last word (its value may be the next), and the values it sets. */
function readOption(args: readonly string[], at: number, options: FilterOptions): { last: number; values: [string, string][] } | undefined {
  const word = args[at];
  if (word.startsWith("--")) return options.long?.test(word) ? { last: at, values: [] } : undefined;
  if (options.count && /^-\d+$/.test(word)) return { last: at, values: [] };
  for (let i = 1; i < word.length; i++) {
    const flag = word[i];
    if (options.values?.includes(flag)) {
      if (i < word.length - 1) return { last: at, values: [[flag, word.slice(i + 1)]] };
      const value = args[at + 1];
      return value === undefined ? undefined : { last: at + 1, values: [[flag, value]] };
    }
    if (!options.flags.includes(flag)) return undefined;
  }
  return { last: at, values: [] };
}

/**
 * A filter's words read by its options: its operands, and each option's
 * value. GNU tools read options after operands too, BSD ones read them as
 * files: such words count as both.
 */
function readOptions(args: readonly string[], options: FilterOptions): { operands: string[]; values: [string, string][] } | undefined {
  const operands: string[] = [];
  const values: [string, string][] = [];
  for (let i = 0; i < args.length; i++) {
    const word = args[i];
    if (word === "--") {
      operands.push(...args.slice(operands.length > 0 ? i : i + 1));
      break;
    }
    if (!word.startsWith("-") || word === "-") {
      operands.push(word);
      continue;
    }
    const option = readOption(args, i, options);
    if (!option) return undefined;
    values.push(...option.values);
    if (operands.length > 0) operands.push(...args.slice(i, option.last + 1));
    i = option.last;
  }
  return { operands, values };
}

/** A filter whose operands are all files it reads. */
function readsOperands(options: FilterOptions, most = Infinity): (args: readonly string[]) => string[] | undefined {
  return (args) => {
    const read = readOptions(args, options);
    return read && read.operands.length <= most ? read.operands : undefined;
  };
}

/**
 * Filters, each reading its words into the files it reads, or undefined
 * when it could do more: run a command, write a file, read one its
 * operands don't name. Options not listed here make it undefined.
 */
const FILTERS: Readonly<Record<string, (args: readonly string[]) => string[] | undefined>> = {
  cat: readsOperands({ flags: "benstuvAET" }),
  head: readsOperands({ flags: "q", values: "nc", count: true }),
  tail: readsOperands({ flags: "qr", values: "nc", count: true }),
  wc: readsOperands({ flags: "clmwL" }),
  // Not -o (writes), -T (temporary files) or --compress-program (runs one).
  sort: readsOperands({ flags: "bdfghiMmnRrsuVcCz", values: "ktS" }),
  // A second operand is a file it writes.
  uniq: readsOperands({ flags: "cdDiuz", values: "fsw" }, 1),
  cut: readsOperands({ flags: "nsz", values: "bcdf" }),
  // Its operands are sets of characters; it reads only its input.
  tr: (args) => {
    const read = readOptions(args, { flags: "cdsCu" });
    return read && read.operands.length <= 2 ? [] : undefined;
  },
  grep: grepInputs,
  egrep: grepInputs,
  fgrep: grepInputs,
  awk: awkInputs,
  gawk: awkInputs,
  mawk: awkInputs,
  nawk: awkInputs,
  sed: sedInputs,
};

/** Not -r or -R (they read directories), -d or -D, nor -f (it reads a file of patterns). */
function grepInputs(args: readonly string[]): string[] | undefined {
  const read = readOptions(args, { flags: "EFGPiyvwxcLlqsHhnbouzaIUZ", values: "eABCm", long: /^--colou?r(=(never|always|auto))?$/ });
  if (!read) return undefined;
  // Without -e, the first operand is the pattern.
  return read.values.some(([flag]) => flag === "e") ? read.operands : read.operands.slice(1);
}

/** Only -F and -v: not -f (a program file), nor gawk's options that load or include code. */
function awkInputs(args: readonly string[]): string[] | undefined {
  const read = readOptions(args, { flags: "", values: "Fv" });
  if (!read) return undefined;
  const [program, ...operands] = read.operands;
  if (program === undefined || !awkProgramOnlyFilters(program)) return undefined;
  // `name=value` operands assign; the rest are files.
  return operands.filter((operand) => !/^[A-Za-z_]\w*=/.test(operand));
}

/** Only -n, -E, -r, -s, -u, -z and -e: not -i (writes its files) nor -f (a script file). */
function sedInputs(args: readonly string[]): string[] | undefined {
  const read = readOptions(args, { flags: "nErsuz", values: "e" });
  // An option after an operand: GNU sed reads it, BSD sed reads the first operand as the script.
  if (!read || read.operands.some((operand) => operand.startsWith("-") && operand !== "-")) return undefined;
  const scripts = read.values.map(([, value]) => value);
  const operands = scripts.length > 0 ? read.operands : read.operands.slice(1);
  if (scripts.length === 0) scripts.push(read.operands[0] ?? "");
  return scripts.every(sedScriptOnlyFilters) ? operands : undefined;
}

/**
 * Words an awk program may not hold anywhere, even in a string: what runs
 * a command or reads a file (`system`, `getline`), changes which files it
 * reads (`ARGV`, `ARGC`), reads the environment, or is gawk's way to load
 * code or call a function by name (`@`).
 */
const AWK_BANNED = /system|getline|ARGV|ARGC|ENVIRON|PROCINFO|SYMTAB|FUNCTAB|@/;

/**
 * Where a regular expression that starts at `start` (its delimiter) ends:
 * the index of its closing delimiter. Undefined when it does not end on its
 * line, or a delimiter stands inside a bracket expression (`[/]`), where
 * implementations end it differently.
 */
function regexEnd(text: string, start: number): number | undefined {
  const delimiter = text[start];
  let bracket = false;
  for (let i = start + 1; i < text.length; i++) {
    const c = text[i];
    if (c === "\n") return undefined;
    if (c === "\\") i++;
    else if (!bracket && c === delimiter) return i;
    else if (bracket && c === delimiter) return undefined;
    else if (!bracket && c === "[") {
      bracket = true;
      // `]` first in a bracket expression is literal.
      if (text[i + 1] === "^") i++;
      if (text[i + 1] === "]") i++;
    } else if (bracket && c === "[" && /[:.=]/.test(text[i + 1] ?? "")) {
      // A class like `[:space:]`.
      const close = text.indexOf(`${text[i + 1]}]`, i + 2);
      if (close < 0 || /[\n]/.test(text.slice(i, close)) || text.slice(i, close).includes(delimiter)) return undefined;
      i = close + 1;
    } else if (bracket && c === "]") bracket = false;
  }
  return undefined;
}

/** Words after which a `/` starts a regular expression, not a division. */
const AWK_KEYWORDS: ReadonlySet<string> = new Set([
  "BEGIN", "END", "BEGINFILE", "ENDFILE", "function", "func", "if", "else", "while", "for", "do", "break", "continue",
  "next", "nextfile", "exit", "return", "delete", "in", "getline", "print", "printf", "switch", "case", "default",
]);

/**
 * Whether an awk program only turns its input into output: none of
 * [`AWK_BANNED`], no pipe (`print | "cmd"`, `"cmd" | getline`), no
 * output redirection (`print > "file"`). Strings, regular expressions and
 * comments are skipped as awk reads them; a `/` inside a bracket
 * expression, which awks end a regular expression at differently, makes it
 * false.
 */
export function awkProgramOnlyFilters(program: string): boolean {
  if (AWK_BANNED.test(program)) return false;
  /** The last token ends an operand: a `/` after it divides. */
  let operand = false;
  /** Open parentheses; each says whether it holds an `if`, `while` or `for` condition. */
  const parens: boolean[] = [];
  /** The last word was `if`, `while` or `for`: its `(` holds a condition. */
  let control = false;
  /** Inside a `print` or `printf`, at this many open parentheses: a `>` there redirects. */
  let printing: number | undefined;
  for (let i = 0; i < program.length; i++) {
    const c = program[i];
    if (c === " " || c === "\t") continue;
    const wasControl = control;
    control = false;
    if (c === '"') {
      for (i++; i < program.length && program[i] !== '"'; i++) {
        if (program[i] === "\n") return false;
        if (program[i] === "\\") i++;
      }
      if (i >= program.length) return false;
      operand = true;
    } else if (c === "/" && !operand) {
      const end = regexEnd(program, i);
      if (end === undefined) return false;
      i = end;
      operand = true;
    } else if (c === "#") {
      while (i + 1 < program.length && program[i + 1] !== "\n") i++;
    } else if (c === "\\") {
      // Only a line continuation.
      if (program[++i] !== "\n") return false;
    } else if (/[A-Za-z_]/.test(c)) {
      let end = i;
      while (end + 1 < program.length && /\w/.test(program[end + 1])) end++;
      const word = program.slice(i, end + 1);
      i = end;
      operand = !AWK_KEYWORDS.has(word);
      control = word === "if" || word === "while" || word === "for";
      if (word === "print" || word === "printf") printing = parens.length;
    } else if (/[0-9.]/.test(c)) {
      const number = /^(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?/.exec(program.slice(i))?.[0];
      if (!number) return false;
      i += number.length - 1;
      // `1print`: awk reads a number, then a word.
      if (/\w/.test(program[i + 1] ?? "")) return false;
      operand = true;
    } else if (c === "(") {
      parens.push(wasControl);
      operand = false;
    } else if (c === ")") {
      if (parens.length === 0) return false;
      // After an `if (…)` comes a statement, which may start with a regular expression.
      operand = !parens.pop();
    } else if (c === "]" || c === "$") {
      operand = c === "]";
    } else if ((c === "+" || c === "-") && program[i + 1] === c) {
      i++;
      operand = true;
    } else if (c === "|") {
      if (program[i + 1] !== "|") return false;
      i++;
      operand = false;
    } else if (c === ">") {
      if (printing === parens.length) return false;
      operand = false;
    } else {
      if (c === ";" || c === "\n" || c === "{" || c === "}") printing = undefined;
      operand = false;
    }
  }
  return parens.length === 0;
}

/** sed commands that only print, delete or hold lines (not a, i, c, r, R, w, W, e, b, t or labels). */
const SED_COMMANDS = "pPdDnNgGhHxlqQz=";

/**
 * Whether a sed script only turns its input into output: addresses and
 * [`SED_COMMANDS`], `{ … }`, `y` and `s` with flags g, p, i, I, m, M and
 * a number (not w or e). A delimiter inside a bracket expression, which
 * seds end at differently, makes it false.
 */
export function sedScriptOnlyFilters(script: string): boolean {
  let i = 0;
  const space = () => {
    while (script[i] === " " || script[i] === "\t") i++;
  };
  /** Past a delimited part (the delimiter at `i`) to its closing delimiter; false if it does not end on its line. */
  const delimited = (regex: boolean): boolean => {
    let end: number | undefined;
    if (regex) end = regexEnd(script, i);
    else {
      for (let at = i + 1; at < script.length && end === undefined; at++) {
        if (script[at] === "\n") return false;
        if (script[at] === "\\") at++;
        else if (script[at] === script[i]) end = at;
      }
    }
    if (end === undefined) return false;
    i = end + 1;
    return true;
  };
  const delimiterOk = (c: string | undefined) => c !== undefined && c !== "\n" && c !== "\\" && c !== " ";
  /** Past an address, if one is here; false if it is malformed. */
  const address = (): boolean => {
    if (/[0-9]/.test(script[i] ?? "")) {
      while (/[0-9~]/.test(script[i] ?? "")) i++;
    } else if (script[i] === "$") i++;
    else if (script[i] === "/" || script[i] === "\\") {
      if (script[i] === "\\") i++;
      if (!delimiterOk(script[i]) || !delimited(true)) return false;
      while (script[i] === "I" || script[i] === "M") i++;
    }
    return true;
  };
  while (i < script.length) {
    const c = script[i];
    if (c === " " || c === "\t" || c === "\n" || c === ";" || c === "}") {
      i++;
      continue;
    }
    if (!address()) return false;
    if (script[i] === ",") {
      i++;
      if (script[i] === "+" || script[i] === "~") i++;
      if (!address()) return false;
    }
    space();
    if (script[i] === "!") i++;
    space();
    const command = script[i++];
    if (command === "{") continue;
    if (command === "s" || command === "y") {
      if (!delimiterOk(script[i]) || !delimited(command === "s")) return false;
      i--;
      if (!delimited(false)) return false;
      if (command === "s") while (/[gpiImM0-9]/.test(script[i] ?? "")) i++;
    } else if (command !== undefined && SED_COMMANDS.includes(command)) {
      if (/[qQl]/.test(command)) {
        space();
        while (/[0-9]/.test(script[i] ?? "")) i++;
      }
    } else return false;
    space();
    if (i < script.length && !/[;\n}]/.test(script[i])) return false;
  }
  return true;
}

/**
 * The files a filter reads, when that is all it does with its words: it
 * reads them or its input and writes its output, and runs nothing.
 * Undefined for any other command, or a filter given a word known only
 * when it runs. `-` stands for its input. The caller checks the files, and
 * that the program is the system's.
 */
export function filterInputs(words: readonly string[]): string[] | undefined {
  const [program, ...args] = words;
  if (program === undefined || program.includes("/") || words.includes(UNKNOWN) || !Object.hasOwn(FILTERS, program)) return undefined;
  return FILTERS[program](args)?.filter((operand) => operand !== "-");
}

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
    (rule.sha256 === undefined || (typeof rule.sha256 === "string" && /^[0-9a-f]{64}$/.test(rule.sha256))) &&
    (rule.project === undefined || (typeof rule.project === "string" && rule.project.startsWith("/"))) &&
    (rule.env === undefined || (Array.isArray(rule.env) && rule.env.every((name) => typeof name === "string" && /^[A-Za-z_][A-Za-z0-9_]*$/.test(name)))) &&
    (rule.acknowledgeDanger === undefined || typeof rule.acknowledgeDanger === "boolean")
  );
}

// --- Environment and wrappers ------------------------------------------------------

/**
 * Variables that change how programs load or run code, or where they look
 * for it: assigning one in front of a command changes what the command is,
 * so no rule can vouch for it and the reader treats the line as opaque.
 */
const DANGEROUS_ENV =
  /^(PATH|\w*PATH|LD_\w*|DYLD_\w*|\w*_OPTIONS|\w*OPTS|\w*FLAGS|\w*_HOME|\w*ROOT|PYTHON\w*|RUBY\w*|PERL\w*|NODE_\w*|NPM_CONFIG_\w*|YARN_\w*|BUNDLE_\w*|BASH_ENV|ENV|IFS|HOME|XDG_\w*|SHELL|SHELLOPTS|BASHOPTS|EDITOR|VISUAL|\w*PAGER|GIT_\w*|SSH_\w*|\w*_PROXY|\w*_COMMAND|\w*_WRAPPER|RUSTC\w*|CARGO_\w*|CC|CXX|CPP|LD|AR|MAKE|MAKEFLAGS|LESSOPEN|LESSCLOSE|TMPDIR|PROMPT_COMMAND|PS4|CDPATH|GLOBIGNORE|HISTFILE|BROWSER|TERMINFO|\w*ASKPASS|DEVELOPER_DIR|SDKROOT|TOOLCHAINS|DOCKER_\w*|COMPOSE_\w*|KUBE\w*|AWS_\w*|GOOGLE_\w*|CLOUDSDK_\w*|AZURE_\w*|\w*TOKEN\w*|\w*SECRET\w*|\w*PASSWORD\w*|\w*_KEY)$/i;

/** Common settings that only tune output or a build, in families the list above names. */
const BENIGN_ENV =
  /^(NODE_ENV|NODE_NO_WARNINGS|RUST_LOG|RUST_BACKTRACE|RUST_MIN_STACK|CARGO_INCREMENTAL|CARGO_TERM_\w+|CARGO_TARGET_DIR|CARGO_BUILD_JOBS|CARGO_PROFILE_\w+_(DEBUG|OPT_LEVEL|INCREMENTAL|CODEGEN_UNITS|LTO|STRIP|SPLIT_DEBUGINFO)|PYTHONUNBUFFERED|PYTHONDONTWRITEBYTECODE|PYTHONIOENCODING|PYTHONHASHSEED|GIT_TERMINAL_PROMPT)$/;

/** Whether a variable may stand assigned in front of a command: it does not change how programs load or run. */
export function benignEnv(name: string): boolean {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) return false;
  return BENIGN_ENV.test(name) || !DANGEROUS_ENV.test(name);
}

/** A command seen through the wrappers that only run it (`nohup`, `timeout 30`, `env NAME=value`). */
export interface Unwrapped {
  words: string[];
  /** The wrappers' program words, outermost first. */
  wrappers: string[];
  /** Variables `env` assigns. */
  env: string[];
}

const TIMEOUT_VALUES = new Set(["-s", "--signal", "-k", "--kill-after"]);

/**
 * The command a wrapper runs, when it only runs it: `nohup`, `time [-p]`,
 * `nice [-n N]`, `timeout [options] DURATION`, `caffeinate [-dimsu] [-t N]
 * [-w PID]`, `env [-i] [-u NAME] [NAME=value]…`. Undefined when the wrapper
 * does more (`env -S` splits a string into a command), its words are not
 * all known, or `env` assigns a variable that changes how programs run.
 * The caller checks that each wrapper is the system's program.
 */
export function unwrap(words: readonly string[]): Unwrapped | undefined {
  let rest = [...words];
  const wrappers: string[] = [];
  const env: string[] = [];
  for (let depth = 0; depth < 8; depth++) {
    const program = rest[0];
    if (program === undefined || program === UNKNOWN) return undefined;
    const name = basename(program);
    let i = 1;
    switch (name) {
      case "nohup":
        break;
      case "time":
        while (rest[i] === "-p") i++;
        break;
      case "nice":
        if (rest[i] === "-n") i += 2;
        else if (/^(-\d+|-n\d+|--adjustment=-?\d+)$/.test(rest[i] ?? "")) i++;
        break;
      case "timeout":
        for (; i < rest.length && rest[i].startsWith("-"); i++) {
          if (TIMEOUT_VALUES.has(rest[i])) i++;
          else if (!/^(--preserve-status|--foreground|-v|--verbose|--signal=\S+|--kill-after=\S+|-s\S+|-k\S+)$/.test(rest[i])) return undefined;
        }
        if (!/^\d+(\.\d+)?[smhd]?$/.test(rest[i] ?? "")) return undefined;
        i++;
        break;
      case "caffeinate":
        for (; i < rest.length && rest[i].startsWith("-"); i++) {
          if (rest[i] === "-t" || rest[i] === "-w") i++;
          else if (!/^-[dimsu]+$/.test(rest[i])) return undefined;
        }
        break;
      case "env":
        for (; i < rest.length; i++) {
          const word = rest[i];
          if (word === "-i" || word === "-" || word === "--ignore-environment") continue;
          if (word === "-u" || word === "--unset") {
            i++;
            continue;
          }
          if (word === UNKNOWN || word.startsWith("-")) return undefined;
          const assignment = /^([A-Za-z_][A-Za-z0-9_]*)=/.exec(word);
          if (!assignment) break;
          if (!benignEnv(assignment[1])) return undefined;
          env.push(assignment[1]);
        }
        break;
      default:
        return depth === 0 ? undefined : { words: rest, wrappers, env };
    }
    if (rest.slice(1, i).includes(UNKNOWN)) return undefined;
    wrappers.push(program);
    rest = rest.slice(i);
  }
  return undefined;
}

function wordMatches(glob: string, word: string): boolean {
  if (word === UNKNOWN) return false;
  if (!/[*?]/.test(glob)) return glob === word;
  const source = glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[\\s\\S]*").replace(/\?/g, "[\\s\\S]");
  return new RegExp(`^${source}$`).test(word);
}

function restMatches(pattern: readonly string[], words: readonly string[], worst: boolean): boolean {
  if (worst && words[0] === UNKNOWN) {
    // It may be any words: those the pattern wants next, or none.
    for (let take = 0; take <= pattern.length; take++) if (restMatches(pattern.slice(take), words.slice(1), worst)) return true;
    return false;
  }
  if (pattern.length === 0) return words.length === 0;
  const [head, ...tail] = pattern;
  if (head === "**") {
    for (let skip = 0; skip <= words.length; skip++) if (restMatches(tail, words.slice(skip), worst)) return true;
    return false;
  }
  return words.length > 0 && wordMatches(head, words[0]) && restMatches(tail, words.slice(1), worst);
}

/**
 * Whether `pattern` matches a simple command. The program matches by name
 * (`git` matches `/usr/bin/git`); a pattern naming a path matches that path.
 *
 * A word known only when the line runs ([`UNKNOWN`]) matches only `**`: an
 * `allow` rule vouches for what it can see. With `worst`, for `ask` and
 * `deny` rules, it matches whatever the rule wants: the rule applies if the
 * command could turn out to be one it names.
 */
export function patternMatches(pattern: readonly string[], words: readonly string[], worst = false): boolean {
  if (words.length === 0) return false;
  const [program, ...rest] = pattern;
  const run = words[0];
  const programOk = program.includes("/") ? program === run : basename(run) === program;
  return programOk && restMatches(rest, words.slice(1), worst);
}

// --- Categories ------------------------------------------------------------------

/**
 * CLIs whose first word says what they do (`git push`, `gh pr`, `xcrun
 * simctl`): their category is that word. Each lists its options before that
 * word that take a value.
 */
const SUBCOMMANDS: Readonly<Record<string, readonly string[]>> = {
  git: ["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env"],
  gh: ["-R", "--repo", "--hostname"],
  glab: ["-R", "--repo"],
  docker: ["-H", "--host", "-c", "--context", "--config", "-l", "--log-level"],
  podman: ["-c", "--connection", "--url", "--identity"],
  kubectl: ["-n", "--namespace", "--context", "--cluster", "--kubeconfig", "-s", "--server", "--user"],
  helm: ["-n", "--namespace", "--kube-context", "--kubeconfig"],
  aws: ["--profile", "--region", "--output", "--endpoint-url"],
  gcloud: ["--project", "--account", "--configuration"],
  az: ["--subscription"],
  doctl: ["-t", "--access-token", "--context"],
  fly: ["-a", "--app"],
  flyctl: ["-a", "--app"],
  wrangler: [],
  vercel: ["--scope", "--token"],
  netlify: [],
  supabase: [],
  firebase: ["--project"],
  heroku: [],
  terraform: [],
  tofu: [],
  pulumi: [],
  brew: [],
  port: [],
  npm: ["--prefix", "-w", "--workspace"],
  pnpm: ["-C", "--dir", "-F", "--filter"],
  yarn: ["--cwd"],
  cargo: [],
  go: [],
  swift: [],
  dotnet: [],
  gem: [],
  pip: [],
  pip3: [],
  uv: ["--directory", "--project"],
  poetry: [],
  bundle: [],
  pod: [],
  fastlane: [],
  gradle: [],
  mix: [],
  make: ["-C", "-f", "--directory", "--file"],
  just: ["-f", "--justfile", "-d", "--working-directory"],
  xcrun: ["--sdk", "--toolchain"],
  launchctl: [],
  systemctl: ["-H", "--host", "-M", "--machine"],
  defaults: [],
  security: [],
  tailscale: ["--socket"],
  tmux: ["-L", "-S", "-f"],
};

/** The options before a CLI's subcommand that take a value (`git -C dir push`). */
export function globalValueOptions(program: string): readonly string[] {
  return SUBCOMMANDS[program] ?? [];
}

/**
 * Subcommands whose next word picks what runs (`npm run build`, `uv run
 * pytest`): their category includes that word too. `xcrun` runs the tool
 * its first word names.
 */
const RUNS_NEXT: Readonly<Record<string, ReadonlySet<string>>> = {
  npm: new Set(["run", "run-script", "exec", "x"]),
  pnpm: new Set(["run", "exec", "dlx"]),
  yarn: new Set(["run", "exec", "dlx"]),
  uv: new Set(["run"]),
  poetry: new Set(["run"]),
  bundle: new Set(["exec"]),
};

/**
 * Programs that change, read or send whatever their words name: a category
 * (`rm …`) would allow any of it, so they are allowed only as written.
 */
const AS_WRITTEN: ReadonlySet<string> = new Set([
  "rm", "rmdir", "mv", "cp", "ln", "install", "chmod", "chown", "chgrp", "chflags", "xattr", "dd", "truncate", "shred", "srm", "tee", "touch", "mkdir",
  "cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "ag", "strings", "xxd", "hexdump", "od", "base64", "sqlite3", "plutil", "zip", "unzip", "tar", "gzip", "gunzip", "ditto",
  "curl", "wget", "nc", "ncat", "socat", "telnet", "ftp", "scp", "sftp", "rsync",
  "kill", "pkill", "killall", "open", "pbcopy", "pbpaste", "screencapture", "tccutil", "csrutil", "diskutil", "mount", "umount", "hdiutil", "crontab", "at",
  "gpg", "ssh-add", "ssh-keygen", "openssl", "printenv", "op", "pass", "vault", "keychain",
]);

/** ssh options that take a value (`-p 22`, `-p22`). */
const SSH_VALUE_OPTIONS = "BbcDEeFIiJLlmOoPpQRSWw";

/**
 * The pattern a command's category is: what "allow for this chat" covers
 * when the agent proposed none. The program and what it does (`git push …`,
 * `xcrun simctl …`, `npm run build …`), the host for ssh (`ssh deploy@web
 * …`), any use of a script named by its path, any use of another program;
 * the command as written for programs whose words are the risk
 * ([`AS_WRITTEN`]). When no rule could name it, why not.
 */
export function categoryPattern(words: readonly string[]): string[] | string {
  const [program, ...args] = words;
  if (program === undefined || program === UNKNOWN || /[*?[]/.test(program)) return "a program in it is known only when it runs";
  if (BANNED_PROGRAMS.has(basename(program))) return anyCommand(program);
  if (program.includes("/")) return [program, "**"];
  if (INERT_PROGRAMS.has(program)) return `\`${program}\` never needs to leave the sandbox`;
  if (program === "ssh") return sshCategory(words);
  if (AS_WRITTEN.has(program)) return asWritten(words);
  const valueOptions = SUBCOMMANDS[program];
  if (!valueOptions) return [program, "**"];
  const prefix: string[] = [program];
  let i = 0;
  for (; i < args.length && args[i].startsWith("-"); i++) {
    prefix.push(args[i]);
    if (valueOptions.includes(args[i])) prefix.push(args[++i] ?? "");
  }
  const subcommand = args[i];
  if (subcommand === undefined) return asWritten(words);
  prefix.push(subcommand);
  if (program === "xcrun" && BANNED_PROGRAMS.has(subcommand)) return anyCommand(subcommand);
  if (RUNS_NEXT[program]?.has(subcommand)) {
    const next = args[i + 1];
    if (next === undefined || next === UNKNOWN || next.startsWith("-")) return `\`${program} ${subcommand}\` is not followed by what it runs`;
    if (BANNED_PROGRAMS.has(basename(next))) return anyCommand(next);
    prefix.push(next);
  }
  return literal(prefix) ? [...prefix, "**"] : computed(program);
}

function anyCommand(program: string): string {
  return `\`${basename(program)}\` can run any command, so no rule may name it`;
}

function computed(program: string): string {
  return `\`${program}\` is given words known only when it runs`;
}

/** `ssh [options] host …`: the options as written, the host, any command. */
function sshCategory(words: readonly string[]): string[] | string {
  const prefix = [words[0]];
  let i = 1;
  for (; i < words.length && words[i].startsWith("-") && words[i] !== "-"; i++) {
    prefix.push(words[i]);
    const flags = words[i].slice(1);
    const at = [...flags].findIndex((flag) => SSH_VALUE_OPTIONS.includes(flag));
    if (at === flags.length - 1) prefix.push(words[++i] ?? "");
  }
  const host = words[i];
  if (host === undefined) return "`ssh` is given no host";
  prefix.push(host);
  return literal(prefix) ? [...prefix, "**"] : computed("ssh");
}

function asWritten(words: readonly string[]): string[] | string {
  return literal(words) ? [...words] : computed(words[0]);
}

/** Words a pattern can hold as they are: known, with no glob characters. */
function literal(words: readonly string[]): boolean {
  return words.every((word) => word !== UNKNOWN && word.length > 0 && !/[*?]/.test(word) && word !== "**");
}

/**
 * Why no rule may vouch for a command, whatever it allows: an ssh, scp or
 * sftp option that runs a command on this computer (`ProxyCommand`,
 * `LocalCommand`, a config file, another ssh program), or options ssh reads
 * after the host. Undefined when a rule may.
 */
export function ruleCannotVouch(words: readonly string[]): string | undefined {
  const name = basename(words[0] ?? "");
  if (name !== "ssh" && name !== "scp" && name !== "sftp") return undefined;
  const risky = `${name} is given an option that can run a command on this computer`;
  for (let i = 1; i < words.length; i++) {
    const word = words[i];
    if (word === "--") break;
    if (word === UNKNOWN) return risky;
    if (!word.startsWith("-") || word === "-") {
      if (name !== "ssh") continue;
      // ssh reads options again straight after the host.
      if (words[i + 1]?.startsWith("-")) return risky;
      break;
    }
    const flags = word.slice(1);
    for (let at = 0; at < flags.length; at++) {
      const flag = flags[at];
      if (flag === "F" || (flag === "S" && name !== "ssh")) return risky;
      if (flag === "o") {
        const value = at < flags.length - 1 ? flags.slice(at + 1) : (words[++i] ?? "");
        if (value === UNKNOWN || /(proxycommand|localcommand|knownhostscommand|match|include)/i.test(value)) return risky;
        break;
      }
      if (SSH_VALUE_OPTIONS.includes(flag)) {
        if (at === flags.length - 1) i++;
        break;
      }
    }
  }
  return undefined;
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
