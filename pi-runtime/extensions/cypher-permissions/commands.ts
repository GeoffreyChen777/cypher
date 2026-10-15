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
