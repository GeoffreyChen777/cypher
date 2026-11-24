/**
 * The permission engine for one Pi process: configuration, zones, grants, the
 * approval pipeline (policy → grant → judge → human) and the per-call checks
 * the file tools run at the filesystem boundary. Nothing here imports Pi
 * values, so the whole pipeline runs under `node --test`.
 */

import { execFileSync, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { constants, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, readlinkSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { open, mkdir, access as fsAccess, stat as fsStat, readdir as fsReaddir } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve as resolvePath } from "node:path";
import { AuditLog } from "./audit.ts";
import type { AuditRecord } from "./audit.ts";
import { appendCommandRule, appendRule, approvalMode as approvalModeFor, CONFIG_FILE, editUserConfig, loadConfig, ownFolders, readUserConfig, resolveLevel } from "./config.ts";
import { CREATED_ENTRY, CreatedStore, creationsOf } from "./created.ts";
import type { CreationFacts, CreationPlan } from "./created.ts";
import type { CatalogueFacts } from "./catalogue.ts";
import { genericDestroyer, targetsOf } from "./catalogue.ts";
import { readTool, remoteEffect } from "./tools.ts";
import { entryCovers, Ledger, ledgerFile, ledgerKind, taintFile } from "./ledger.ts";
import type { LedgerEntity, LedgerEntry } from "./ledger.ts";
import { Denials, denialsFile, forRecord } from "./denials.ts";
import type { ActionClass, DenialRecord, DenialScope } from "./denials.ts";
import { commandEffects, unwrapped } from "./effects.ts";
import type { EffectClass } from "./effects.ts";
import type { LifecycleFacts } from "./lifecycle.ts";
import type { PullRequestFact } from "./entities.ts";
import type { Capability, LevelName, PermissionsConfig } from "./config.ts";
import {
  anchorDirectories,
  dropExceptions,
  expandAbsolute,
  findRipgrep,
  HomeIndexCache,
  listMatching,
  ripgrepGlobs,
  splitPatterns,
} from "./discover.ts";
import { globBase, hasGlob } from "./glob.ts";
import { GRANT_ENTRY, GrantStore, newGrant, REVOKE_ENTRY, ROOT_ENTRY } from "./grants.ts";
import type { Grant, GrantScope } from "./grants.ts";
import { checkDataConsent, checkExplicit, consentText, describeAttribute, describeTarget, lineTargets, mentions, repoName, scanConsents, scanDataConsent } from "./explicit.ts";
import type { ExplicitClaim, ExplicitFacts, ExplicitResult, LineTargets, OwnerWords, Target, TargetFacts } from "./explicit.ts";
import { answeredByConsent, conversationContext, judgePrompt, JUDGE_SYSTEM_PROMPT, parseVerdict } from "./judge.ts";
import type { Concern, JudgeEvidence, JudgeRequest } from "./judge.ts";
import { floorOf } from "./floor.ts";
import type { FloorHit, FloorSubject } from "./floor.ts";
import { describeGrade, gradesOf, kindGrades } from "./grades.ts";
import { isUnder, PathContext } from "./paths.ts";
import type { Classification } from "./paths.ts";
import { decide, describe } from "./policy.ts";
import type { Verdict } from "./policy.ts";
import { compilePolicy, landstripStatus, splitSelf, startFilterProxy, tempDirs } from "./sandbox.ts";
import type { CompiledPolicy, ConcretePaths, FilterProxy, LandstripStatus } from "./sandbox.ts";
import { benignEnv, quietSsh, categoryPattern, describePattern, fileSha256, filterInputs, isInert, patternError, patternMatches, resolveProgram, ruleCannotVouch, unwrap } from "./commands.ts";
import type { CommandRule } from "./commands.ts";
import { assessCommand, assessOpaque, downloadsAndRuns, exfiltrates, installedProgram, lineFlow, normalRefspec, redirectRisk, runtimeWriteSets, secretName, secretWrites, sinkOf, TIER_RANK, unresolvedWrites } from "./risk.ts";
import type { PatternFinding } from "./risk.ts";
import { globsIntersect, resolvePattern, resolveText } from "./resolve.ts";
import { codeOf, INTERPRETERS as INTERPRETER_TABLE, interpreterOf, optionCommands, scanCode, shebangInterpreter } from "./code.ts";
import type { Interpreter } from "./code.ts";
import type { Assessment, GitFacts, KillTarget, ProcessFacts, RiskFacts, Tier } from "./risk.ts";
import { INSPECT, inspectMatches, viewOf } from "./packs.ts";
import { literalOutput, parseShell, UNKNOWN } from "./shell.ts";
import type { ParsedLine, SimpleCommand } from "./shell.ts";

export const UNAVAILABLE_ENTRY = "cypher-permissions.unavailable";

export interface UiLike {
  select(title: string, options: string[], opts?: { signal?: AbortSignal; timeout?: number }): Promise<string | undefined>;
  notify(message: string, type?: "info" | "warning" | "error"): void;
}

type Entry = { type?: string; customType?: string; data?: unknown; message?: { role?: string; content?: unknown } };

/** What a request needs from Pi: the UI, the session, the judge's model and a way to stop the turn. */
export interface RequestContext {
  cwd: string;
  hasUI: boolean;
  ui: UiLike;
  signal?: AbortSignal;
  branch(): readonly Entry[];
  /** One completion from the judge model; undefined when it gives none, or it throws with why. */
  complete?(system: string, prompt: string, signal: AbortSignal): Promise<string | undefined>;
  abort?(): void;
  /** The tool call a dialog gates, so Cypher files the dialog under it. */
  toolCallId?: string;
}

/**
 * How permission control treated one tool call: the `cypherPermissions`
 * record in its result's details, which Cypher shows on the call's row
 * (`cypher_proto::ToolPermission`).
 */
export interface PermissionRecord {
  /** `outside` the sandbox, `blocked` by it, `denied`, or a file access `allowed`. */
  outcome: "outside" | "blocked" | "denied" | "allowed";
  /** How long the approval lasts; `rule` when an earlier grant or rule allowed it. */
  scope?: "once" | "chat" | "always" | "rule";
  /** The command rule that let it run outside. */
  rule?: string;
  /** What the agent said the command was for. */
  purpose?: string;
}

/**
 * The last line of every permission dialog's title: what it is about, for
 * Cypher (`crates/harness/src/pi/wire.rs` `permission_prompt` strips it).
 */
const PROMPT_MARK = "cypher-permission: ";

/**
 * One `Label: value` line of a permission dialog's context. The value's own
 * line breaks (a multi-line command, an agent's reason) are indented, so the
 * fact stays one fact and nothing inside it can pass for a label, a
 * `Change:` block or the prompt line (`crates/ui/src/composer/permission_card.rs`
 * `parse`).
 */
export function fact(label: string, value: string): string {
  return `${label}: ${value.replace(/\r\n?/g, "\n").split("\n").join("\n  ")}`;
}

function promptLine(kind: "escalate" | "access" | "network" | "tool", toolCallId: string | undefined): string {
  return `\n${PROMPT_MARK}${JSON.stringify({ kind, ...(toolCallId ? { toolCallId } : {}) })}`;
}

export interface AccessRequest {
  capability: Capability;
  /** As the tool received it (relative, `~`, `@`). */
  path: string;
  /** The change, for writes the judge should see. */
  diff?: string;
}

export interface CallInfo {
  tool: string;
  toolCallId?: string;
  command?: string;
  justification?: string;
}

export type AuthResult =
  /** `record`: set when an access went past the defaults (asked, or a grant or rule). */
  | { ok: true; approved: Approved; record?: PermissionRecord }
  | { ok: false; reason: string; abort?: boolean };

/** Canonical paths approved for one call, by capability. Write implies read. */
export class Approved {
  readonly read = new Set<string>();
  readonly write = new Set<string>();

  add(capability: Capability, path: string): void {
    this.read.add(path);
    if (capability === "write") this.write.add(path);
  }

  covers(capability: Capability, path: string): boolean {
    return capability === "read" ? this.read.has(path) : this.write.has(path);
  }

  /** `mkdir -p` of a directory on the way to an approved write. */
  coversParentOf(path: string): boolean {
    for (const approved of this.write) if (isUnder(approved, path)) return true;
    return false;
  }
}

export class PermissionDenied extends Error {
  readonly code = "EACCES";
}

const HUMAN = {
  once: "Allow once",
  chat: "Allow for this chat",
  always: "Always allow",
  deny: "Deny",
  stop: "Deny and stop",
} as const;

const DOMAIN_ONCE = "Allow for this command";

const RUN_ONCE = "Run once";

/** Local work: once, beside "Always in <project>". */
const ONCE = "Once";

/** The prefix of the option that keeps a rule for this project. */
const ALWAYS_IN = "Always in ";

/** Dangerous work: until the user's next message. */
const TASK = "Allow for this task";

/**
 * A prompt that came back without a button: closed, timed out, or cut off by the turn ending. It is
 * not a refusal (round 3): only Deny (and Deny and stop) is. It is audited as `dismissed` (never
 * `deny`), keeps nothing, and the agent is told it was not answered rather than refused.
 */
function unanswered(choice: string | undefined, started: number, timeoutMs: number | null | undefined, signal: AbortSignal | undefined): "dismissed" | "timeout" | "aborted" | undefined {
  if (choice !== undefined && choice !== "") return undefined;
  if (signal?.aborted) return "aborted";
  if (typeof timeoutMs === "number" && Date.now() - started >= timeoutMs) return "timeout";
  return "dismissed";
}

/** What the agent is told about an unanswered prompt. */
const UNANSWERED = "Not run: the owner closed the permission prompt without answering. That is not a refusal; don't repeat it on your own. Ask them whether they want it, or continue without it.";


/** A reviewing model that failed to sign in (an expired session, a missing key). */
const AUTH_FAILURE = /authenticat|unauthori[sz]ed|\b401\b|\b403\b|expired|log ?in|sign ?in|credential|api key|forbidden/i;

/** The variable each command the agent runs carries (`ownsProcesses`). */
export const PROCESS_MARKER = "CYPHER_AGENT_CHAT";

/** How long an approval lasts: a grant's scope, or a rule for this project, or an existing grant or rule. */
export type Scope = GrantScope | "project" | "rule";

/** How a review went: the decision, whether an approval keeps nothing, and the model's answer for the audit. */
interface Review {
  decision: "approve" | "ask-user";
  once: boolean;
  reason: string;
  state: "approved" | "asked" | "unavailable" | "paused";
  audit?: Pick<AuditRecord, "judgeVerdict">;
  /** What the model quoted and named, for a danger (explicit.ts checks it). */
  claim?: ExplicitClaim;
  /** The concern an ask named (judge.ts `CONCERNS`). */
  concern?: Concern;
}

/**
 * Whether the owner's own words, as the code checked them (`JudgeEvidence`), answer a concern the judge
 * asked with (judge.ts `CONCERN_TABLE` `answeredByConsent`): a consent of a kind the code vouches for by
 * itself (typed, a plan, its card or an answer; a reply's assent is the judge's call), covering every
 * target the line names, not taken back since; for the installed app's data, owner words naming it.
 * Returns the words, or undefined. Round 3: "what the owner's previous turn already asked for, the judge
 * must let through".
 */
/** A denial, for the judge's evidence. */
function describeDenial(denial: DenialRecord): string {
  return `${denial.what} (denied ${denial.at.slice(0, 16).replace("T", " ")} UTC; same action, on ${denial.scopes.slice(0, 3).map((scope) => (scope.kind === "line" ? "the line itself" : `${scope.kind} ${scope.says?.[0] ?? scope.name}`)).join(", ")})`;
}

/** A file, domain or tool request in words, for a denial's record. */
function describeRequest(request: { capability: string; tool: string; domain?: string; toolInput?: string }, paths: readonly string[], home: string): string {
  if (request.domain !== undefined) return `reaching ${request.domain}`;
  if (request.toolInput !== undefined) return `the ${request.tool} tool (${request.toolInput.length > 120 ? `${request.toolInput.slice(0, 120)}…` : request.toolInput})`;
  return `${request.tool} ${request.capability} of ${paths.map((path) => displayPath(path, home)).join(", ")}`;
}

export function consentAnswers(concern: Concern | undefined, evidence: JudgeEvidence | undefined): string | undefined {
  if (concern === undefined || !answeredByConsent(concern) || !evidence) return undefined;
  if (concern === "installed-app" && evidence.installedData) return evidence.installedData.finding.ok ? evidence.installedData.finding.quote : undefined;
  return evidence.consents?.find((finding) => finding.ok && !finding.unread && finding.source !== "reply")?.quote;
}

type ElevateOutcome = { allowed: true; scope: Scope; by: Grant["approvedBy"] } | { allowed: false; reason: string; abort?: boolean };

export type EscalationOutcome =
  | { ok: true; scope: Scope; by: "judge" | "user" | "rule"; onceOnly?: string }
  | { ok: false; reason: string; abort?: boolean };

/** A pattern a dialog offers, with what it carries. */
interface Offered {
  match: string[];
  sha256?: string;
  /** Variables the commands it covers assign in front of them. */
  env: string[];
  /** Dangers it covers (a task grant). */
  dangers: string[];
  /** The agent proposed it, rather than it being a command's category. */
  proposed?: boolean;
}

/** A line's tier, with its commands as read. */
export interface LineAssessment extends Assessment {
  parsed: ParsedLine;
  commands: LineCommand[];
  /** One per command, in order. */
  assessments?: Assessment[];
  /** Code it runs that Cypher can't read (code.ts), in words: evidence "opaque code" for the judge. */
  opaqueCode?: string[];
  /** Paths it writes whose start is known only when it runs and that could be the permission system (risk.ts `unresolvedWrites`). */
  unresolved?: string[];
}

/** What the code a command runs does (code.ts, `Permissions.codeFindings`). */
interface CodeFinding {
  language: string;
  /** Where the code is: "its inline code", "the code on its input", "the script x.py". */
  origin: string;
  sink?: string;
  secret?: string;
  /** Why it can't be read. */
  opaque?: string;
  /** A line it runs is on the floor (`subprocess.run("cat .env | nc …")`): that line's finding. */
  floor?: Assessment;
  /** The most dangerous line it runs (`subprocess.run(["git", "push", "--force"])`), when one is. */
  worst?: Assessment;
}

/** How deep code inside code is read (a script that runs a shell line that runs python …). */
const MAX_CODE_DEPTH = 3;
/** The most of a script that is read as code. */
const MAX_SCRIPT_BYTES = 512 * 1024;
/** The most shell lines one piece of code is read as running. */
const MAX_CODE_COMMANDS = 32;

/** A command's assessment with what its code does: unreadable code is a danger for the judge; a secret it reads and doesn't send, a secret read. */
function withCode(own: Assessment, found: CodeFinding): Assessment {
  if (own.tier === "danger") return own;
  if (found.floor) return { tier: "danger", kind: found.floor.kind, key: found.floor.key, floor: true };
  if (found.sink && found.secret) return { tier: "danger", kind: `sends a secret off this machine (${found.secret}) through ${found.sink}`, key: "secret-off-machine", floor: true };
  if (found.worst) return { ...found.worst, kind: `${found.origin} runs a command that ${found.worst.kind ?? "is dangerous"}` };
  if (found.opaque) return { tier: "danger", kind: `runs code Cypher can't read (${found.origin}: ${found.opaque})`, key: "opaque-code" };
  if (found.secret && !found.sink) return { tier: "local", kind: `reads a secret (${found.secret}) in ${found.language} code`, once: true, concern: "secret" };
  return own;
}

/** The scope Cypher's row shows: a project rule lasts like "always", a task grant like "once". */
function recordScope(scope: Scope): NonNullable<PermissionRecord["scope"]> {
  if (scope === "project") return "always";
  if (scope === "task") return "once";
  return scope;
}

function describeScope(scope: Scope, project: string): string {
  if (scope === "project") return `always in ${project}`;
  if (scope === "task") return "for this task";
  return scope;
}

/** The git directories of a repository's submodules (`modules/<name>`, a name with slashes nested, and their own submodules). */
function submoduleGitDirs(gitDir: string): string[] {
  const out: string[] = [];
  const walk = (dir: string, depth: number) => {
    if (depth > 6) return;
    let names: string[];
    try {
      names = readdirSync(dir);
    } catch {
      return;
    }
    for (const name of names) {
      const child = join(dir, name);
      try {
        if (!statSync(child).isDirectory()) continue;
      } catch {
        continue;
      }
      if (existsSync(join(child, "HEAD"))) {
        out.push(child);
        walk(join(child, "modules"), depth + 1);
      } else walk(child, depth + 1);
    }
  };
  walk(join(gitDir, "modules"), 0);
  return out;
}

/** A rule matches a command as written, or without ssh options that change nothing here (`quietSsh`). */
function matchesQuietly(pattern: readonly string[], words: readonly string[], worst = false): boolean {
  return patternMatches(pattern, words, worst) || patternMatches(pattern, quietSsh(words), worst);
}

/** The user messages on a branch. */
function userTurns(entries: readonly Entry[]): number {
  return entries.filter((entry) => entry.type === "message" && entry.message?.role === "user").length;
}

/** A process's environment, as `NAME=value` text; undefined when it cannot be read. */
function processEnvironment(pid: number): string | undefined {
  if (!Number.isInteger(pid) || pid <= 0) return undefined;
  if (process.platform === "linux") {
    try {
      return readFileSync(`/proc/${pid}/environ`, "latin1").split("\0").join("\n");
    } catch {
      return undefined;
    }
  }
  // macOS: `ps -E` appends the environment to the command line, for the user's own processes.
  const result = spawnSync("ps", ["-wwE", "-o", "command=", "-p", String(pid)], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
  if (result.status !== 0 || !result.stdout.trim()) return undefined;
  // macOS hides the environment of its own binaries (`sleep`): no variable at all means unreadable.
  const words = result.stdout.trim().split(/\s+/);
  return words.some((word) => /^[A-Za-z_][A-Za-z0-9_]*=/.test(word)) ? words.join("\n") : undefined;
}

/** The installed app on macOS. */
const INSTALLED_APP = "/Applications/Cypher.app";

/** A process's program and working directory, each undefined when it cannot be read. */
function processPlace(pid: number): { program?: string; cwd?: string } {
  if (process.platform === "linux") {
    const link = (name: string) => {
      try {
        return readlinkSync(`/proc/${pid}/${name}`);
      } catch {
        return undefined;
      }
    };
    return { program: link("exe"), cwd: link("cwd") };
  }
  const run = (program: string, args: string[]) => {
    const result = spawnSync(program, args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
    return result.status === 0 ? result.stdout : undefined;
  };
  // `lsof -Fn` prints `p<pid>`, then `fcwd`/`ftxt` and `n<path>` per file.
  const files = run("lsof", ["-a", "-p", String(pid), "-d", "cwd,txt", "-Fn"]) ?? "";
  let cwd: string | undefined;
  let program: string | undefined;
  let fd: string | undefined;
  for (const line of files.split("\n")) {
    if (line.startsWith("f")) fd = line.slice(1);
    else if (line.startsWith("n") && fd === "cwd") cwd ??= line.slice(1);
    else if (line.startsWith("n") && fd === "txt") program ??= line.slice(1);
  }
  program ??= run("ps", ["-o", "comm=", "-p", String(pid)])?.trim();
  return { program: program && isAbsolute(program) ? program : undefined, cwd };
}

/** How often the reviewing model is asked before its silence or failure goes to the user: once, and one retry. */
const JUDGE_ATTEMPTS = 2;

const ESCALATE_HINT =
  "If the command needs what the sandbox withholds (files outside the project folders, credentials, keys, the keychain, other processes, system services), run it again with `escalate`: a reviewing model or the user decides whether it runs outside the sandbox.";

/** What command rules say about one bash call. */
export type CommandCheck =
  /** A `deny` rule matches: the call never runs. */
  | { kind: "deny"; reason: string }
  /** Every command in it is allowed outside the sandbox: by rules, or as an inspection (`inspected`). */
  | { kind: "outside"; rules: CommandRule[]; inspected?: boolean; tier?: Tier; why?: string }
  /** It runs in the sandbox unless an escalation is approved. */
  | { kind: "sandbox"; tier?: Tier; why?: string };

/** One command of a bash line, seen through its wrappers, with the directory it runs in (undefined: not known). */
interface LineCommand extends SimpleCommand {
  cwd: string | undefined;
  /** It runs code from its input that is known only when it runs (risk.ts). */
  stdinCode?: boolean;
  inert: boolean;
  /** The wrappers that only run it (`nohup`, `timeout 30`). */
  wrappers: string[];
  /** Values assigned in front of it and its wrappers' words. */
  carried: string[];
}

const DIRECTORY_BUILTINS: ReadonlySet<string> = new Set(["cd", "pushd", "popd"]);

/** A danger's kind or key, by the action class a consent must ask for (lexicon.ts words). */
const UNREAD_ACTIONS: readonly [string, RegExp][] = [
  ["delete", /delet|remov|discard|destroy|wipe|prune|erase|\brm\b|undoes/i],
  ["stop", /stop|kill/i],
  ["push", /push|publish to|force/i],
  ["publish", /publish|release/i],
  ["merge", /merge|pull request/i],
  ["install", /install/i],
  ["apply", /deploy|appl|scale|chang/i],
  ["write", /write|writes/i],
  ["reach", /send|reach|upload/i],
];

/** The command a command runs (effects.ts `unwrapped`: `sudo`, `env`, `xargs`, …; and what `find -exec` runs). */
function runWords(words: readonly string[]): string[] {
  const inner = unwrapped(words);
  if (/^(find|fd)$/.test(basename(inner[0] ?? ""))) {
    const at = inner.findIndex((word) => /^-(exec|execdir|ok|okdir|x|X)$|^--exec/.test(word));
    if (at >= 0) {
      const run = inner.slice(at + 1);
      const end = run.findIndex((word) => word === ";" || word === "+");
      return unwrapped(end < 0 ? run : run.slice(0, end));
    }
  }
  return inner;
}

/** A script's text (round 5), or why it can't be read: not there, too large, a binary. */
function readScript(path: string): { text?: string; why?: string } {
  try {
    const stat = statSync(path);
    if (!stat.isFile()) return { why: "it isn't a file" };
    if (stat.size > MAX_SCRIPT_BYTES) return { why: `it is too large to read (${stat.size} bytes)` };
    const buffer = readFileSync(path);
    if (buffer.subarray(0, 8000).includes(0)) return { why: "it is a binary" };
    return { text: buffer.toString("utf8") };
  } catch {
    return { why: "it isn't there to read" };
  }
}

/** The interpreter a script's extension names (`.py` → python3), by code.ts `INTERPRETERS`. */
function extensionInterpreter(path: string): string | undefined {
  const dot = path.lastIndexOf(".");
  if (dot < 0) return undefined;
  const extension = path.slice(dot);
  const row = INTERPRETER_TABLE.find((candidate) => candidate.extensions.includes(extension));
  return row ? (row.language === "python" ? "python3" : row.language === "javascript" ? "node" : row.language === "shell" ? "sh" : row.language === "applescript" ? "osascript" : row.language === "r" ? "Rscript" : row.language === "tcl" ? "tclsh" : row.language) : undefined;
}

/** Shells and interpreters that run code from their input (code.ts `INTERPRETERS`): no script, no inline code. */
function readsCode(words: readonly string[]): boolean {
  return codeOf(words, UNKNOWN)?.source.from === "stdin";
}

/**
 * Code with the values the command's own assignments give its environment (round 5 step 2: `D=~/.cypher/…
 * node -e "fs.writeFileSync(process.env.D+'/permissions.json', …)"`): each read of such a variable, in any
 * language's spelling (`ENV_READ`), becomes the value's string literal, so the scanner sees the path it builds.
 */
const ENV_READ = (name: string) =>
  new RegExp(`(?:process\\.env\\.${name}\\b|process\\.env\\[['"]${name}['"]\\]|os\\.environ\\[['"]${name}['"]\\]|os\\.environ\\.get\\(\\s*['"]${name}['"]\\s*\\)|os\\.getenv\\(\\s*['"]${name}['"]\\s*\\)|ENV\\[['"]${name}['"]\\]|\\$ENV\\{${name}\\}|Sys\\.getenv\\(\\s*['"]${name}['"]\\s*\\)|getenv\\(\\s*['"]${name}['"]\\s*\\)|Deno\\.env\\.get\\(\\s*['"]${name}['"]\\s*\\)|\\$env:${name}\\b)`, "g");

function withAssignments(text: string, item: { env?: readonly string[]; envValues?: readonly string[] }): string {
  let out = text;
  for (const [index, name] of (item.env ?? []).entries()) {
    const value = item.envValues?.[index];
    if (value === undefined || value === UNKNOWN || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) continue;
    out = out.replace(ENV_READ(name), () => JSON.stringify(value));
  }
  return out;
}

/** How many entries and folders deep a folder is searched for a secret inside it (`secretInside`). */
const SECRET_SCAN_LIMIT = 4000;
const SECRET_SCAN_DEPTH = 8;

/** Variables bash itself reads: a line that assigns one changes how what follows runs. */
const SHELL_VARIABLES =
  /^(BASH\w*|LC_\w+|HIST\w*|COMP\w*|READLINE_\w+|EPOCH\w+|PATH|IFS|CDPATH|HOME|PWD|OLDPWD|ENV|GLOBIGNORE|EXECIGNORE|FIGNORE|PS[0-4]|PROMPT_COMMAND|TMPDIR|POSIXLY_CORRECT|SHELLOPTS|LANG|TIMEFORMAT|OPTIND|OPTERR|OPTARG|FUNCNEST|MAIL|MAILPATH|MAILCHECK|TMOUT|SHELL|UID|EUID|PPID|RANDOM|SRANDOM|SECONDS|LINENO|GROUPS|DIRSTACK|PIPESTATUS|HOSTNAME|HOSTTYPE|OSTYPE|MACHTYPE|COLUMNS|LINES|REPLY|INPUTRC|auto_resume|histchars)$/;

/** What the agent proposes when it asks to leave the sandbox. */
export interface EscalationProposal {
  /** Patterns that later matching commands may run outside without asking. */
  patterns?: string[][];
  /** What the commands are for, in plain words. */
  label?: string;
  reason?: string;
}

const ELEVATE_HINT =
  "If the access is needed, call request_permission with the paths (and, for bash, the exact command), then retry.";

export interface PermissionsInit {
  agentDir: string;
  chatId?: string;
  cwd: string;
  levelEnv?: string;
  home?: string;
  /** Tests skip the Landstrip probe. */
  landstrip?: LandstripStatus;
  /** Scratch space that counts as workspace; the temp directories by default (tests keep their fixtures out). */
  scratch?: readonly string[];
  /** The owner's own words, as the engine passed them (`CYPHER_OWNER_WORDS`); undefined when it passed none. */
  ownerWords?: OwnerWords;
  /**
   * The task this chat belongs to (round 6, W3): a workspace's id for it and its threads, a plan's for its
   * sessions (`CYPHER_TASK_ID`). Chats of one task are never each other's "other task".
   */
  taskId?: string;
}

/**
 * Every way a request can be allowed (round 6, W1). Each goes through `Permissions.gate` before it allows:
 * the owner's denials (denials.ts) and the floor's `before` rows (floor.ts) are consulted on all of them.
 * A request with no matching denial and no floor row passes as before: no prompt, no judge call.
 */
export const ENTRY_POINTS = {
  "file:policy": "a file tool's access the policy allows by default (a read in the project, a write in the workspace)",
  "file:grant": "a file tool's access an earlier approval covers",
  "file:rule": "a file tool's access a saved rule covers",
  "request_permission": "paths asked for ahead that are already allowed",
  "tool:setting": "an MCP or extension tool whose setting is allow",
  "tool:approval": "an MCP or extension tool an earlier approval or rule covers",
  "bash:outside": "a command a saved rule allows outside the sandbox, or one that only inspects",
  "bash:sandbox": "a command that runs in the sandbox",
  "bash:escalate": "a command run outside the sandbox on review",
} as const;
export type EntryPoint = keyof typeof ENTRY_POINTS;

/** The file tools (the extension's own, and apply_patch) and the access each asks for. */
export const FILE_TOOL_ACCESS: Readonly<Record<string, Capability>> = { read: "read", ls: "read", grep: "read", find: "read", write: "write", edit: "write", apply_patch: "write" };

function gitRoots(cwd: string): { top?: string; gitDirs: string[]; gitDir?: string; common?: string } {
  try {
    const out = execFileSync("git", ["rev-parse", "--show-toplevel", "--absolute-git-dir", "--git-common-dir"], {
      cwd,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: 5000,
    })
      .trim()
      .split("\n");
    const [top, gitDir, common] = out;
    const resolveDir = (dir: string | undefined) => {
      if (!dir) return undefined;
      try {
        return realpathSync.native(dir.startsWith("/") ? dir : join(cwd, dir));
      } catch {
        return undefined;
      }
    };
    let own = resolveDir(gitDir);
    let shared = resolveDir(common);
    const root = resolveDir(top);
    // A linked worktree's git directory points back at it (`gitdir`): a
    // `.git` file that names another repository's directory without that
    // back-pointer is not this checkout's, and earns none of its trust.
    if (own && root && own !== join(root, ".git")) {
      const resolved = (path: string | undefined) => {
        try {
          return path ? realpathSync.native(resolvePath(own!, path)) : undefined;
        } catch {
          return undefined;
        }
      };
      let back: string | undefined;
      try {
        back = resolved(dirname(readFileSync(join(own, "gitdir"), "utf8").trim()));
      } catch {
        // A submodule's git directory names its checkout in core.worktree instead.
        try {
          back = resolved(/^\s*worktree\s*=\s*(.+)$/m.exec(readFileSync(join(own, "config"), "utf8"))?.[1]?.trim());
        } catch {
          back = undefined;
        }
      }
      if (back !== root) {
        own = undefined;
        shared = undefined;
      }
    }
    if (!own) return { gitDirs: [] };
    return {
      top: root,
      gitDir: own,
      common: shared,
      gitDirs: [...new Set([own, shared].filter((dir): dir is string => dir !== undefined))],
    };
  } catch {
    return { gitDirs: [] };
  }
}

export class Permissions {
  readonly agentDir: string;
  readonly chatId: string | undefined;
  readonly home: string;
  readonly audit: AuditLog;
  readonly grants: GrantStore;
  /** What this task made (created.ts): the facts a danger over its own leavings rests on. */
  readonly created: CreatedStore;
  /** The owner's Deny clicks in this chat, by action class (denials.ts). */
  readonly denials: Denials;
  /** The task this chat belongs to (`PermissionsInit.taskId`). */
  readonly taskId: string | undefined;
  /** Each `gate` call's entry point, for tests (round 6, W1): undefined unless a test sets it to a list. */
  gated: { entry: EntryPoint; held?: string }[] | undefined;
  readonly status: LandstripStatus;
  config!: PermissionsConfig;
  level!: LevelName;
  paths!: PathContext;
  warnings: string[] = [];
  /** A command prefix from the settings runs before every command (it may export variables). */
  shellPrefix = false;
  private readonly levelEnv: string | undefined;
  /**
   * The owner's own words (explicit.ts): what a person typed in this chat,
   * and the instruction a plan carries out. The only text a danger may run
   * on without a dialog. Undefined when the engine passed none.
   */
  ownerWords: OwnerWords | undefined;
  /**
   * Set by the extension: the owner's words as the engine knows them now.
   * A Pi process outlives the turn it was started for, so what it started
   * with lacks every message the owner sent since. Undefined result: none
   * newer could be had, and the words at hand stand.
   */
  ownerWordsNow?: () => Promise<OwnerWords | undefined>;
  private cwd: string;
  private extraRoots: string[] = [];
  private baseRoots: string[] = [];
  private gitDirs: string[] = [];
  /** A linked worktree's own git directory and the repository's, when they differ (`compile` lets git write them). */
  private worktreeGit: { gitDir: string; common: string } | undefined;
  /**
   * The project rules apply in: the repository (its shared git directory's
   * parent, so every worktree of a repository shares its rules), else the
   * working directory.
   */
  project = "";
  private readonly scratchDirs: readonly string[];
  /** The user's file as last read, to notice another process's edits. */
  private configStamp = "";
  private readonly homeIndex: HomeIndexCache;
  private approvalQueue: Promise<unknown> = Promise.resolve();
  /** Approvals of in-flight calls the Runtime patch hooks consult (apply_patch). */
  readonly inflight = new Map<string, Approved>();
  private sensitiveCache: { at: number; key: string; paths: ConcretePaths } | undefined;
  private proxy: Promise<FilterProxy | undefined> | undefined;
  private readonly proxyToken = randomBytes(18).toString("base64url");
  /** Running bash calls, for the filter proxy's domain questions (latest last). */
  private readonly commands = new Map<string, { command: string; ctx: RequestContext; domains: Set<string> }>();

  constructor(init: PermissionsInit) {
    this.scratchDirs = init.scratch ?? tempDirs();
    this.agentDir = init.agentDir;
    this.chatId = init.chatId;
    this.home = init.home ?? homedir();
    this.levelEnv = init.levelEnv;
    this.ownerWords = init.ownerWords;
    this.cwd = init.cwd;
    this.audit = new AuditLog(init.agentDir, init.chatId);
    this.grants = new GrantStore(init.chatId);
    this.created = new CreatedStore(init.chatId, ledgerFile(init.agentDir, init.chatId));
    this.taskId = init.taskId ?? (process.env.CYPHER_TASK_ID || undefined);
    this.created.ledger.task = this.taskId;
    this.taint = new Ledger(init.agentDir ? taintFile(init.agentDir, init.chatId) : undefined, init.chatId);
    this.denials = new Denials(denialsFile(init.agentDir, init.chatId), init.chatId);
    this.status = init.landstrip ?? landstripStatus();
    this.homeIndex = new HomeIndexCache(join(init.agentDir, "cypher-permissions", "home-index.json"), this.home);
    this.reload(init.cwd, []);
  }

  /** Whether the extension enforces anything on this host and level. */
  get enforcing(): boolean {
    return this.status.available && this.level !== "full-access";
  }

  get roots(): readonly string[] {
    return this.paths.roots;
  }

  /** Re-read the configuration and the session's grants and roots. */
  reload(cwd: string, entries: readonly Entry[]): void {
    this.cwd = cwd;
    let canonicalCwd = cwd;
    try {
      canonicalCwd = realpathSync.native(cwd);
    } catch {
      // A missing cwd stays as given.
    }
    const git = gitRoots(canonicalCwd);
    this.gitDirs = git.gitDirs;
    this.worktreeGit = git.gitDir && git.common && git.gitDir !== git.common ? { gitDir: git.gitDir, common: git.common } : undefined;
    this.project = git.common ? (basename(git.common) === ".git" ? dirname(git.common) : git.common) : canonicalCwd;
    this.gitFacts.clear();
    this.baseRoots = [...new Set([canonicalCwd, ...(git.top ? [git.top] : [])])];
    this.extraRoots = this.rootsFromEntries(entries);
    const roots = [...new Set([...this.baseRoots, ...this.extraRoots])];
    this.loadConfigFor(roots);
    this.created.restore(entries);
    this.grants.restore(entries);
    this.grants.turn = userTurns(entries);
  }

  private loadConfigFor(roots: string[]): void {
    this.configStamp = this.readStamp();
    const loaded = loadConfig(this.agentDir, roots);
    this.config = loaded.config;
    this.warnings = loaded.warnings;
    this.level = resolveLevel(this.levelEnv, this.config);
    this.paths = new PathContext({
      roots,
      sensitive: this.config.sensitive,
      protectedPatterns: this.config.protected,
      self: this.config.self,
      writableSupport: this.config.writableSupport,
      permissionSystem: this.config.permissionSystem,
      scratch: this.scratchDirs,
      // A workspace bot's folder lies in Cypher's data: for its own chats it is the project.
      ownFolders: ownFolders(roots, this.agentDir),
      home: this.home,
    });
    this.sensitiveCache = undefined;
  }

  private readStamp(): string {
    try {
      const stat = statSync(join(this.agentDir, CONFIG_FILE), { bigint: true });
      return `${stat.mtimeNs}:${stat.ctimeNs}:${stat.ino}:${stat.size}`;
    } catch {
      return "";
    }
  }

  /** Re-read the user's file when another process changed it (a rule saved in another chat). */
  refresh(): void {
    if (this.readStamp() !== this.configStamp) this.loadConfigFor([...this.paths.roots]);
  }

  /** The user messages on the branch: a task grant lasts until the next one. */
  setTurn(entries: readonly Entry[]): void {
    this.grants.turn = userTurns(entries);
  }

  private rootsFromEntries(entries: readonly Entry[]): string[] {
    const roots: string[] = [];
    for (const entry of entries) {
      if (entry.type !== "custom" || entry.customType !== ROOT_ENTRY) continue;
      const data = entry.data as { path?: string; action?: string; chatId?: string } | undefined;
      if (!data || typeof data.path !== "string") continue;
      if (this.chatId && data.chatId !== this.chatId) continue;
      const index = roots.indexOf(data.path);
      if (data.action === "remove") {
        if (index >= 0) roots.splice(index, 1);
      } else if (index < 0) roots.push(data.path);
    }
    return roots;
  }

  get extraWorkspaceRoots(): readonly string[] {
    return this.extraRoots;
  }

  classify(path: string, cwd = this.cwd): Classification {
    return this.paths.classify(this.paths.canonical(path, cwd));
  }

  verdict(capability: Capability, classification: Classification): Verdict {
    return decide(this.config, this.level, classification, capability);
  }

  private ruleCovers(capability: Capability, path: string): boolean {
    // No rule lets the permission system's own files be written (the always-ask floor).
    if (this.systemWrite(capability, path)) return false;
    return this.config.rules.some((rule) => (capability === "read" || rule.capability === "write") && isUnder(path, rule.path) && this.ruleApplies(rule));
  }

  /** A write to one of the permission system's own files (config.ts): only the user approves one, file by file. */
  private systemWrite(capability: Capability, path: string): boolean {
    return capability === "write" && this.paths.isPermissionSystem(path) !== undefined;
  }

  /**
   * A grant that lets a file tool touch this path. One of the permission
   * system's own files is covered only by a grant the user gave for that
   * very file: approving a folder never hands over the policy inside it.
   */
  private fileGrant(capability: Capability, path: string): Grant | undefined {
    if (!this.systemWrite(capability, path)) return this.grants.findForFile(capability, path);
    return this.grants.all().find((grant) => grant.pattern === undefined && grant.capability === "write" && grant.approvedBy === "user" && grant.paths.includes(path));
  }

  /**
   * The synchronous check every filesystem operation runs: may this call touch
   * this canonical path? Throws PermissionDenied otherwise.
   */
  assert(capability: Capability, path: string, approved: Approved | undefined): string {
    const canonical = this.paths.canonical(path, this.cwd);
    if (!this.enforcing) return canonical;
    const classification = this.paths.classify(canonical);
    const verdict = this.verdict(capability, classification);
    if (verdict.kind === "allow") return canonical;
    if (approved?.covers(capability, canonical)) return canonical;
    for (const flight of this.inflight.values()) if (flight.covers(capability, canonical)) return canonical;
    const grant = this.fileGrant(capability, canonical);
    if (grant) return canonical;
    if (this.ruleCovers(capability, canonical) && classification.zone !== "sensitive") return canonical;
    const reason = verdict.kind === "deny" ? verdict.reason : `${describe(classification)} needs approval to ${capability}`;
    throw new PermissionDenied(`Permission denied: ${reason}. ${ELEVATE_HINT}`);
  }

  /** `mkdir -p` towards an approved write. */
  assertMkdir(path: string, approved: Approved | undefined): string {
    const canonical = this.paths.canonical(path, this.cwd);
    if (approved?.coversParentOf(canonical)) return canonical;
    return this.assert("write", path, approved);
  }

  /** Serialize dialogs so parallel tool calls never stack approvals on the user. */
  private serialized<T>(task: () => Promise<T>): Promise<T> {
    const next = this.approvalQueue.then(task, task);
    this.approvalQueue = next.catch(() => undefined);
    return next;
  }

  /** What "Always in …" names: the project's folder. */
  get projectName(): string {
    return basename(this.project) || this.project;
  }

  /**
   * Authorize one tool call's accesses before it runs, asking when needed.
   * The returned approvals cover exactly the canonical paths requested.
   */
  async authorize(requests: readonly AccessRequest[], call: CallInfo, ctx: RequestContext): Promise<AuthResult> {
    const approved = new Approved();
    if (!this.enforcing) return { ok: true, approved };
    this.refresh();
    // The widest approval this call used, for its row in Cypher.
    const order = ["rule", "once", "chat", "always"] as const;
    let scope: PermissionRecord["scope"];
    const used = (next: NonNullable<PermissionRecord["scope"]>) => {
      if (!scope || order.indexOf(next) > order.indexOf(scope)) scope = next;
    };
    for (const request of requests) {
      const classification = this.classify(request.path, ctx.cwd);
      const canonical = classification.path;
      const verdict = this.verdict(request.capability, classification);
      // Every allow below consults the owner's denials and the floor first (round 6, W1, `gate`): held, it is reviewed.
      const held = verdict.kind === "deny" ? undefined : await this.gate(verdict.kind === "allow" ? "file:policy" : this.fileGrant(request.capability, canonical) ? "file:grant" : "file:rule", this.fileClass(call.tool, request.capability, [canonical]), { permissionSystemPath: request.capability === "write" && this.paths.isPermissionSystem(canonical) !== undefined ? canonical : undefined });
      if (verdict.kind === "allow" && !held) continue;
      if (verdict.kind === "deny") {
        this.audit.write({ tool: call.tool, capability: request.capability, zone: classification.zone, guard: classification.guard, paths: [canonical], decision: "deny", decidedBy: "policy", reason: verdict.reason });
        return { ok: false, reason: `Permission denied: ${verdict.reason}.` };
      }
      const grant = held ? undefined : this.fileGrant(request.capability, canonical);
      if (grant) {
        this.audit.write({ tool: call.tool, capability: request.capability, zone: classification.zone, guard: classification.guard, paths: [canonical], decision: "allow", decidedBy: "grant", grantId: grant.id, scope: grant.scope });
        this.grants.used(grant);
        approved.add(request.capability, canonical);
        used("rule");
        continue;
      }
      if (!held && classification.zone !== "sensitive" && this.ruleCovers(request.capability, canonical)) {
        this.audit.write({ tool: call.tool, capability: request.capability, zone: classification.zone, guard: classification.guard, paths: [canonical], decision: "allow", decidedBy: "rule" });
        approved.add(request.capability, canonical);
        used("rule");
        continue;
      }
      const outcome = await this.serialized(async () => {
        // An earlier dialog in the queue may have allowed it meanwhile.
        if (!held && (this.fileGrant(request.capability, canonical) || (classification.zone !== "sensitive" && this.ruleCovers(request.capability, canonical)))) {
          return { allowed: true as const, scope: "rule" as const, by: "rule" as const };
        }
        return this.elevate(
          {
            capability: request.capability,
            classifications: [classification],
            key: verdict.kind === "elevate" ? verdict.key : "held",
            mode: verdict.kind === "elevate" && verdict.mode === "assisted" ? "assisted" : "ask",
            held,
            tool: call.tool,
            justification: call.justification,
            diff: request.diff,
          },
          ctx,
        );
      });
      if (!outcome.allowed) return { ok: false, reason: outcome.reason, abort: outcome.abort };
      approved.add(request.capability, canonical);
      used(recordScope(outcome.scope));
      if (outcome.scope !== "once" && outcome.scope !== "rule") this.persistGrant(request.capability, [classification], outcome.scope, outcome.by);
    }
    // What this chat's file tools write is this task's (round 5): code it runs later is read (`taskWrote`).
    if (call.tool === "write" || call.tool === "edit") for (const request of requests) if (request.capability === "write") this.noteWritten(this.classify(request.path, ctx.cwd).path);
    return { ok: true, approved, ...(scope ? { record: { outcome: "allowed", scope } } : {}) };
  }

  /**
   * `request_permission`: ask ahead for paths the file tools will touch.
   * Commands never get extra paths: one that needs them runs outside the
   * sandbox instead (`requestEscalation`). Returns text for the agent.
   */
  async requestPermission(
    input: { capability: Capability; paths?: string[]; reason?: string },
    ctx: RequestContext,
  ): Promise<{ ok: boolean; text: string; abort?: boolean; record?: PermissionRecord }> {
    if (!this.enforcing) return { ok: true, text: "Permission control is not active here; nothing to request." };
    this.refresh();
    const requested = input.paths ?? [];
    if (requested.length === 0) return { ok: false, text: "Name the paths the file tools need." };
    const classifications = requested.map((path) => this.classify(path, ctx.cwd));
    const verdicts = classifications.map((item) => this.verdict(input.capability, item));
    const denied = verdicts.find((verdict) => verdict.kind === "deny");
    if (denied && denied.kind === "deny") {
      this.audit.write({ tool: "request_permission", capability: input.capability, paths: classifications.map((item) => item.path), decision: "deny", decidedBy: "policy", reason: denied.reason });
      return { ok: false, text: `Denied: ${denied.reason}.` };
    }
    const elevations = verdicts.filter((verdict): verdict is Extract<Verdict, { kind: "elevate" }> => verdict.kind === "elevate");
    // Already allowed paths still consult the owner's denials and the floor (round 6, W1).
    const held = elevations.length === 0 ? await this.gate("request_permission", this.fileClass("request_permission", input.capability, classifications.map((item) => item.path))) : undefined;
    if (held) {
      const outcome = await this.serialized(() => this.elevate({ capability: input.capability, classifications, key: "held", mode: "ask", tool: "request_permission", justification: input.reason, held }, ctx));
      if (!outcome.allowed) return { ok: false, text: outcome.reason, abort: outcome.abort };
      return { ok: true, text: `Allowed once: ${classifications.map((item) => item.path).join(", ")}.` };
    }
    if (elevations.length === 0) {
      return { ok: true, text: `Already allowed: ${classifications.map((item) => item.path).join(", ")}. Run the tool again; if it still fails, the cause is not a permission.` };
    }
    // The strictest elevation decides the mode.
    const order = ["sensitive.read", "sensitive.write", "self.write", "protected.write"];
    const key = elevations.map((verdict) => verdict.key).sort((a, b) => (order.indexOf(a) + 1 || 99) - (order.indexOf(b) + 1 || 99))[0];
    const mode = elevations.some((verdict) => verdict.mode === "ask") ? "ask" : elevations[0].mode;
    const outcome = await this.serialized(() =>
      this.elevate(
        { capability: input.capability, classifications, key, mode: mode === "assisted" ? "assisted" : "ask", tool: "request_permission", justification: input.reason },
        ctx,
      ),
    );
    if (!outcome.allowed) return { ok: false, text: outcome.reason, abort: outcome.abort };
    const scope = outcome.scope === "rule" ? "once" : outcome.scope;
    if (scope === "once") {
      const sensitive = classifications.some((item) => item.zone === "sensitive");
      this.grants.add(newGrant({ capability: input.capability, paths: classifications.map((item) => item.path), sensitive: sensitive || undefined, scope, approvedBy: outcome.by, chatId: this.chatId }));
    } else {
      this.persistGrant(input.capability, classifications, scope, outcome.by);
    }
    return {
      ok: true,
      record: { outcome: "allowed", scope: recordScope(scope) },
      text: `Approved (${describeScope(scope, this.projectName)}) for the file tools (read, write, edit, ls, grep, find): ${classifications.map((item) => item.path).join(", ")}. Run the file tool now. Commands never get this: a command that needs it (ssh, git, a CLI reading credentials) runs again with \`escalate\`.`,
    };
  }

  /** Set by the extension: persist a custom session entry. */
  appendEntry?: (type: string, data: unknown) => void;

  private persistGrant(capability: Capability, classifications: Classification[], scope: Exclude<Scope, "once" | "rule">, by: Grant["approvedBy"]): void {
    const sensitive = classifications.some((item) => item.zone === "sensitive");
    const paths = classifications.map((item) => item.path);
    if (scope === "always" || scope === "project") {
      if (sensitive) return;
      const project = scope === "project" ? this.project : undefined;
      for (const path of paths) {
        const rule = { capability, path, createdAt: new Date().toISOString(), ...(project ? { project } : {}) };
        appendRule(this.agentDir, rule);
        this.config.rules.push(rule);
      }
      this.configStamp = this.readStamp();
      return;
    }
    const grant = newGrant({ capability, paths, sensitive: sensitive || undefined, scope, approvedBy: by, chatId: this.chatId, ...(scope === "task" ? { turn: this.grants.turn } : {}) });
    this.grants.add(grant);
    if (scope === "chat") this.appendEntry?.(GRANT_ENTRY, grant);
  }

  revoke(id: string): Grant | undefined {
    const match = this.grants.all().find((grant) => grant.id === id || grant.id.startsWith(id));
    const grant = match ? this.grants.remove(match.id) : undefined;
    if (grant?.scope === "chat") this.appendEntry?.(REVOKE_ENTRY, { id: grant.id, chatId: this.chatId });
    return grant;
  }

  addRoot(path: string): void {
    this.appendEntry?.(ROOT_ENTRY, { path, action: "add", chatId: this.chatId });
  }

  removeRoot(path: string): void {
    this.appendEntry?.(ROOT_ENTRY, { path, action: "remove", chatId: this.chatId });
  }

  // --- The reviewing model -----------------------------------------------------

  private get healthPath(): string {
    return join(this.agentDir, "cypher-permissions", "judge-health.json");
  }

  /** Whether review is paused after failures, and why (shared by every Pi process: one runs per turn). */
  judgePaused(now = Date.now()): string | undefined {
    const health = this.readHealth();
    if (!health.pausedUntil || health.pausedUntil <= now) return undefined;
    const until = new Date(health.pausedUntil).toTimeString().slice(0, 5);
    return `Automatic review is paused until ${until}: ${health.reason ?? "the reviewing model kept failing"}.`;
  }

  private readHealth(): { pausedUntil?: number; reason?: string; timeouts?: number } {
    try {
      return JSON.parse(readFileSync(this.healthPath, "utf8"));
    } catch {
      return {};
    }
  }

  private writeHealth(health: { pausedUntil?: number; reason?: string; timeouts?: number }): void {
    try {
      mkdirSync(dirname(this.healthPath), { recursive: true });
      writeFileSync(this.healthPath, JSON.stringify(health), { mode: 0o600 });
    } catch {
      // Health is a convenience; review still fails safe without it.
    }
  }

  /**
   * Note how a review went. A sign-in failure pauses review for 30 minutes;
   * two timeouts in a row for 10. Paused, requests skip the wait and go
   * straight to the user, with one line saying why.
   */
  private noteReview(result: "ok" | "timeout" | "auth" | "error", why?: string): void {
    if (result === "ok") {
      if (this.readHealth().timeouts || this.readHealth().pausedUntil) this.writeHealth({});
      return;
    }
    if (result === "auth") {
      this.writeHealth({ pausedUntil: Date.now() + 30 * 60_000, reason: `the reviewing model could not sign in${why ? ` (${why})` : ""}; sign in again` });
      return;
    }
    if (result === "timeout") {
      const timeouts = (this.readHealth().timeouts ?? 0) + 1;
      this.writeHealth(timeouts >= 2 ? { pausedUntil: Date.now() + 10 * 60_000, reason: "the reviewing model timed out twice in a row", timeouts: 0 } : { timeouts });
    }
  }

  /**
   * Ask the reviewing model about one request. Its verdict decides: an
   * approval runs the call, and an ask stands only with a concern from the
   * closed list (judge.ts `CONCERN_TABLE`); an ask without one runs once.
   * A model that cannot decide (none here, paused, an error, a timeout or
   * an unreadable answer after the retry) is the floor's `no-judge` row:
   * the owner is asked, with the reason.
   */
  private async review(
    request: Omit<JudgeRequest, "level" | "workspaceRoots" | "userMessages" | "recentToolCalls">,
    ctx: RequestContext,
  ): Promise<Review> {
    if (!ctx.complete) return { decision: "ask-user", once: false, reason: "No reviewing model is available here.", state: "unavailable" };
    const paused = this.judgePaused();
    if (paused) return { decision: "ask-user", once: false, reason: paused, state: "paused" };
    const { userMessages, recentToolCalls } = conversationContext(ctx.branch());
    const judgeRequest: JudgeRequest = { ...request, level: this.level, workspaceRoots: this.roots, userMessages, recentToolCalls, ownerWords: this.ownerWords };
    // One attempt, then one retry when it timed out, gave no readable answer, or failed for a reason other than signing in.
    let text: string | undefined;
    let failure: string | undefined;
    let timedOut = false;
    let verdict: ReturnType<typeof parseVerdict> | undefined;
    for (let attempt = 0; attempt < JUDGE_ATTEMPTS; attempt++) {
      const controller = new AbortController();
      timedOut = false;
      failure = undefined;
      const timer = setTimeout(() => {
        timedOut = true;
        controller.abort();
      }, this.config.judge.timeoutMs);
      const onAbort = () => controller.abort();
      ctx.signal?.addEventListener("abort", onAbort, { once: true });
      try {
        text = await ctx.complete(JUDGE_SYSTEM_PROMPT, judgePrompt(judgeRequest), controller.signal);
      } catch (error) {
        text = undefined;
        failure = error instanceof Error ? error.message.trim() : undefined;
      } finally {
        clearTimeout(timer);
        ctx.signal?.removeEventListener("abort", onAbort);
      }
      verdict = text === undefined ? undefined : parseVerdict(text);
      if ((verdict && verdict.kind !== "unreadable") || ctx.signal?.aborted || (failure && AUTH_FAILURE.test(failure))) break;
    }
    if (text === undefined) {
      if (timedOut) {
        this.noteReview("timeout");
        return { decision: "ask-user", once: false, reason: `The reviewing model did not answer within ${Math.round(this.config.judge.timeoutMs / 1000)} seconds, twice.`, state: "unavailable" };
      }
      const short = failure && failure.length > 200 ? `${failure.slice(0, 200)}…` : failure;
      if (failure && AUTH_FAILURE.test(failure)) this.noteReview("auth", short);
      const why = short ? `: ${short}` : ".";
      return { decision: "ask-user", once: false, reason: `The reviewing model failed${why}`, state: "unavailable" };
    }
    this.noteReview("ok");
    verdict ??= parseVerdict(text);
    // No readable verdict, twice: no judge decided.
    if (verdict.kind === "unreadable") return { decision: "ask-user", once: false, reason: `${verdict.reason} (twice)`, state: "unavailable", audit: { judgeVerdict: "unreadable" } };
    // An ask with no concern from the closed list runs once: caution without a concrete concern is no reason to interrupt the owner.
    if (verdict.kind === "ask-user" && verdict.concern === undefined) {
      return {
        decision: "approve",
        once: true,
        reason: `${verdict.reason} (it named no concrete concern, so it runs once)`,
        state: "approved",
        audit: { judgeVerdict: "ask-user" },
        ...(verdict.quote || verdict.targets ? { claim: { quote: verdict.quote, ...(verdict.source ? { source: verdict.source } : {}), targets: verdict.targets } } : {}),
      };
    }
    return {
      decision: verdict.decision,
      once: verdict.once,
      reason: verdict.reason,
      state: verdict.decision === "approve" ? "approved" : "asked",
      audit: { judgeVerdict: verdict.kind },
      ...(verdict.quote || verdict.targets ? { claim: { quote: verdict.quote, ...(verdict.source ? { source: verdict.source } : {}), targets: verdict.targets } } : {}),
      ...(verdict.concern ? { concern: verdict.concern } : {}),
    };
  }

  /**
   * An ask on a concern the owner's own words answer (`consentAnswers`) runs once instead: the review's
   * new decision, or the review as it was.
   */
  private answered(review: Review, evidence: JudgeEvidence | undefined): Review {
    if (review.decision !== "ask-user" || review.state !== "asked") return review;
    const words = consentAnswers(review.concern, evidence);
    if (words === undefined) return review;
    return { ...review, decision: "approve", once: true, state: "approved", reason: `${review.reason} (but your own words ask for it, "${words.length > 120 ? `${words.slice(0, 120)}…` : words}", so it runs once)` };
  }

  private async elevate(
    request: {
      capability: Capability;
      classifications: Classification[];
      key: string;
      mode: "ask" | "assisted";
      tool: string;
      /** The command a domain request comes from, for the dialog. */
      command?: string;
      justification?: string;
      diff?: string;
      /** A tool-level request (MCP and other extension tools). */
      toolInput?: string;
      /** A domain a filtered command wants to reach. */
      domain?: string;
      /** The floor row `gate` found for a request the policy, a grant or a rule would have allowed (round 6, W1). */
      held?: FloorHit;
    },
    ctx: RequestContext,
  ): Promise<ElevateOutcome> {
    const started = Date.now();
    const paths = request.classifications.map((item) => item.path);
    const sensitive = request.classifications.some((item) => item.zone === "sensitive");
    const self = request.classifications.some((item) => item.guard === "self");
    const guarded = request.classifications.some((item) => item.guard === "protected");
    const file = request.domain === undefined && request.toolInput === undefined;
    // A protected write runs code later, and a write to Cypher's own data changes the installed app: dangers, for the judge, never remembered.
    const tier: Tier = file && request.capability === "write" && (guarded || self) && !sensitive ? "danger" : "local";
    // The floor (floor.ts): a write to the permission system's own files; the owner's own "Ask me" setting.
    // The owner's own Deny of a request like it in this chat (denials.ts): same tool, capability, target scope.
    const scopes: DenialScope[] =
      request.domain !== undefined
        ? [{ kind: "host", name: request.domain }]
        : request.toolInput !== undefined
          ? [{ kind: "line", name: `${request.tool} ${request.toolInput}` }]
          : paths.map((path) => ({ kind: "path" as const, name: path, says: [displayPath(path, this.home)] }));
    const action = request.domain !== undefined ? "network" : request.toolInput !== undefined ? `tool:${request.tool}` : request.capability;
    const actionClass: ActionClass = {
      tool: request.tool,
      actions: [action],
      ...(sensitive ? { concern: "secret" } : file && paths.some((path) => this.installedApp(path) && !installedProgram(path)) ? { concern: "installed-app" } : {}),
      scopes,
      // Its effect class (effects.ts): a file tool reads or changes, a domain is reached, a tool is its own.
      units: [{ effects: [request.domain !== undefined ? "send" : request.toolInput !== undefined ? (`tool:${request.tool}` as EffectClass) : request.capability === "write" ? "change" : "read"], scopes }],
    };
    const denied = await this.deniedLike(actionClass);
    const floor = floorOf(
      {
        permissionSystemPath: file && request.capability === "write" ? request.classifications.map((item) => this.paths.isPermissionSystem(item.path) && item.path).find((path): path is string => typeof path === "string") : undefined,
        ownerRule: request.mode === "ask" && !request.held ? "this kind of request is set to Ask me" : undefined,
        ...(denied.active ? { ownerDenied: `you denied ${denied.active.denial.what} and haven't asked for it since` } : {}),
        ...(request.held?.id === "other-task" ? { otherTask: request.held.detail } : {}),
      },
      "before",
    ) ?? request.held;
    const base: AuditRecord = {
      decision: "deny",
      decidedBy: "user",
      tool: request.tool,
      command: request.command,
      capability: request.capability,
      zone: request.classifications[0]?.zone,
      guard: request.classifications[0]?.guard,
      paths,
      tier,
    };
    const finish = (record: Partial<AuditRecord>) => this.audit.write({ ...base, latencyMs: Date.now() - started, ...record } as AuditRecord);
    // Model first: the judge decides; only the floor asks without it.
    let judgeNote: string | undefined;
    if (floor?.id === "permission-system") judgeNote = `Only you can approve this: ${displayPath(floor.detail, this.home)} is part of the permission system itself.`;
    if (floor === undefined) {
      const first = request.classifications[0];
      // The installed app's data (the owner's chats, journals, databases): evidence, and whether the owner's words name it.
      const data = file ? paths.filter((path) => this.installedApp(path) && !installedProgram(path)) : [];
      if (data.length) await this.refreshOwnerWords();
      const denials = denied.similar.length ? { denials: denied.similar.map((denial) => describeDenial(denial)) } : {};
      const evidence: JudgeEvidence | undefined = data.length
        ? { installedData: { paths: data.map((path) => displayPath(path, this.home)), finding: scanDataConsent(this.ownerWords, data) }, ...denials }
        : denied.similar.length
          ? denials
          : undefined;
      const reviewed = await this.review(
        {
          // A domain reads as "network" with the domain as its path; a tool call as "tool".
          capability: request.domain !== undefined ? "network" : request.toolInput !== undefined ? "tool" : request.capability,
          paths: request.domain !== undefined ? [request.domain] : request.toolInput !== undefined ? [request.tool] : paths,
          zone: first?.zone ?? "external",
          guard: first?.guard,
          guardReason: first?.guard ? `matches ${first.match}` : undefined,
          command: request.command,
          tool: request.tool,
          diff: request.diff,
          input: request.toolInput,
          justification: request.justification,
          tier,
          ...(tier === "danger" ? { tierReason: self ? "writes Cypher's own data" : "writes a file that runs code later" } : {}),
          ...(evidence ? { evidence } : {}),
        },
        ctx,
      );
      const verdict = this.answered(reviewed, evidence);
      base.judge = verdict.state;
      Object.assign(base, verdict.audit);
      if (verdict.decision === "approve") {
        // Remembered for the chat, so the same access is not reviewed again; a secret, a danger, the installed app's data, or an approval of this call alone, only once.
        const scope = sensitive || verdict.once || tier === "danger" || data.length > 0 ? "once" : "chat";
        finish({ decision: "allow", decidedBy: "judge", reason: verdict.reason, scope, prompted: false });
        return { allowed: true, scope, by: "judge" };
      }
      judgeNote = verdict.reason;
    }
    if (judgeNote) base.reviewer = judgeNote;
    if (!ctx.hasUI) {
      const reason = `Permission denied: ${describeAll(request.classifications)} needs approval and no one is here to approve it.`;
      finish({ decidedBy: "policy", reason, prompted: false });
      return { allowed: false, reason };
    }
    const projectOption = `${ALWAYS_IN}${this.projectName}`;
    let options: string[];
    if (request.domain !== undefined) options = [DOMAIN_ONCE, HUMAN.chat, HUMAN.always, HUMAN.deny, HUMAN.stop];
    else if (request.toolInput !== undefined) options = [HUMAN.once, HUMAN.chat, HUMAN.always, HUMAN.deny, HUMAN.stop];
    else if (sensitive || self) options = [HUMAN.once, HUMAN.chat, HUMAN.deny, HUMAN.stop];
    else if (tier === "danger") options = [TASK, HUMAN.deny, HUMAN.stop];
    else options = [projectOption, ONCE, HUMAN.deny, HUMAN.stop];
    const kind = request.domain !== undefined ? "network" : request.toolInput !== undefined ? "tool" : "access";
    const title = approvalTitle(request, judgeNote, this.home, tier === "danger") + promptLine(kind, ctx.toolCallId);
    const shown = Date.now();
    const choice = await ctx.ui.select(title, options, {
      signal: ctx.signal,
      timeout: this.config.ask.timeoutMs ?? undefined,
    });
    const nobody = unanswered(choice, shown, this.config.ask.timeoutMs, ctx.signal);
    const asked = { prompted: true, choice: nobody ?? choice };
    switch (choice) {
      case HUMAN.once:
      case ONCE:
      case DOMAIN_ONCE:
        finish({ decision: "allow", scope: "once", ...asked });
        return { allowed: true, scope: "once", by: "user" };
      case TASK:
        finish({ decision: "allow", scope: "task", ...asked });
        return { allowed: true, scope: "task", by: "user" };
      case HUMAN.chat:
        finish({ decision: "allow", scope: "chat", ...asked });
        return { allowed: true, scope: "chat", by: "user" };
      case HUMAN.always:
        finish({ decision: "allow", scope: "always", ...asked });
        return { allowed: true, scope: "always", by: "user" };
      case projectOption:
        finish({ decision: "allow", scope: "project", ...asked });
        return { allowed: true, scope: "project", by: "user" };
      case HUMAN.stop:
        this.denials.record(actionClass, describeRequest(request, paths, this.home), this.ownerWords);
        finish({ decision: "abort", ...asked });
        ctx.abort?.();
        return { allowed: false, reason: "The user denied this and stopped the turn.", abort: true };
      case HUMAN.deny: {
        this.denials.record(actionClass, describeRequest(request, paths, this.home), this.ownerWords);
        const reason = "The user denied this request.";
        finish({ reason, ...asked });
        return { allowed: false, reason: `${reason} Do not retry the same access; continue without it or ask the user.` };
      }
      default:
        // Closed, timed out, or an answer that is none of the buttons: no decision, and no denial.
        finish({ decision: "dismissed", reason: `No answer (${nobody ?? "not one of the options"}).`, ...asked });
        return { allowed: false, reason: UNANSWERED };
    }
  }

  /** A tool-level approval for an MCP or extension tool whose setting is `elevate`. */
  async requestToolUse(name: string, input: unknown, ctx: RequestContext, options: { held?: FloorHit } = {}): Promise<{ ok: true } | { ok: false; reason: string; abort?: boolean }> {
    this.refresh();
    const key = `tool:${name}`;
    const covered = () => {
      const grant = this.grants.findForFile("read", key);
      if (grant) this.grants.used(grant);
      return grant !== undefined || this.config.rules.some((rule) => rule.path === key && this.ruleApplies(rule));
    };
    // Covered by an approval, or allowed by its setting (`options.held` from the caller's `gateTool`): denials and the floor first (round 6, W1).
    const held = options.held ?? (covered() ? await this.gateTool("tool:approval", name, input) : undefined);
    if (!held && covered()) return { ok: true };
    const mode = approvalModeFor(this.config, "tools");
    if (mode === "deny") return { ok: false, reason: `Permission denied: the ${name} tool needs approval, which is set to always deny.` };
    let rendered = "";
    try {
      rendered = JSON.stringify(input, null, 2) ?? "";
    } catch {
      rendered = String(input);
    }
    const classification: Classification = { path: key, zone: "external", match: "tool" };
    const outcome = await this.serialized(async () =>
      !held && covered()
        ? ({ allowed: true, scope: "rule", by: "rule" } as const)
        : this.elevate({ capability: "read", classifications: [classification], key: "tools", mode: held ? "ask" : mode, tool: name, toolInput: rendered.slice(0, 2000), ...(held ? { held } : {}) }, ctx),
    );
    if (!outcome.allowed) return { ok: false, reason: outcome.reason, abort: outcome.abort };
    this.rememberPseudoPath(key, outcome.scope, outcome.by);
    return { ok: true };
  }

  /** Keep an approval of a tool or domain (`tool:`, `net:`) as long as its scope says. */
  private rememberPseudoPath(key: string, scope: Scope, by: Grant["approvedBy"]): void {
    if (scope === "always" || scope === "project") {
      const rule = { capability: "read" as const, path: key, createdAt: new Date().toISOString(), ...(scope === "project" ? { project: this.project } : {}) };
      appendRule(this.agentDir, rule);
      this.config.rules.push(rule);
      this.configStamp = this.readStamp();
    } else if (scope === "chat" || scope === "task") {
      const grant = newGrant({ capability: "read", paths: [key], scope, approvedBy: by, chatId: this.chatId, ...(scope === "task" ? { turn: this.grants.turn } : {}) });
      this.grants.add(grant);
      if (scope === "chat") this.appendEntry?.(GRANT_ENTRY, grant);
    }
  }

  // --- Running outside the sandbox ------------------------------------------------

  /** Whether a rule applies here: every project, or this one. */
  private ruleApplies(rule: { project?: string }): boolean {
    return rule.project === undefined || rule.project === this.project;
  }

  /** Every command rule in effect: the user's and project's, then this chat's. */
  commandRules(): (CommandRule & { dangers?: string[] })[] {
    return [...this.config.commands.filter((rule) => this.ruleApplies(rule)), ...this.grants.commandRules()];
  }

  /** Places a sandboxed command may write: a program there could be the agent's. */
  private writableRoots(): string[] {
    return [...this.roots, ...this.writableSupportDirs(), ...tempDirs(), ...this.gitWritable()];
  }

  /**
   * Whether a rule may run this program outside the sandbox: the program
   * exists, and one in a writable place still has the contents the rule
   * pinned.
   */
  private programTrusted(program: string, cwd: string | undefined, rule: CommandRule): boolean {
    const resolved = this.resolveIn(program, cwd);
    if (!resolved) return false;
    if (!this.writableRoots().some((root) => isUnder(resolved, root))) return true;
    return rule.sha256 !== undefined && fileSha256(resolved) === rule.sha256;
  }

  /** The system's program: found, and not in a place the agent can write. */
  private systemProgram(program: string, cwd: string | undefined): boolean {
    const resolved = this.resolveIn(program, cwd);
    return resolved !== undefined && !this.writableRoots().some((root) => isUnder(resolved, root));
  }

  /** A program as bash would find it from `cwd`; a relative path from a directory not known is not found. */
  private resolveIn(program: string, cwd: string | undefined): string | undefined {
    if (cwd === undefined && program.includes("/") && !isAbsolute(program)) return undefined;
    return resolveProgram(program, cwd ?? this.cwd);
  }

  /**
   * Whether a git command outside the sandbox reads only configuration the
   * agent cannot have written: the repository it works in has its own
   * `.git` directory (whose config and hooks are protected), or is one of
   * the chat's, or lives where sandboxed commands cannot write. A `.git`
   * file the agent wrote could point git at a config that runs a program.
   */
  private gitRepositoryTrusted(words: readonly string[], cwd: string | undefined): boolean {
    let dir = cwd;
    for (let i = 1; i < words.length; i++) {
      const word = words[i];
      if (word === "-C") {
        const next = words[i + 1];
        if (next === undefined || next === UNKNOWN || (dir === undefined && !isAbsolute(next))) return false;
        dir = resolvePath(dir ?? "/", next);
        i++;
      } else if (/^(-c|--config-env|--git-dir|--work-tree|--exec-path)(=|$)/.test(word)) return false;
      else if (!word.startsWith("-")) break;
    }
    if (dir === undefined) return false;
    const writable = this.writableRoots();
    for (let current = this.paths.canonical(dir, this.cwd); ; current = dirname(current)) {
      const marker = join(current, ".git");
      let stat;
      try {
        stat = statSync(marker);
      } catch {
        if (dirname(current) === current) return true;
        continue;
      }
      if (stat.isDirectory()) {
        // The chat's own repository, or one sandboxed commands cannot have
        // made: a `.git` directory created in the workspace could hold any config.
        let real: string;
        try {
          real = realpathSync.native(marker);
        } catch {
          return false;
        }
        return this.gitDirs.includes(real) || !writable.some((root) => isUnder(real, root));
      }
      let target: string;
      try {
        const pointer = /^gitdir:\s*(.+)$/m.exec(readFileSync(marker, "utf8"))?.[1]?.trim();
        if (!pointer) return false;
        target = realpathSync.native(resolvePath(current, pointer));
      } catch {
        return false;
      }
      if (this.gitDirs.includes(target)) return true;
      let common = target;
      try {
        const pointer = readFileSync(join(target, "commondir"), "utf8").trim();
        common = realpathSync.native(resolvePath(target, pointer));
      } catch {
        // No commondir: the target is the repository.
      }
      return ![target, common].some((path) => writable.some((root) => isUnder(path, root)) && !this.gitDirs.includes(path));
    }
  }

  /**
   * Whether a variable a line assigns stays in its shell, unseen by the
   * programs it runs: not one the environment exports (bash runs with the
   * engine's), not one bash itself reads, and not when a command prefix
   * from the settings could export more.
   */
  private assignable(name: string): boolean {
    return !this.shellPrefix && !Object.hasOwn(process.env, name) && !SHELL_VARIABLES.test(name) && !/proxy/i.test(name);
  }

  private parse(command: string, cwd?: string): ParsedLine {
    return parseShell(command, this.home, {
      assignable: (name) => this.assignable(name),
      prefixAllowed: benignEnv,
      // Read as an evaluator (shell.ts): loops over literal words, functions, scripts in heredocs, `PATH=… cmd`.
      evaluate: true,
      resolveProgram: (program, assignments) => this.resolveWithPath(program, assignments),
      // An exported value is followed for later words; `export` itself never runs outside by a rule (`ruleCannotVouch`).
      exportable: (name) => !SHELL_VARIABLES.test(name) && !/proxy/i.test(name),
      // `$PWD` is where the line starts, unless a command prefix from the settings could move it first.
      ...(cwd !== undefined && !this.shellPrefix ? { cwd } : {}),
    });
  }

  /**
   * A line's commands, each with the directory it runs in: undefined after a
   * `cd` that may or may not have run (after `&&` or `||`, in a pipeline,
   * `if`, loop or substitution), or that goes somewhere not known. Each is
   * seen through the wrappers that only run it (`nohup`, `timeout 30`),
   * which must be the system's programs.
   */
  private commandsOf(commands: readonly SimpleCommand[], cwd: string): LineCommand[] {
    const out: LineCommand[] = [];
    let here: string | undefined = cwd;
    const moves = (command: SimpleCommand) => DIRECTORY_BUILTINS.has(command.words[0] ?? "");
    // A `cd` in a loop moves every command of the loop, before it too.
    const movesInLoop = commands.some((command) => command.loop && moves(command));
    // A subshell starts where the line's shell is, and its `cd` stays in it.
    let outside: string | undefined = cwd;
    let group: number | undefined;
    for (const [index, command] of commands.entries()) {
      if (command.subshell !== group) {
        // Entering one from the line's shell, or leaving one (for the line, or the next subshell).
        if (group === undefined) outside = here;
        else here = outside;
        group = command.subshell;
      }
      if (command.loop && movesInLoop) here = undefined;
      const unwrapped = unwrap(command.words);
      const wrapped = unwrapped && unwrapped.wrappers.every((wrapper) => this.systemProgram(wrapper, here)) ? unwrapped : undefined;
      const words = wrapped?.words ?? command.words;
      out.push({
        ...command,
        words,
        // The wrappers' words are gone from the front: so are their hints and globs.
        ...(command.hints ? { hints: command.hints.slice(command.words.length - words.length) } : {}),
        ...(command.patterns ? { patterns: command.patterns.slice(command.words.length - words.length) } : {}),
        env: [...command.env, ...(wrapped?.env ?? [])],
        // The values in front of it and its wrappers' words: they can name the installed app too (`CYPHER_DATA_DIR=~/.cypher cmd`).
        carried: [...(command.envValues ?? []), ...(wrapped ? command.words.slice(0, command.words.length - words.length) : [])],
        wrappers: wrapped?.wrappers ?? [],
        cwd: here,
        // A system shell whose script the reader read (`bash -c '…'`, `bash <<'EOF'`) only runs the commands that follow it.
        inert: isInert(words) || this.harmlessFilter(words, here) || (command.script === true && this.systemProgram(words[0], here)),
        // A shell or interpreter reading code from its input, when the reader couldn't read that code.
        ...(command.script !== true && readsCode(words) && (command.reads.length > 0 || command.inputUnknown === true || commands[index - 1]?.joint === "|") ? { stdinCode: true } : {}),
      });
      if (!moves(command)) continue;
      const [program, target, ...rest] = command.words;
      const certain = program === "cd" && target !== undefined && target !== UNKNOWN && target !== "-" && rest.length === 0 && !command.scoped && !process.env.CDPATH;
      here = certain && here !== undefined ? resolvePath(here, target) : undefined;
    }
    return out;
  }

  /**
   * Whether a command is a filter that needs no leave ([`filterInputs`]):
   * the system's program, not one in a place the agent can write, reading
   * only its input and files commands in the sandbox may read.
   */
  private harmlessFilter(words: readonly string[], cwd: string | undefined): boolean {
    const inputs = filterInputs(words);
    if (inputs === undefined) return false;
    if (!this.systemProgram(words[0], cwd)) return false;
    return inputs.every((path) => {
      if (cwd === undefined && !isAbsolute(path)) return false;
      return this.verdict("read", this.classify(path, cwd)).kind === "allow";
    });
  }

  // --- What this task made ---------------------------------------------------

  /** Whether a path may ever be recorded as made: not sensitive, protected, the permission system's, the installed app's, a home or root. */
  private recordablePath(path: string): boolean {
    if (path === "/" || path === this.home || dirname(path) === path) return false;
    const classification = this.paths.classify(path);
    if (classification.zone === "sensitive" || classification.guard !== undefined) return false;
    if (this.paths.isPermissionSystem(path) !== undefined) return false;
    return !this.installedApp(path);
  }

  /** What `creationsOf` needs: canonical paths, and what may never be recorded. */
  private creationFacts(): CreationFacts {
    return {
      home: this.home,
      canonical: (path, cwd) => {
        if (cwd === undefined && !isAbsolute(path) && !path.startsWith("~")) return undefined;
        return this.paths.canonical(path, cwd ?? this.cwd);
      },
      // Already there: naming it (`mkdir -p ~/Developer/other`) does not make
      // it this task's, and a command the sandbox refused made nothing at all.
      recordable: (path) => this.recordablePath(path) && !existsSync(path),
    };
  }

  /** Repositories (shared git directories) by directory, for the catalogue. */
  private readonly repositories = new Map<string, string | undefined>();

  /** What the catalogue (catalogue.ts) and the lifecycle table (lifecycle.ts) need from this machine. */
  catalogueFacts(): CatalogueFacts & LifecycleFacts {
    const git = (args: string[]) => {
      const result = spawnSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
      return result.status === 0 ? result.stdout.trim() : undefined;
    };
    return {
      ...this.creationFacts(),
      // What is the chat's anyway (its workspace, scratch) needs no entry.
      recordable: (path) => this.recordablePath(path) && !this.paths.inWorkspace(path) && !this.paths.inScratch(path),
      exists: (path) => {
        try {
          lstatSync(path);
          return true;
        } catch {
          return false;
        }
      },
      repository: (dir) => {
        if (dir === undefined) return undefined;
        if (!this.repositories.has(dir)) {
          const common = git(["-C", dir, "rev-parse", "--path-format=absolute", "--git-common-dir"]);
          let canonical: string | undefined;
          try {
            canonical = common ? realpathSync.native(common) : undefined;
          } catch {
            canonical = undefined;
          }
          this.repositories.set(dir, canonical);
        }
        return this.repositories.get(dir);
      },
      refExists: (repo, ref) => git(["--git-dir", repo, "rev-parse", "-q", "--verify", ref]) !== undefined,
      trusted: (program, cwd) => this.systemProgram(program, cwd),
      prHead: (pr) => this.prHead(pr),
      worktreeBranch: (path) => git(["-C", path, "symbolic-ref", "--short", "-q", "HEAD"]),
    };
  }

  /** Pull requests' head branches, by `repo#number`, as gh last said. */
  private readonly prHeads = new Map<string, string | undefined>();

  /** A pull request's head branch: its entity's, else gh's (once per pull request). */
  private prHead(pr: LedgerEntity): string | undefined {
    if (pr.head) return pr.head;
    if (!/^\d+$/.test(pr.name)) return undefined;
    const key = `${pr.repo ?? ""}#${pr.name}`;
    if (!this.prHeads.has(key)) {
      const args = ["pr", "view", pr.name, "--json", "headRefName", "-q", ".headRefName", ...(pr.repo && pr.repo.includes("/") && !pr.repo.startsWith("/") ? ["-R", pr.repo] : [])];
      const result = spawnSync("gh", args, { cwd: this.cwd, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 8000 });
      this.prHeads.set(key, result.status === 0 && result.stdout.trim() ? result.stdout.trim() : undefined);
    }
    return this.prHeads.get(key);
  }

  /** The line's commands as the catalogue reads them; undefined when it can't be read. */
  private catalogueCommands(command: string, cwd: string): LineCommand[] | undefined {
    try {
      const parsed = this.parse(command, cwd);
      if (parsed.kind === "opaque") return undefined;
      return this.commandsOf(parsed.commands, cwd);
    } catch {
      return undefined;
    }
  }

  /**
   * Before a command this chat runs: what it may create, and what is there
   * already (catalogue.ts). Settle it after the run (`settleCreated`).
   * Undefined when nothing is enforced or the line can't be read.
   */
  planCreated(command: string, cwd: string): (CreationPlan & { taints?: LedgerEntity[] }) | undefined {
    if (!this.enforcing) return undefined;
    const commands = this.catalogueCommands(command, cwd);
    if (!commands) return undefined;
    const taints = this.taintsOf(commands);
    // What it writes by redirection or `tee` is this task's (round 5): code it runs later is read (`taskWrote`).
    for (const item of commands) {
      for (const path of [...item.writes, ...(item.words[0] === "tee" ? item.words.slice(1) : [])]) {
        if (path !== UNKNOWN && !path.startsWith("-") && (item.cwd !== undefined || isAbsolute(path))) this.noteWritten(this.classify(path, item.cwd).path);
      }
    }
    const plan = this.created.plan(command, commands, this.catalogueFacts());
    return plan.candidates.length || taints.length ? { ...plan, ...(taints.length ? { taints } : {}) } : undefined;
  }

  /** The chat's taint ledger (ledger.ts `taintFile`): what its commands wrote from a secret. */
  private readonly taint: Ledger;

  /** What a line writes from a secret (risk.ts `secretWrites`), as taint entries. */
  private taintsOf(commands: readonly LineCommand[]): LedgerEntity[] {
    try {
      const facts = this.riskFacts();
      const found = secretWrites(commands, facts);
      return [
        ...found.paths.map((item) => ({ kind: "path" as const, name: item.path, from: item.from })),
        ...found.repos.map((item) => ({ kind: "repo" as const, name: this.paths.canonical(item.dir, this.cwd), from: item.from })),
      ];
    } catch {
      return [];
    }
  }

  /** Record a line's taints once it ran (a failed line wrote nothing for sure, but may have: kept unless it surely failed). */
  private settleTaints(plan: { taints?: LedgerEntity[]; command: string } | undefined, exitCode: number | undefined): void {
    if (!plan?.taints?.length) return;
    if (exitCode !== undefined && exitCode !== 0 && plan.taints.every((entity) => !existsSync(entity.name))) return;
    this.taint.record(plan.taints, plan.command);
  }

  /** The secret a canonical path holds by the taint ledger (it, or a folder holding it, was written from one). */
  taintedPath(path: string): string | undefined {
    return this.taint.all().find((entry) => entry.kind === "path" && entryCovers(entry, { kind: "path", name: path }))?.from;
  }

  /** The secret a repository holds by the taint ledger: one staged where `dir` is, or above or below it. */
  taintedRepo(dir: string): string | undefined {
    const at = this.paths.canonical(dir, this.cwd);
    return this.taint.all().find((entry) => entry.kind === "repo" && (isUnder(at, entry.name) || isUnder(entry.name, at)))?.from;
  }

  /** The folders of the permission system's patterns that have no wildcard (`<agent>/permissions.json` → `<agent>`), canonical. */
  permissionPlaces(): string[] {
    const places = new Set<string>();
    for (const pattern of this.config.permissionSystem) {
      if (pattern.startsWith("!") || pattern.startsWith("**")) continue;
      const dir = pattern.replace(/\/\*\*$/, "").replace(/\/[^/]*$/, "");
      if (!dir || /[*?[{]/.test(dir)) continue;
      places.add(this.paths.canonical(dir, this.cwd));
    }
    return [...places];
  }

  /** The first sensitive file inside an existing folder, hidden ones too (as an archiver reads it): breadth first, bounded. */
  secretInside(dir: string): string | undefined {
    const queue: [string, number][] = [[dir, 0]];
    let visited = 0;
    while (queue.length) {
      const [at, depth] = queue.shift()!;
      let names: string[];
      try {
        if (!statSync(at).isDirectory()) return undefined;
        names = readdirSync(at);
      } catch {
        continue;
      }
      for (const name of names) {
        if (++visited > SECRET_SCAN_LIMIT) return undefined;
        const path = join(at, name);
        if (this.paths.classify(path).zone === "sensitive") return displayPath(path, this.home);
        if (name === ".git" || depth >= SECRET_SCAN_DEPTH) continue;
        try {
          if (lstatSync(path).isDirectory()) queue.push([path, depth + 1]);
        } catch {
          // Gone.
        }
      }
    }
    return undefined;
  }

  settleCreated(plan: (CreationPlan & { taints?: LedgerEntity[] }) | undefined, result: { exitCode?: number; output?: string }): void {
    if (!plan) return;
    this.settleTaints(plan, result.exitCode);
    try {
      const made = this.created.settle(plan, result, this.catalogueFacts());
      const legacy = made.filter((entry) => entry.action === undefined && (entry.kind === "path" || entry.kind === "worktree" || (entry.kind === "branch" && entry.remote === undefined)));
      // The session keeps its own record too, for a Runtime that reads only that.
      if (legacy.length) {
        this.appendEntry?.(CREATED_ENTRY, {
          ...(legacy.some((entry) => entry.kind === "path") ? { paths: legacy.filter((entry) => entry.kind === "path").map((entry) => entry.name) } : {}),
          ...(legacy.some((entry) => entry.kind === "worktree") ? { worktrees: legacy.filter((entry) => entry.kind === "worktree").map((entry) => entry.name) } : {}),
          ...(legacy.some((entry) => entry.kind === "branch") ? { branches: legacy.filter((entry) => entry.kind === "branch").map((entry) => entry.name) } : {}),
          ...(this.chatId ? { chatId: this.chatId } : {}),
          createdAt: new Date().toISOString(),
        });
      }
    } catch {
      // Recording is best effort: a command that ran stays run.
    }
  }

  /**
   * Record what a command creates without its result: the worktrees,
   * branches, directories, dev instances and scratch files it names that
   * are not there yet (created.ts `creationsOf`). For replays and tests;
   * a real run plans and settles around the command (`planCreated`,
   * `settleCreated`).
   */
  noteCreated(command: string, cwd: string): void {
    if (!this.enforcing) return;
    const commands = this.catalogueCommands(command, cwd);
    if (!commands) return;
    this.settleTaints({ taints: this.taintsOf(commands), command }, undefined);
    const record = this.created.note(creationsOf(commands, this.creationFacts()), command);
    if (record) this.appendEntry?.(CREATED_ENTRY, record);
  }

  /** `PATH=… cmd` with literal values: the program the assignments make it, when it is the system's. */
  private resolveWithPath(program: string, assignments: readonly { name: string; value: string | undefined }[]): string | undefined {
    if (program.includes("/")) return undefined;
    if (!assignments.every((item) => item.name === "PATH" && item.value !== undefined && item.value !== "")) return undefined;
    const path = assignments.at(-1)!.value!;
    for (const dir of path.split(":")) {
      if (!dir || !isAbsolute(dir)) return undefined;
      const candidate = join(dir, program);
      if (!existsSync(candidate)) continue;
      // Only a program in a place the agent can't write.
      return this.systemProgram(candidate, this.cwd) ? candidate : undefined;
    }
    return undefined;
  }

  /**
   * Why a command's redirections keep a rule from vouching for it: a file
   * that commands in the sandbox may not read or write without asking.
   */
  private redirectProblem(item: LineCommand): string | undefined {
    for (const [capability, paths] of [["read", item.reads], ["write", item.writes]] as const) {
      const verb = capability === "read" ? "reads" : "writes";
      for (const path of paths) {
        if (path === UNKNOWN) return `it ${verb} a file known only when it runs`;
        if (item.cwd === undefined && !isAbsolute(path)) return `it ${verb} ${path} in a directory known only when it runs`;
        const classification = this.classify(path, item.cwd);
        if (this.verdict(capability, classification).kind !== "allow") return `it ${verb} ${displayPath(classification.path, this.home)}, which commands in the sandbox can't`;
      }
    }
    return undefined;
  }

  /** A built-in or user inspection command (risk.ts R0) that may run outside: the system's program, and git only in a repository it can trust. */
  private inspection(item: LineCommand, assessment: Assessment): boolean {
    if (assessment.tier !== "inspect" || item.words.length === 0) return false;
    if (!this.systemProgram(item.words[0], item.cwd)) return false;
    if (basename(item.words[0]) === "git" && !this.gitRepositoryTrusted(item.words, item.cwd)) return false;
    return true;
  }

  /** An allow rule vouches for a command: it matches, applies here, may see its variables, and its program is the one approved. */
  private allowRule(rules: readonly (CommandRule & { dangers?: string[] })[], item: LineCommand, assessment: Assessment): CommandRule | undefined {
    return rules.find(
      (rule) =>
        rule.decision === "allow" &&
        matchesQuietly(rule.match, item.words) &&
        item.env.every((name) => rule.env?.includes(name)) &&
        (assessment.tier !== "danger" || rule.acknowledgeDanger === true || (assessment.key !== undefined && rule.dangers?.includes(assessment.key))) &&
        this.programTrusted(item.words[0], item.cwd, rule) &&
        (basename(item.words[0]) !== "git" || this.gitRepositoryTrusted(item.words, item.cwd)),
    );
  }

  /** The risk tier of a whole line: its most dangerous command (risk.ts). */
  assess(command: string, cwd: string, depth = 0): LineAssessment {
    const parsed = this.parse(command, cwd);
    if (parsed.kind === "opaque") return { ...assessOpaque(command, this.riskFacts(), false, cwd), commands: [], parsed };
    const commands = this.commandsOf(parsed.commands, cwd);
    const facts = this.riskFacts();
    // Code an interpreter runs (code.ts): read like the line around it, before its commands are judged.
    const code = this.codeFindings(commands, depth);
    // An inert builtin or a filter needs no rule, but its redirections can still be dangerous (`echo x >> ~/.zshrc`).
    const assessments = commands.map((item): Assessment => {
      if (!item.inert) {
        const own = assessCommand(item, facts);
        const found = code.get(item);
        return found ? withCode(own, found) : own;
      }
      return redirectRisk(item, facts) ?? { tier: "inspect" };
    });
    let worst: Assessment = { tier: "inspect" };
    for (const assessment of assessments) if (TIER_RANK[assessment.tier] > TIER_RANK[worst.tier]) worst = assessment;
    if (downloadsAndRuns(commands)) worst = { tier: "danger", kind: "downloads code and runs it", key: "download-run" };
    // A secret leaving the machine: the always-ask floor, for the line as a whole — the code it runs included.
    const exfiltration = exfiltrates(commands, facts, parsed.expands, code);
    const nested = [...code.values()].find((found) => found.floor)?.floor;
    if (exfiltration) worst = { tier: "danger", kind: exfiltration, key: "secret-off-machine", floor: true };
    else if (nested) worst = { tier: "danger", kind: nested.kind, key: nested.key, floor: true };
    else {
      // A write whose target is known only by what a listing could print (round 6, W5, risk.ts `runtimeWriteSets`).
      const listed = runtimeWriteSets(commands, facts);
      if (listed.length) worst = { tier: "danger", kind: listed[0], key: "permission-system", floor: true };
    }
    const opaqueCode = [...code.values()].flatMap((found) => (found.opaque ? [`${found.origin} (${found.language}): ${found.opaque}`] : []));
    const unresolved = commands.flatMap((item) => unresolvedWrites(item, facts));
    return { ...worst, commands, assessments, parsed, ...(opaqueCode.length ? { opaqueCode } : {}), ...(unresolved.length ? { unresolved } : {}) };
  }

  /**
   * The code each command of a line runs (code.ts `codeOf`): an interpreter's inline code, its input (a
   * heredoc, a here-string, what `echo`/`printf` or a script's `cat` pipes into it), or a script this task
   * wrote (in this line, by its file tools, or by an earlier command: `taskWrote`). Read by language
   * (`scanCode`): what reaches the network is a sink, a secret it reads a source; the shell lines it runs
   * are read as lines of their own (`assess`). Code that can't be read is `opaque`.
   */
  private codeFindings(commands: readonly LineCommand[], depth: number): Map<LineCommand, CodeFinding> {
    const out = new Map<LineCommand, CodeFinding>();
    if (depth > MAX_CODE_DEPTH) return out;
    /** What earlier commands of the line wrote, with its text (undefined: known only when it runs). */
    const written = new Map<string, string | undefined>();
    for (const [index, item] of commands.entries()) {
      const found = this.codeFinding(item, commands[index - 1], written, depth);
      if (found) out.set(item, found);
      const text = item.input ?? (/^(echo|printf)$/.test(item.words[0] ?? "") && !item.words.includes(UNKNOWN) ? (literalOutput(item.words) ?? `${item.words.slice(1).join(" ")}\n`) : undefined);
      for (const path of item.writes) if (path !== UNKNOWN && (item.cwd !== undefined || isAbsolute(path))) written.set(this.classify(path, item.cwd).path, text);
      if (item.words[0] === "tee") for (const path of item.words.slice(1)) if (path !== UNKNOWN && !path.startsWith("-") && (item.cwd !== undefined || isAbsolute(path))) written.set(this.classify(path, item.cwd).path, item.input);
    }
    return out;
  }

  private codeFinding(item: LineCommand, before: LineCommand | undefined, written: ReadonlyMap<string, string | undefined>, depth: number): CodeFinding | undefined {
    let interpreter: Interpreter | undefined;
    let text: string | undefined;
    let origin = "its inline code";
    let opaque: string | undefined;
    /** A script: what this line wrote there, else its file when this task wrote it; undefined when it isn't this task's. */
    const script = (path: string, cwd: string | undefined): { text?: string; why?: string; shown: string } | undefined => {
      if (path === UNKNOWN) return undefined;
      if (cwd === undefined && !isAbsolute(path) && !path.startsWith("~")) return undefined;
      const canonical = this.classify(path, cwd).path;
      const shown = displayPath(canonical, this.home);
      if (written.has(canonical)) {
        const content = written.get(canonical);
        return content === undefined ? { why: "this line writes it with text known only when it runs", shown } : { text: content, shown };
      }
      if (!this.taskWrote(canonical)) return undefined;
      return { ...readScript(canonical), shown };
    };
    // Through what runs it (`sudo`, `xargs`, `find -exec`): the interpreter is the command it runs.
    const runs = runWords(item.words);
    // The command lines its options or configuration run (`man -P …`, `git -c core.pager=…`, code.ts `optionCommands`).
    const optionLines = optionCommands(runs, UNKNOWN);
    if (optionLines.length) return this.scanFinding(optionLines.join("\n"), "shell", item.cwd, depth, "the command its options run");
    const read = codeOf(runs, UNKNOWN, runs.length === item.words.length ? item.patterns : undefined);
    if (read) {
      interpreter = read.interpreter;
      const source = read.source;
      // A shell's `-c` and the scripts it reads from its input are read by the shell evaluator itself.
      if (interpreter.language === "shell" && (source.from === "inline" || source.from === "inline-unknown" || item.script === true)) return undefined;
      if (source.from === "inline") text = source.text;
      else if (source.from === "inline-unknown" && interpreter.filter) return undefined;
      else if (source.from === "inline-unknown") opaque = "it is known only when it runs";
      else if (source.from === "script") {
        const found = script(source.path, item.cwd);
        if (!found) return undefined;
        origin = `the script ${found.shown}`;
        text = found.text;
        opaque = found.why;
      } else if (source.from === "stdin") {
        origin = "the code on its input";
        if (item.input !== undefined) text = item.input;
        else if (item.inputUnknown) opaque = "it is known only when it runs";
        else if (item.reads.length) {
          const found = script(item.reads[0], item.cwd);
          if (!found) return undefined;
          origin = `the script ${found.shown} on its input`;
          text = found.text;
          opaque = found.why;
        } else if (before?.joint === "|") {
          text = this.pipedCode(before, written);
          // What it reads is known only when it runs: the stdin-code danger (risk.ts) stands.
          if (text === undefined) return undefined;
        } else return undefined;
      } else return undefined;
    } else {
      // A script run as a program (`./x`): its `#!` line, or its extension, says what reads it.
      const program = item.words[0];
      if (program === undefined || program === UNKNOWN || !program.includes("/")) return undefined;
      const found = script(program, item.cwd);
      if (!found) return undefined;
      interpreter = (found.text !== undefined ? shebangInterpreter(found.text) : undefined) ?? interpreterOf(extensionInterpreter(program) ?? "sh");
      if (!interpreter) return undefined;
      origin = `the script ${found.shown}`;
      text = found.text;
      opaque = found.why;
    }
    if (text !== undefined && item.stdinCode) item.stdinCode = false;
    const language = interpreter.language;
    if (opaque !== undefined || text === undefined) return { language, origin, opaque: opaque ?? "it can't be read" };
    return this.scanFinding(withAssignments(text, item), language, item.cwd, depth, origin);
  }

  /** What a command pipes into an interpreter as its program: `echo`/`printf` of literal words (shell.ts `literalOutput`), or `cat` of a script this task wrote. */
  private pipedCode(before: LineCommand, written: ReadonlyMap<string, string | undefined>): string | undefined {
    const [program, ...args] = before.words;
    if (program === undefined || before.words.includes(UNKNOWN)) return undefined;
    const printed = literalOutput(before.words);
    if (printed !== undefined) return printed;
    if (program === "cat" && args.length === 1 && !args[0].startsWith("-") && (before.cwd !== undefined || isAbsolute(args[0]))) {
      const path = this.classify(args[0], before.cwd).path;
      if (written.has(path)) return written.get(path);
      return this.taskWrote(path) ? readScript(path).text : undefined;
    }
    if (before.input !== undefined && (program === "cat" || program === "tee") && args.every((word) => word.startsWith("-"))) return before.input;
    return undefined;
  }

  /** Code's findings (code.ts `scanCode`), or a shell script's (read as a line). */
  private scanFinding(text: string, language: string, cwd: string | undefined, depth: number, origin: string): CodeFinding {
    const facts = this.riskFacts();
    const at = cwd ?? this.cwd;
    const finding: CodeFinding = { language, origin };
    const nested = (line: string) => {
      let assessed: LineAssessment;
      try {
        assessed = this.assess(line, at, depth + 1);
      } catch {
        return;
      }
      if (assessed.floor) finding.floor ??= assessed;
      else if (assessed.tier === "danger" && !finding.worst) finding.worst = { tier: "danger", kind: assessed.kind, key: assessed.key };
      if (assessed.parsed.kind !== "simple") return;
      const flow = lineFlow(assessed.commands, facts, assessed.parsed.expands);
      finding.sink ??= flow.sink;
      finding.secret ??= flow.secret;
      if (assessed.opaqueCode?.length) finding.opaque ??= assessed.opaqueCode[0];
    };
    if (language === "shell") {
      nested(text);
      return finding;
    }
    const scan = scanCode(text, language as Parameters<typeof scanCode>[1], secretName);
    if (scan.opaque.length) finding.opaque = scan.opaque.join("; ");
    if (scan.network.length) finding.sink = `${language} code (${scan.network.slice(0, 3).map((name) => name.replace(/\($/, "")).join(", ")})`;
    else if (scan.urls.length && scan.reads) finding.sink = `${language} code (it opens ${scan.urls[0].slice(0, 80)})`;
    if (scan.reads || scan.spawn.length) {
      for (const path of scan.paths) {
        if (!/[/.~]/.test(path) || /^[a-z][\w+.-]*:\/\//i.test(path)) continue;
        const classification = this.classify(resolveText(path, this.home), at);
        if (classification.zone === "sensitive") {
          finding.secret ??= path;
          break;
        }
      }
    }
    if (scan.env.length) finding.secret ??= scan.env[0] === "the environment" ? "the environment" : `$${scan.env[0]}`;
    // A permission-system file the code names, in code that writes files (code.ts `write` names): the floor (round 5 step 2).
    if (scan.writes) {
      for (const path of scan.paths) {
        if (!/[/.~]/.test(path)) continue;
        const classification = this.classify(resolveText(path, this.home), at);
        const system = this.paths.isPermissionSystem(classification.path);
        if (system !== undefined) {
          finding.floor ??= { tier: "danger", kind: `changes the permission system itself (${path}, in ${language} code)`, key: "permission-system", floor: true };
          break;
        }
      }
    }
    for (const line of scan.commands.slice(0, MAX_CODE_COMMANDS)) nested(line);
    return finding;
  }

  /** Canonical paths this chat's file tools wrote, and its bash lines' redirections (this process); `taskWrote` reads the transcript too. */
  private readonly written = new Set<string>();

  /** The chat's own transcript, for what its tools wrote before this process (the extension sets it). */
  transcript?: () => readonly Entry[];

  /** Record a path this chat's write or edit tool wrote. */
  noteWritten(path: string): void {
    this.written.add(path);
  }

  /**
   * Whether this task wrote a file (round 5, provenance for code it runs): its file tools or bash lines in
   * this process, the ledger (what its commands made), or its transcript's write/edit calls and the bash
   * lines that write it (a restart reads the same transcript).
   */
  taskWrote(path: string): boolean {
    if (this.written.has(path) || this.created.has(path)) return true;
    const entries = this.transcript?.() ?? [];
    // The transcript only grows: a path read against as many entries as before reads the same.
    const cached = this.wroteCache.get(path);
    if (cached && cached.entries === entries.length) return cached.wrote;
    const wrote = this.transcriptWrote(path, entries);
    this.wroteCache.set(path, { entries: entries.length, wrote });
    return wrote;
  }
  private readonly wroteCache = new Map<string, { entries: number; wrote: boolean }>();

  private transcriptWrote(path: string, entries: readonly Entry[]): boolean {
    const name = basename(path);
    for (const entry of entries) {
      const message = (entry as { type?: string; message?: { role?: string; content?: unknown } }).message;
      if ((entry as { type?: string }).type !== "message" || message?.role !== "assistant" || !Array.isArray(message.content)) continue;
      for (const part of message.content as { type?: string; name?: string; arguments?: Record<string, unknown> }[]) {
        if (part?.type !== "toolCall") continue;
        const args = part.arguments ?? {};
        if ((part.name === "write" || part.name === "edit") && typeof args.path === "string" && this.classify(args.path, this.cwd).path === path) return true;
        if (part.name === "bash" && typeof args.command === "string" && args.command.includes(name)) {
          const commands = this.catalogueCommands(args.command, this.cwd) ?? [];
          for (const item of commands) {
            const targets = [...item.writes, ...(item.words[0] === "tee" ? item.words.slice(1) : []), ...(/^(cp|mv|install|ln)$/.test(item.words[0] ?? "") ? item.words.slice(-1) : [])];
            if (targets.some((target) => target !== UNKNOWN && !target.startsWith("-") && (item.cwd !== undefined || isAbsolute(target)) && this.classify(target, item.cwd).path === path)) return true;
          }
        }
      }
    }
    return false;
  }

  /**
   * What the rules say about a bash call, before it runs. Every command the
   * line can run is checked, in `$(…)` and loops too, inert builtins and
   * filters skipped: a `deny` that matches anywhere refuses the line; it
   * runs outside only if every command is an inspection (risk.ts R0) or
   * matches an `allow` whose program is trusted and that may vouch for it
   * (never a dangerous command, unless the user acknowledged it or a task
   * grant covers that danger), none could turn out to match a `deny` or
   * `ask`, and its redirections reach only files the sandbox allows.
   */
  checkCommand(command: string, cwd: string, options: { escalated?: boolean } = {}): CommandCheck {
    if (!this.enforcing) return { kind: "sandbox" };
    this.refresh();
    const line = this.assess(command, cwd);
    // A line the parser cannot read cannot match a rule: it runs in the
    // sandbox unless it is approved as written.
    if (line.parsed.kind === "opaque") return { kind: "sandbox", tier: line.tier, why: line.kind };
    const rules = this.commandRules();
    const commands = line.commands;
    const used: CommandRule[] = [];
    let outside = commands.some((item) => !item.inert);
    let inspected = false;
    /** An inspection here needs to leave the sandbox, or the agent asked to. */
    let needed = options.escalated === true;
    // No rule vouches for a line that downloads and runs code, or for one on the floor (round 5: a floor the line's code reaches too).
    const lineDanger = line.key === "download-run" || line.floor === true;
    for (const [index, item] of commands.entries()) {
      if (this.redirectProblem(item)) outside = false;
      if (item.inert) continue;
      const deny = rules.find((rule) => rule.decision === "deny" && matchesQuietly(rule.match, item.words));
      if (deny) return { kind: "deny", reason: `Permission denied: \`${describePattern(deny.match)}\` is set to never run.` };
      if (ruleCannotVouch(item.words) || rules.some((rule) => rule.decision !== "allow" && matchesQuietly(rule.match, item.words, true))) {
        outside = false;
        continue;
      }
      const assessment = line.assessments![index];
      // A rule the user kept runs it outside; an inspection may run there without one.
      const allow = lineDanger ? undefined : this.allowRule(rules, item, assessment);
      if (allow) {
        used.push(allow);
        needed = true;
      } else if (!lineDanger && this.inspection(item, assessment)) {
        inspected = true;
        needed ||= assessment.outside === true;
      } else outside = false;
    }
    // Inspections the sandbox runs as well stay in it: nothing to leave for.
    if (outside && needed) return { kind: "outside", rules: used, inspected, tier: line.tier, why: line.kind };
    return { kind: "sandbox", tier: line.tier, why: line.kind };
  }

  /**
   * The agent asks to run a bash call outside the sandbox. The floor
   * (floor.ts) goes to a human; everything else to the reviewing model
   * first (round 3), with the code's evidence:
   *
   * - dangerous: its approval runs the line once, never remembered; when it
   *   asks with a listed concern, a human decides: run once, or allow its
   *   patterns until the user's next message;
   * - local: its approval is remembered for the chat; when it asks, a human
   *   decides: once, or always in this project by patterns for similar
   *   commands (the agent's, then each other command's category).
   *
   * `onceOnly` says why no pattern could be offered.
   */
  async requestEscalation(
    command: string,
    proposal: EscalationProposal,
    ctx: RequestContext,
  ): Promise<EscalationOutcome> {
    const started = Date.now();
    this.refresh();
    const mode = approvalModeFor(this.config, "escalate");
    const line = this.assess(command, ctx.cwd);
    const audit: AuditRecord = { tool: "bash", command, capability: "escalate", decision: "deny", decidedBy: "policy", tier: line.tier, kind: line.kind };
    const finish = (record: Partial<AuditRecord>) => this.audit.write({ ...audit, latencyMs: Date.now() - started, ...record } as AuditRecord);
    if (mode === "deny") {
      const reason = "Permission denied: running commands outside the sandbox is set to always deny.";
      finish({ reason });
      return { ok: false, reason };
    }
    const check = this.checkCommand(command, ctx.cwd, { escalated: true });
    if (check.kind === "deny") {
      finish({ decidedBy: "rule", reason: check.reason, prompted: false });
      return { ok: false, reason: check.reason };
    }
    // A rule or an inspection allows it without review, unless a denial or the floor holds it (round 6, W1).
    const heldOutside = check.kind === "outside" ? await this.gateLine("bash:outside", command, ctx.cwd) : undefined;
    if (check.kind === "outside" && !heldOutside) {
      finish({ decision: "allow", decidedBy: check.rules.length ? "rule" : "inspect", prompted: false });
      return { ok: true, scope: "rule", by: "rule" };
    }
    const dangerous = line.tier === "danger";
    // Patterns, each with the pin for a program in a writable place.
    const patterns: Offered[] = [];
    const pinned = (program: string, cwd: string | undefined): { sha256?: string } | undefined => {
      const resolved = this.resolveIn(program, cwd);
      if (!resolved) return undefined;
      return { sha256: this.writableRoots().some((root) => isUnder(resolved, root)) ? fileSha256(resolved) : undefined };
    };
    /** Why only this run can be approved. */
    let onceOnly: string | undefined;
    if (line.parsed.kind === "opaque") {
      onceOnly = `it uses ${line.parsed.reason}, which rules can't check`;
    } else {
      const commands = line.commands;
      const facts = this.riskFacts();
      // Inspections and filters need no rule; nor does a command already allowed.
      const pending = commands
        .map((item, index) => ({ item, assessment: line.assessments![index] }))
        .filter(({ item, assessment }) => !item.inert && !this.inspection(item, assessment));
      for (const pattern of proposal.patterns ?? []) {
        const error = patternError(pattern);
        if (error) return { ok: false, reason: `Invalid pattern ${JSON.stringify(pattern)}: ${error}` };
        // A filter needs no pattern, but the agent may still propose one for it.
        const matched = commands.find((item) => patternMatches(pattern, item.words));
        if (!matched) return { ok: false, reason: `The pattern \`${describePattern(pattern)}\` does not match this command; propose patterns that cover it.` };
        const pin = pinned(matched.words[0], matched.cwd);
        if (!pin) return { ok: false, reason: `\`${matched.words[0]}\` was not found.` };
        patterns.push({ match: pattern, ...pin, env: [...matched.env], dangers: [], proposed: true });
      }
      const rules = this.commandRules();
      onceOnly = commands.map((item) => this.redirectProblem(item)).find(Boolean);
      for (const { item, assessment } of pending) {
        if (onceOnly) break;
        onceOnly = ruleCannotVouch(item.words);
        const held = rules.find((rule) => rule.decision !== "allow" && matchesQuietly(rule.match, item.words, true));
        if (held) onceOnly ??= held.decision === "deny" ? `it could turn out to be \`${describePattern(held.match)}\`, which is set to never run` : `\`${describePattern(held.match)}\` is set to always ask`;
        if (onceOnly) break;
        if (line.key === "download-run") {
          onceOnly = "it downloads code and runs it";
          break;
        }
        const danger = assessment.tier === "danger" ? assessment.key : undefined;
        // Covered: by a proposal, or already allowed.
        const proposed = patterns.find((pattern) => patternMatches(pattern.match, item.words));
        if (proposed) {
          for (const name of item.env) if (!proposed.env.includes(name)) proposed.env.push(name);
          if (danger && !proposed.dangers.includes(danger)) proposed.dangers.push(danger);
          continue;
        }
        if (this.allowRule(rules, item, assessment)) continue;
        const category = categoryPattern(item.words);
        if (typeof category === "string" || patternError(category)) {
          onceOnly = typeof category === "string" ? category : patternError(category)!.replace(/\.$/, "");
          break;
        }
        const pin = pinned(item.words[0], item.cwd);
        if (!pin) {
          onceOnly = `\`${basename(item.words[0])}\` was not found`;
          break;
        }
        const same = patterns.find((pattern) => JSON.stringify(pattern.match) === JSON.stringify(category));
        if (same) {
          for (const name of item.env) if (!same.env.includes(name)) same.env.push(name);
          if (danger && !same.dangers.includes(danger)) same.dangers.push(danger);
        } else patterns.push({ match: category, ...pin, env: [...item.env], dangers: danger ? [danger] : [] });
      }
      void facts;
    }
    if (onceOnly) patterns.length = 0;
    const label = proposal.label?.trim() || undefined;
    const offered = patterns.map((pattern) => describePattern(pattern.match));
    if (onceOnly) audit.onceOnly = onceOnly;
    if (offered.length) audit.offered = offered;

    return this.serialized(async () => {
      // An earlier dialog in the queue may have allowed it meanwhile.
      this.refresh();
      if (!heldOutside && this.checkCommand(command, ctx.cwd, { escalated: true }).kind === "outside" && !(await this.gateLine("bash:outside", command, ctx.cwd))) {
        finish({ decision: "allow", decidedBy: "rule", prompted: false });
        return { ok: true, scope: "rule", by: "rule" } as const;
      }
      // The reviewing model decides (round 3). Only the floor (floor.ts) asks without it: a secret leaving
      // the machine, a change to the permission system, the owner's own rule set to ask; and, after it, no
      // judge that could decide. Everything else reaches it, dangers and lines whose target can't be read
      // included, with what the code checked as evidence (`evidenceFor`): the owner's words against the
      // line's targets, the grade, the installed app's data, the ledger. Its approval runs the line (a
      // danger once, never remembered); an ask stands only with a concern from the closed list that the
      // owner's own words don't answer (`answered`).
      // What the line acts on, read once: for the judge, the owner's denials and other tasks' work.
      const read = dangerous && line.parsed.kind !== "opaque" ? this.lineTargets(line) : undefined;
      // The owner's own Deny of a request like it (denials.ts), and another chat's or task's work it touches.
      // Read when there is a denial to check it against, or one to record (its targets may ask git and the catalogue).
      let classOfLine: ActionClass | undefined;
      const actionClass = () => (classOfLine ??= this.lineClass(line, command, ctx.cwd, read));
      const denied = this.denials.any() ? await this.deniedLike(actionClass()) : { similar: [] };
      // Another task's work: a danger's read targets, and what any line destroys of another chat's, globs too (round 6, W3).
      const foreign = (read ? await this.foreignTargets("targets" in read ? read.targets : (read.known ?? [])) : undefined) ?? (this.othersMade().length ? await this.otherTaskOf(actionClass()) : undefined);
      this.gated?.push({ entry: "bash:escalate", ...(denied.active ? { held: "owner-denied" } : foreign ? { held: "other-task" } : {}) });
      const floor = this.alwaysAsk(line, {
        ...(denied.active ? { ownerDenied: `you denied \`${denied.active.denial.what}\` (${denied.active.denial.at.slice(0, 16).replace("T", " ")} UTC) and haven't asked for it since` } : {}),
        ...(foreign ? { otherTask: foreign } : {}),
      });
      const targets: LineTargets | undefined =
        dangerous && floor === undefined
          ? line.parsed.kind === "opaque"
            ? { unknown: `it uses ${line.parsed.reason}, which Cypher can't read`, known: [] }
            : read
          : undefined;
      // What the line says, as written and with the evaluator's known expansions: an unreadable target the model claims must occur in it.
      if (targets && "unknown" in targets) targets.text = [command, ...line.commands.map((item) => item.words.filter((word) => word !== UNKNOWN).join(" "))].join("\n");
      const unknownTarget = targets && "unknown" in targets ? targets.unknown : undefined;
      // The installed app's data the line reads (risk.ts `concern`): whether the owner's words name it is evidence too.
      const data = [line, ...(line.assessments ?? [])].flatMap((assessment) => (assessment.concern === "installed-app" && assessment.data ? [assessment.data] : []));
      const installedRead = !dangerous && data.length > 0;
      if (floor !== undefined) audit.explicit = { ok: false, reason: `only you can approve this: ${floor.detail}`, targets: [], passed: [] };
      let review: string | undefined;
      if (mode === "assisted" && floor === undefined) {
        // The owner's words as they have written them by now: the evidence is checked against them.
        if (dangerous || installedRead) await this.refreshOwnerWords();
        const purpose = [proposal.label && `Purpose: ${proposal.label}`, proposal.reason && `Reason: ${proposal.reason}`].filter(Boolean).join("\n");
        // An approval is remembered for the commands' own categories, never for patterns the agent proposed; a danger's, never.
        const memo = dangerous ? [] : patterns.filter((pattern) => !pattern.proposed);
        const evidence = this.evidenceFor(targets, data, ctx, denied.similar, [line.kind, ...(line.assessments ?? []).map((assessment) => assessment.kind)], line);
        const reviewed = await this.review(
          {
            capability: "escalate",
            paths: [],
            zone: "outside the sandbox",
            command,
            tool: "bash",
            justification: purpose || undefined,
            tier: line.tier,
            tierReason: line.kind,
            remembers: memo.map((pattern) => describePattern(pattern.match)),
            ...(targets && "targets" in targets ? { targets: targets.targets.map((target) => describeTarget(target)) } : {}),
            ...(targets && "unknown" in targets ? { targets: (targets.known ?? []).map((target) => describeTarget(target)), unknownTarget: targets.unknown } : {}),
            created: this.createdSummary(line),
            evidence,
          },
          ctx,
        );
        audit.judge = reviewed.state;
        Object.assign(audit, reviewed.audit);
        // No reviewing model could decide: the floor's last row.
        const noJudge = floorOf({ review: reviewed }, "after");
        const verdict = noJudge ? reviewed : this.answered(reviewed, evidence);
        audit.judge = verdict.state;
        // The code's check of what the model cited, for the audit: it no longer gates (round 3).
        if (!noJudge && dangerous && targets && (verdict.decision === "approve" || verdict.claim)) audit.explicit = checkExplicit(verdict.claim ?? {}, targets, this.ownerWords, ctx.branch(), this.explicitFacts());
        else if (!noJudge && installedRead && verdict.decision === "approve") {
          const cited = this.installedConsent(verdict.claim, data);
          audit.explicit = cited.ok ? { ok: true, quote: cited.quote, targets: [], passed: ["quote", "current"] } : { ok: false, reason: cited.reason, targets: [], passed: [] };
        }
        if (verdict.decision === "approve") {
          // `approve-once`: this call runs, and nothing is remembered; so does an approval of a danger, of a
          // line that reads a secret (risk.ts `once`) or the installed app's data, or of an ask the owner's words answered.
          const kept = verdict.once || dangerous || installedRead || line.once === true || line.assessments?.some((assessment) => assessment.once === true) ? [] : memo;
          for (const pattern of kept) this.addCommandGrant(pattern, "chat", "judge", label);
          const quoted = audit.explicit?.ok && audit.explicit.quote ? ` (your words: "${audit.explicit.quote}")` : "";
          finish({ decision: "allow", decidedBy: "judge", reason: `${verdict.reason}${quoted}`, scope: kept.length ? "chat" : "once", prompted: false });
          return { ok: true, scope: "once", by: "judge", ...(onceOnly ? { onceOnly } : {}) } as const;
        }
        review = verdict.reason;
        audit.reviewer = review;
      }
      if (!ctx.hasUI) {
        const reason = "Permission denied: running outside the sandbox needs approval and no one is here to approve it.";
        finish({ reason, prompted: false });
        return { ok: false, reason } as const;
      }
      const lines: string[] = [];
      if (dangerous && line.kind) {
        const asks = floor !== undefined ? " This always asks." : unknownTarget ? ` Cypher can't read all it acts on: ${unknownTarget}.` : "";
        lines.push(fact("Note", `Dangerous: it ${homeRelative(line.kind, this.home)}.${asks}`));
      }
      if (!dangerous && floor !== undefined) lines.push(fact("Note", `This always asks: ${homeRelative(floor.detail, this.home)}.`));
      if (proposal.label) lines.push(fact("Purpose", proposal.label));
      lines.push(fact("Command", command));
      for (const pattern of patterns) lines.push(fact("Pattern", describePattern(pattern.match)));
      if (proposal.reason) lines.push(fact("Reason", proposal.reason));
      if (review) lines.push(fact("Review", review.startsWith("Automatic review is paused") ? review : `The reviewing model couldn't approve this automatically. ${review}`));
      lines.push("Note: It runs as you, with your files, keys, keychain and network.");
      if (patterns.some((pattern) => pattern.sha256)) lines.push("Note: A program here is in a writable place; approval covers its current contents only.");
      if (onceOnly) lines.push(fact("Note", `${onceOnly[0].toUpperCase()}${onceOnly.slice(1)}, so it can only run once as written.`));
      if (!onceOnly && dangerous) lines.push("Note: Allow for this task covers commands like it, with the same danger, until your next message.");
      const question = dangerous ? "Run this dangerous command outside the sandbox?" : "Run this command outside the sandbox?";
      const title = `${question}\n\nContext:\n${lines.join("\n")}${promptLine("escalate", ctx.toolCallId)}`;
      const projectOption = `${ALWAYS_IN}${this.projectName}`;
      const options = patterns.length === 0 ? [RUN_ONCE, HUMAN.deny, HUMAN.stop] : dangerous ? [TASK, HUMAN.deny, HUMAN.stop] : [projectOption, ONCE, HUMAN.deny, HUMAN.stop];
      const shown = Date.now();
      const choice = await ctx.ui.select(title, options, { signal: ctx.signal, timeout: this.config.ask.timeoutMs ?? undefined });
      const nobody = unanswered(choice, shown, this.config.ask.timeoutMs, ctx.signal);
      const asked = { prompted: true, choice: nobody ?? choice, decidedBy: "user" as const };
      switch (choice) {
        case RUN_ONCE:
        case ONCE:
          finish({ decision: "allow", scope: "once", ...asked });
          return { ok: true, scope: "once", by: "user", ...(onceOnly ? { onceOnly } : {}) } as const;
        case TASK:
          for (const pattern of patterns) this.addCommandGrant(pattern, "task", "user", label);
          finish({ decision: "allow", scope: "task", ...asked });
          return { ok: true, scope: "task", by: "user" } as const;
        case projectOption:
          for (const pattern of patterns) {
            const rule: CommandRule = {
              match: pattern.match,
              decision: "allow",
              ...(patterns.length === 1 && label ? { label } : {}),
              ...(pattern.sha256 ? { sha256: pattern.sha256 } : {}),
              ...(pattern.env.length ? { env: [...pattern.env].sort() } : {}),
              project: this.project,
              createdAt: new Date().toISOString(),
            };
            appendCommandRule(this.agentDir, rule);
            this.config.commands.push(rule);
          }
          this.configStamp = this.readStamp();
          finish({ decision: "allow", scope: "project", ...asked });
          return { ok: true, scope: "project", by: "user" } as const;
        case HUMAN.stop:
          this.denials.record(forRecord(actionClass()), command, this.ownerWords);
          finish({ decision: "abort", ...asked });
          ctx.abort?.();
          return { ok: false, reason: "The user denied this and stopped the turn.", abort: true } as const;
        case HUMAN.deny: {
          this.denials.record(forRecord(actionClass()), command, this.ownerWords);
          const reason = "The user denied running this outside the sandbox.";
          finish({ ...asked, reason });
          return { ok: false, reason: `${reason} Continue in the sandbox or ask the user.` } as const;
        }
        default:
          // Closed, timed out, or an answer that is none of the buttons: no decision, and no denial.
          finish({ ...asked, decision: "dismissed", reason: `No answer (${nobody ?? "not one of the options"}).` });
          return { ok: false, reason: UNANSWERED } as const;
      }
    });
  }

  /**
   * Whether the model cited owner words that ask for reading the installed app's data (explicit.ts
   * `checkDataConsent`): a consent of a kind, its quote in that owner word, naming the data or the
   * app, not taken back or moved on from. The quote is the model's; that it holds is the code's.
   */
  installedConsent(claim: ExplicitClaim | undefined, data: readonly string[]): { ok: true; quote: string } | { ok: false; reason: string } {
    return checkDataConsent(claim, this.ownerWords, data);
  }

  /** Ask the engine for the owner's words as of now; a failure keeps the ones at hand. */
  private async refreshOwnerWords(): Promise<void> {
    if (!this.ownerWordsNow) return;
    try {
      const words = await this.ownerWordsNow();
      if (words) this.ownerWords = words;
    } catch {
      // The words the run started with stand.
    }
  }

  /**
   * The floor (floor.ts `FLOOR`, the rows tested before the judge): why no
   * rule, grant or reviewing model may approve this line, whatever the
   * owner wrote — a secret or credential leaving this machine, a change to
   * the permission system itself (risk.ts sets `floor` on both), or the
   * owner's own command rule set to ask or deny. The last row, no reviewing
   * model, is tested on the review's outcome (`requestEscalation`).
   */
  private alwaysAsk(line: LineAssessment, extra: { ownerDenied?: string; otherTask?: string } = {}): FloorHit | undefined {
    const rules = this.commandRules();
    let ownerRule: string | undefined;
    for (const item of line.commands) {
      if (item.inert) continue;
      const held = rules.find((rule) => rule.decision !== "allow" && matchesQuietly(rule.match, item.words, true));
      if (held) {
        ownerRule = `\`${describePattern(held.match)}\` is set to ${held.decision === "deny" ? "never run" : "always ask"}`;
        break;
      }
    }
    return floorOf({ assessments: [line, ...(line.assessments ?? [])], ownerRule, ...extra }, "before");
  }

  /**
   * What the code established about a line, for the judge (round 3): each owner word checked as a
   * consent for the line's targets (the same checks a cited consent gets: explicit.ts `scanConsents`),
   * the grade of what it does (grades.ts), what its flags add, and whether the owner's words name the
   * installed app's data it reads. Evidence, never a gate.
   */
  private evidenceFor(targets: LineTargets | undefined, data: readonly string[], ctx: RequestContext, denied: readonly DenialRecord[] = [], kinds: readonly (string | undefined)[] = [], line?: LineAssessment): JudgeEvidence | undefined {
    const evidence: { -readonly [K in keyof JudgeEvidence]: JudgeEvidence[K] } = {};
    // Code it runs that can't be read, and paths known only when it runs that could be the permission system (round 5).
    if (line?.opaqueCode?.length) evidence.opaqueCode = line.opaqueCode;
    if (line?.unresolved?.length) evidence.unresolved = line.unresolved;
    if (targets) {
      const known = "targets" in targets ? targets.targets : (targets.known ?? []);
      evidence.consents = scanConsents(targets, this.ownerWords, ctx.branch(), this.memoFacts());
      const grades = gradesOf(known, targets.attributes ?? []).map(describeGrade);
      if (grades.length) evidence.grades = grades;
      if (targets.attributes?.length) evidence.attributes = targets.attributes.map((attribute) => describeAttribute(attribute));
    }
    // What the classifier says a command does, where its targets don't tell the reach (an install's directory).
    const byKind = kindGrades(kinds).map(describeGrade).filter((grade) => !evidence.grades?.includes(grade));
    if (byKind.length) evidence.grades = [...(evidence.grades ?? []), ...byKind];
    if (data.length) evidence.installedData = { paths: [...new Set(data.map((path) => displayPath(path, this.home)))], finding: scanDataConsent(this.ownerWords, data) };
    if (denied.length) evidence.denials = denied.map((denial) => describeDenial(denial));
    if (targets) {
      const notOurs = this.notThisTask("targets" in targets ? targets.targets : (targets.known ?? []));
      if (notOurs.length) evidence.notThisTask = notOurs.map((item) => `${item.what}: ${item.why}${item.named ? ` (the owner's words name it: "${item.named.length > 100 ? `${item.named.slice(0, 100)}…` : item.named}")` : ""}`);
    }
    return Object.keys(evidence).length ? evidence : undefined;
  }

  /**
   * A bash line's action class (denials.ts, round 5): one unit per pipeline (effects.ts
   * `commandEffects`, read through wrappers and `find -exec`): its effect classes, and the typed things it
   * acts on — the tool table's entities, git's refs and refspecs (a branch, never a path), the
   * catalogue's targets, the paths its words, redirections and data findings name (canonical, through the
   * resolver: a glob by its fixed directory), the targets read from the line. The line itself when it
   * names nothing.
   */
  private lineClass(line: LineAssessment, command: string, cwd: string, read: LineTargets | undefined): ActionClass {
    const actions = new Set<string>();
    const all: DenialScope[] = [];
    const units: { effects: EffectClass[]; scopes: DenialScope[]; destroys?: boolean; globs?: string[] }[] = [];
    const seen = new Set<string>();
    const scopeOf = (scope: DenialScope): DenialScope => {
      const key = `${scope.kind}\u0000${scope.name}\u0000${scope.remote ?? ""}\u0000${scope.scope ?? ""}\u0000${scope.type ?? ""}`;
      if (!seen.has(key)) {
        seen.add(key);
        all.push(scope);
      }
      return scope;
    };
    const pathScope = (word: string, at: string | undefined, pattern?: string): DenialScope | undefined => {
      if (word === UNKNOWN) {
        // A glob or a word known only when it runs: what its fixed directory holds (resolve.ts), when that is known.
        if (pattern === undefined) return undefined;
        const found = this.couldBe(pattern, at);
        if (found.anywhere || !found.fixed.length) return undefined;
        const path = found.fixed[0];
        return scopeOf({ kind: "path", name: path, says: [displayPath(path, this.home)], glob: true });
      }
      if (/^[a-z][\w+.-]*:\/\//i.test(word) || word.startsWith("-")) return undefined;
      if (at === undefined && !isAbsolute(word) && !word.startsWith("~")) return undefined;
      const path = this.classify(word, at ?? cwd).path;
      return scopeOf({ kind: "path", name: path, says: [displayPath(path, this.home), word] });
    };
    let unit: { effects: EffectClass[]; scopes: DenialScope[]; destroys?: boolean; globs?: string[] } | undefined;
    const effect = (value: EffectClass) => {
      if (unit && !unit.effects.includes(value)) unit.effects.push(value);
    };
    const facts = this.riskFacts();
    for (const [index, item] of line.commands.entries()) {
      // A pipeline is one unit: what `| xargs rm` deletes is what the command before it named.
      if (!unit || line.commands[index - 1]?.joint !== "|") {
        unit = { effects: [], scopes: [] };
        units.push(unit);
      }
      const add = (scope: DenialScope | undefined) => {
        if (scope && unit && !unit.scopes.includes(scope)) unit.scopes.push(scope);
      };
      // What destroys (round 6, W3: the other-task floor): a delete, erase or stop by the program tables, a move away.
      const inner = unwrapped(item.words);
      const removal = remoteEffect(inner);
      const row = readTool(inner);
      const operation = targetsOf(item, this.catalogueFacts())?.op;
      if ((removal && /^(delete|erase|destroy|stop)$/.test(removal.action)) || (row && "row" in row && /^(delete|erase|destroy|stop)$/.test(row.row.action)) || basename(inner[0] ?? "") === "mv" || inner.includes("-delete") || operation === "destroy" || operation === "stop") unit.destroys = true;
      // The globs it is given, canonical (resolve.ts): what they could match is what it acts on.
      for (const pattern of item.patterns ?? []) {
        if (pattern === undefined || !/[*?[]/.test(pattern)) continue;
        const resolved = resolvePattern(pattern, { home: this.home, cwd: this.paths.canonical(item.cwd ?? cwd, this.cwd) });
        if (!resolved.anywhere) (unit.globs ??= []).push(...resolved.globs);
      }
      for (const [at, path] of item.writes.entries()) {
        effect("change");
        add(pathScope(path, item.cwd, item.writePatterns?.[at]));
      }
      for (const [at, path] of item.reads.entries()) {
        effect("read");
        add(pathScope(path, item.cwd, item.readPatterns?.[at]));
      }
      // What only runs what follows it (a shell whose script was read) or moves the shell (`cd`) is no part of it;
      // an inert filter still reads what it names (`cat f`, `head f`).
      if (!item.words.length || item.script === true || DIRECTORY_BUILTINS.has(item.words[0] ?? "")) continue;
      const program = basename(item.words[0] ?? "");
      const tool = readTool(item.words);
      actions.add(tool && "row" in tool ? [program, ...tool.row.sub].join(" ") : program);
      const found = commandEffects(item.words, {
        sink: (words) => sinkOf(viewOf(words)),
        current: program === "git" ? this.gitAt(item.cwd ?? cwd)?.current : undefined,
      });
      for (const value of found.effects) effect(value);
      for (const target of found.targets) {
        if (target.kind === "path" || target.kind === "worktree") add(pathScope(target.name, item.cwd));
        else add(scopeOf({ kind: target.kind, name: target.name, ...(target.remote ? { remote: target.remote } : {}), ...(target.repo ? { repo: target.repo } : {}), ...(target.scope ? { scope: target.scope } : {}), ...(target.type ? { type: target.type } : {}), says: [target.name] }));
      }
      const offset = item.words.length - (item.patterns?.length ?? item.words.length);
      for (const word of found.paths) {
        const at = item.words.indexOf(word);
        add(pathScope(word, item.cwd, word === UNKNOWN ? undefined : item.patterns?.[at - offset]));
      }
      // Globs and words known only when it runs, by their fixed directory.
      for (const [at, pattern] of (item.patterns ?? []).entries()) if (pattern !== undefined && item.words[at + offset] === UNKNOWN && at + offset > 0) add(pathScope(UNKNOWN, item.cwd, pattern));
      // What the catalogue says it destroys or stops (a simulator, a container, a worktree).
      const destroyed = targetsOf(item, this.catalogueFacts());
      for (const group of destroyed?.targets ?? []) {
        const entity = group[0];
        if (!entity) continue;
        if (entity.kind === "path" || entity.kind === "worktree") add(pathScope(entity.name, undefined));
        else add(scopeOf({ kind: entity.kind, name: entity.name, says: [entity.name], ...(entity.remote ? { remote: entity.remote } : {}) }));
        effect(destroyed!.op === "stop" ? "stop" : "change");
      }
    }
    const assessments = [line, ...(line.assessments ?? [])];
    for (const assessment of assessments) if (assessment.data && unit) unit.scopes.push(...[pathScope(assessment.data, cwd)].filter((scope): scope is DenialScope => scope !== undefined));
    // The targets the line was read for (a danger's), typed.
    for (const target of read ? ("targets" in read ? read.targets : (read.known ?? [])) : []) {
      const scope = target.kind === "path" || target.kind === "worktree" ? pathScope(target.value, target.dir) : scopeOf({ kind: target.kind, name: target.value, says: [target.value], ...(target.remote ? { remote: target.remote } : {}) });
      if (scope && units.length && !units.some((item) => item.scopes.includes(scope))) units.at(-1)!.scopes.push(scope);
    }
    const concern = assessments.find((assessment) => assessment.concern)?.concern ?? (line.tier === "danger" ? line.key : undefined);
    const named = units.filter((item) => item.scopes.length || item.globs?.length);
    const effects = [...new Set(units.flatMap((item) => item.effects))];
    const lineScope = (): DenialScope => scopeOf({ kind: "line", name: command });
    return {
      tool: "bash",
      actions: [...actions],
      ...(concern ? { concern } : {}),
      tokens: line.commands.flatMap((item) => item.words.slice(1).filter((word) => word !== UNKNOWN && !word.startsWith("-"))),
      scopes: all.length ? all : [lineScope()],
      units: named.length
        ? named.map((item) => ({ effects: item.effects.length ? item.effects : ["run"], scopes: item.scopes, ...(item.destroys ? { destroys: true } : {}), ...(item.globs?.length ? { globs: item.globs } : {}) }))
        : [{ effects: effects.length ? effects : ["run"], scopes: [lineScope()], ...(units.some((item) => item.destroys) ? { destroys: true } : {}), ...(units.some((item) => item.globs?.length) ? { globs: units.flatMap((item) => item.globs ?? []) } : {}) }],
    };
  }

  /**
   * The one check every entry point makes before it allows (round 6, W1; `ENTRY_POINTS`): the owner's denials
   * (denials.ts) and the floor's `before` rows (floor.ts) — the permission system, another task's work this
   * destroys (`otherTaskOf`), and whatever the caller adds (`subject`). Undefined: nothing holds it, and it
   * passes as before. Cheap when nothing can match: no denial in this chat, nothing destroyed, no other chat.
   */
  async gate(entry: EntryPoint, cls: ActionClass, subject: FloorSubject = {}): Promise<FloorHit | undefined> {
    const denied = this.denials.any() ? await this.deniedLike(cls) : undefined;
    const other = subject.otherTask ?? (await this.otherTaskOf(cls));
    const hit = floorOf(
      {
        ...subject,
        ...(denied?.active ? { ownerDenied: `you denied \`${denied.active.denial.what}\` (${denied.active.denial.at.slice(0, 16).replace("T", " ")} UTC) and haven't asked for it since` } : {}),
        ...(other ? { otherTask: other } : {}),
      },
      "before",
    );
    this.gated?.push({ entry, ...(hit ? { held: hit.id } : {}) });
    return hit;
  }

  /** A file tool's (or a tool's) request as an action class: its effect and its paths (denials.ts), as `elevate` reads one. */
  fileClass(tool: string, capability: Capability, paths: readonly string[]): ActionClass {
    const scopes: DenialScope[] = paths.map((path) => ({ kind: "path" as const, name: path, says: [displayPath(path, this.home)] }));
    return { tool, actions: [capability], scopes, units: [{ effects: [capability === "write" ? "change" : "read"], scopes }], tokens: paths };
  }

  /** A tool's (MCP, extension) request as an action class: its own effect, its input as the line, and the input's words for the safety net. */
  private toolClass(name: string, input: unknown): ActionClass {
    let rendered = "";
    try {
      rendered = JSON.stringify(input) ?? "";
    } catch {
      rendered = String(input);
    }
    const scopes: DenialScope[] = [{ kind: "line", name: `${name} ${rendered.slice(0, 2000)}` }];
    const words = [...rendered.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((match) => match[1]).filter((word) => word.length >= 2 && word.length <= 400);
    return { tool: name, actions: [`tool:${name}`], scopes, units: [{ effects: [`tool:${name}` as EffectClass], scopes }], tokens: words };
  }

  /** A bash line as an action class (`lineClass`), for `gate`. */
  lineClassOf(command: string, cwd: string): ActionClass {
    const line = this.assess(command, cwd);
    return this.lineClass(line, command, cwd, undefined);
  }

  /**
   * `gate` for a bash line that would run without review (round 6, W1): one a saved rule allows outside the
   * sandbox or that only inspects (`bash:outside`: the whole floor, the line's own findings too), or one that
   * runs in the sandbox (`bash:sandbox`: denials and other tasks' work; the sandbox itself keeps secrets and
   * the permission system out of its reach). Reads the line only when a denial or another chat's ledger exists.
   */
  async gateLine(entry: "bash:outside" | "bash:sandbox" | "bash:escalate", command: string, cwd: string): Promise<FloorHit | undefined> {
    const line = entry === "bash:outside" ? this.assess(command, cwd) : undefined;
    const own = line ? this.alwaysAsk(line) : undefined;
    if (!own && !this.denials.any() && !this.othersMade().length) {
      this.gated?.push({ entry });
      return undefined;
    }
    const cls = this.lineClass(line ?? this.assess(command, cwd), command, cwd, undefined);
    const hit = await this.gate(entry, cls);
    return own ?? hit;
  }

  /** `gate` for a tool call (MCP, extension) its setting or an earlier approval allows. */
  async gateTool(entry: "tool:setting" | "tool:approval", name: string, input: unknown): Promise<FloorHit | undefined> {
    if (!this.denials.any()) {
      this.gated?.push({ entry });
      return undefined;
    }
    return this.gate(entry, this.toolClass(name, input));
  }

  /**
   * The floor's `other-task` detail for what a request destroys (round 6, W3): an entity of any kind that
   * another chat's ledger says it made or started — the same entity, one inside it, or one a glob of the
   * request could match — and that this chat's own ledger doesn't hold, its task (`taskId`) isn't the other
   * chat's, and the owner's words don't name. Only destroying effects (`DenialUnit.destroys`: delete, erase,
   * move away, stop); a change of a file's contents is no destruction.
   */
  private async otherTaskOf(cls: ActionClass): Promise<string | undefined> {
    const units = cls.units.filter((unit) => unit.destroys);
    if (!units.length) return undefined;
    const others = this.othersMade()
      .map((entry) => {
        const kind = ledgerKind(entry.kind) ?? entry.kind;
        return { ...entry, kind, name: kind === "path" || kind === "worktree" ? this.paths.canonical(entry.name, this.cwd) : entry.name };
      })
      .filter((entry) => !(this.taskId && entry.task === this.taskId));
    if (!others.length) return undefined;
    const found = (): { what: string; why: string }[] => {
      const out: { what: string; why: string }[] = [];
      const seen = new Set<string>();
      const note = (entry: LedgerEntry, how: string) => {
        const what = `${entry.kind} ${entry.name}`;
        if (seen.has(what) || this.ownerNames([entry.name, basename(entry.name)]) !== undefined) return;
        seen.add(what);
        out.push({ what, why: `another chat (${entry.chatId ?? "unknown"}) ${entry.action === "start" ? "started" : "made"} it${how}` });
      };
      for (const unit of units) {
        for (const scope of unit.scopes) {
          // A glob's fixed folder is where it looks, not what it destroys: its globs say that (below).
          if (scope.kind === "line" || scope.glob) continue;
          const kind = ledgerKind(scope.kind);
          if (!kind) continue;
          const target: LedgerEntity = { kind, name: scope.name };
          if (this.created.ledger.covers(target)) continue;
          for (const entry of others) {
            const made = { ...entry, repo: undefined, remote: undefined };
            if (entryCovers(made, target)) note(entry, "");
            else if ((kind === "path" || kind === "worktree") && (made.kind === "path" || made.kind === "worktree") && isUnder(made.name, target.name)) note(entry, `, inside ${scope.name}`);
          }
        }
        for (const glob of unit.globs ?? []) {
          for (const entry of others) {
            if (entry.kind !== "path" && entry.kind !== "worktree") continue;
            if (this.created.ledger.covers({ kind: entry.kind, name: entry.name })) continue;
            if (globsIntersect(glob, entry.name) || globsIntersect(`${glob.replace(/\/+$/, "")}/**`, entry.name)) note(entry, `, which ${glob} matches`);
          }
        }
      }
      return out;
    };
    let hits = found();
    if (hits.length && this.ownerWordsNow) {
      await this.refreshOwnerWords();
      hits = found();
    }
    return hits.length ? hits.map((item) => `${item.what} (${item.why})`).join("; ") : undefined;
  }

  /** The owner's denials of `cls` (denials.ts), checked against their words as of now. */
  private async deniedLike(cls: ActionClass): Promise<{ active?: { denial: DenialRecord; scope: DenialScope }; similar: DenialRecord[] }> {
    if (!this.denials.any()) return { similar: [] };
    await this.refreshOwnerWords();
    return this.denials.check(cls, this.ownerWords);
  }

  /** The newest owner word (typed, plan, card or answer) that names `names`, if any. */
  private ownerNames(names: readonly string[]): string | undefined {
    for (const word of [...(this.ownerWords?.messages ?? [])].reverse()) {
      if (word.source === "reply") continue;
      const text = consentText(word);
      if (names.some((name) => name.length >= 2 && mentions(text, name))) return word.text;
    }
    return undefined;
  }

  /** Other chats' ledgers (`<agentDir>/cypher-permissions/ledger/*.jsonl`, not this chat's): what they made. Read once per minute at most. */
  private othersMade(): readonly LedgerEntry[] {
    const now = Date.now();
    if (this.othersCache && now - this.othersCache.at < 60_000) return this.othersCache.entries;
    const entries: LedgerEntry[] = [];
    const own = ledgerFile(this.agentDir, this.chatId);
    const dir = join(this.agentDir, "cypher-permissions", "ledger");
    try {
      const files = readdirSync(dir).filter((name) => name.endsWith(".jsonl") && join(dir, name) !== own);
      for (const name of files.slice(0, 200)) {
        const text = readFileSync(join(dir, name), "utf8");
        for (const line of text.split("\n").slice(-2000)) {
          if (!line.trim()) continue;
          try {
            const entry = JSON.parse(line) as LedgerEntry;
            const kind = entry && typeof entry.kind === "string" ? ledgerKind(entry.kind) : undefined;
            // One task's chats (a workspace and its threads, a plan's sessions) are no other task (round 6, W3).
            if (kind && typeof entry.name === "string" && (entry.action ?? "create") !== "merge" && !(this.taskId && entry.task === this.taskId)) entries.push({ ...entry, kind, chatId: entry.chatId ?? name.replace(/\.jsonl$/, "") });
          } catch {
            // A torn line: skipped.
          }
        }
      }
    } catch {
      // No other chat has a ledger.
    }
    this.othersCache = { at: now, entries };
    return entries;
  }
  private othersCache: { at: number; entries: LedgerEntry[] } | undefined;

  /**
   * What a line acts on that is not this task's (round 4, design §7.3.11): an entity another chat's
   * ledger says it made or started, or a branch that heads an open pull request this chat didn't open
   * (another task's work in review), and that this chat's own ledger and workspace don't hold. Each
   * with the owner's words that name it, if any. Generic over entity kinds: the ledger's matching
   * (ledger.ts `entryCovers`) decides what "the same" is.
   */
  private notThisTask(targets: readonly Target[]): { what: string; why: string; named?: string }[] {
    if (!targets.length) return [];
    const own = this.ownEntities();
    const out: { what: string; why: string; named?: string }[] = [];
    const others = this.othersMade();
    for (const target of targets) {
      const entity: LedgerEntity =
        target.kind === "branch"
          ? { kind: "branch", name: target.value, ...(target.remote ? { remote: target.remote } : {}) }
          : { kind: target.kind as LedgerEntity["kind"], name: target.kind === "path" || target.kind === "worktree" ? (this.classify(target.value, target.dir ?? this.cwd).path ?? target.value) : target.value };
      if (this.created.ledger.covers(entity) || (target.kind === "branch" && own.branches.includes(target.value))) continue;
      const bare = { ...entity, remote: undefined, repo: undefined };
      let why: string | undefined;
      const made = others.find((entry) => entryCovers({ ...entry, repo: undefined, remote: undefined }, bare));
      if (made) why = `another chat (${made.chatId ?? "unknown"}) ${made.action === "start" ? "started" : "made"} it`;
      if (!why && target.kind === "branch" && target.value !== UNKNOWN) {
        const open = this.pullRequestsWithHead(target.dir ?? this.cwd, target.value).find((pr) => pr.state === "open" && !own.prs.some((mine) => mine.number === pr.number));
        if (open) why = `it heads pull request #${open.number}, which is open and this chat didn't open`;
      }
      if (!why) continue;
      const named = this.ownerNames([target.value, basename(target.value)]);
      out.push({ what: `${target.kind} ${target.value}`, why, ...(named ? { named } : {}) });
    }
    return out;
  }

  /** The floor's `other-task` detail: what the line acts on that is another task's and the owner's words don't name. */
  private async foreignTargets(targets: readonly Target[]): Promise<string | undefined> {
    const unnamed = this.notThisTask(targets).filter((item) => !item.named);
    if (!unnamed.length) return undefined;
    if (this.ownerWordsNow) {
      await this.refreshOwnerWords();
      const still = this.notThisTask(targets).filter((item) => !item.named);
      if (!still.length) return undefined;
      return still.map((item) => `${item.what} (${item.why})`).join("; ");
    }
    return unnamed.map((item) => `${item.what} (${item.why})`).join("; ");
  }

  /** The explicit check's facts, each machine fact asked once (a scan checks every owner word against them). */
  private memoFacts(): ExplicitFacts {
    const facts = this.explicitFacts();
    const memo = <A extends unknown[], R>(fn: (...args: A) => R) => {
      const seen = new Map<string, R>();
      return (...args: A): R => {
        const key = JSON.stringify(args);
        if (!seen.has(key)) seen.set(key, fn(...args));
        return seen.get(key)!;
      };
    };
    return {
      originRepo: memo(facts.originRepo),
      release: memo(facts.release),
      ...(facts.pullRequestsWithHead ? { pullRequestsWithHead: memo(facts.pullRequestsWithHead) } : {}),
      ...(facts.worktreeBranch ? { worktreeBranch: memo(facts.worktreeBranch) } : {}),
      ...(facts.own ? { own: memo(facts.own) } : {}),
      ...(facts.currentBranch ? { currentBranch: memo(facts.currentBranch) } : {}),
    };
  }

  /** What a dangerous line's commands act on (explicit.ts), or why that is known only when it runs. */
  lineTargets(line: LineAssessment): LineTargets {
    const facts = this.riskFacts();
    const commands = line.commands.map((item, index) => {
      const danger = line.assessments![index].tier === "danger";
      // A dangerous redirection's file is a target of its own; the command's words, only when they are dangerous too.
      const fromWrites = danger && redirectRisk(item, facts)?.tier === "danger";
      const wordsDanger = danger && (!fromWrites || (!item.inert && assessCommand({ ...item, writes: [] }, facts).tier === "danger"));
      const writes = fromWrites ? item.writes.map((path) => (path === UNKNOWN ? UNKNOWN : (this.classify(path, item.cwd ?? this.cwd).path ?? path))) : [];
      return { words: this.shortRefspecs(item.words, item.cwd), cwd: item.cwd, danger: wordsDanger, ...(writes.length ? { writes } : {}) };
    });
    const read = lineTargets(commands, this.targetFacts());
    if (!("unknown" in read)) return read;
    // A destroyer whose owner Cypher can't tell still names what it destroys: the catalogue's rows, or the
    // generic destroy verb's operands, read as entities for the owner's words to name (explicit.ts `uncovered`).
    const own: Target[] = [];
    const named = commands.map((command) => {
      if (!command.danger || basename(command.words[0] ?? "") === "git") return command;
      const alone = lineTargets([command], this.targetFacts());
      if (!("unknown" in alone)) return command;
      const found = this.destroyedEntities(command.words, command.cwd);
      if (!found) return command;
      own.push(...found);
      return { ...command, danger: false };
    });
    if (own.length === 0) return { ...read, actions: this.unreadActions(line) };
    // The attributes are the line's (its dangerous commands' flags), whichever commands are read as entities.
    const again = { ...lineTargets(named, this.targetFacts()), ...(read.attributes ? { attributes: read.attributes } : {}) };
    return "unknown" in again ? { ...again, known: [...(again.known ?? []), ...own], actions: this.unreadActions(line) } : { ...again, targets: [...again.targets, ...own], ...(read.attributes ? { attributes: read.attributes } : {}) };
  }

  /** What a destroyer the target reader has no row for destroys, as entities: undefined when a name is known only when it runs. */
  private destroyedEntities(words: readonly string[], cwd: string | undefined): Target[] | undefined {
    if (words.includes(UNKNOWN)) return undefined;
    const destruction = targetsOf({ words, cwd, writes: [] }, this.catalogueFacts());
    if (destruction) {
      if (destruction.unknown || destruction.targets.length === 0) return undefined;
      const action = destruction.op === "stop" ? "stop" : "delete";
      return destruction.targets.map(([entity]) => ({ kind: entity.kind as Target["kind"], value: entity.name, action, entity: { ...entity } as Target["entity"], ...(cwd ? { dir: cwd } : {}) }));
    }
    const generic = genericDestroyer(words);
    if (!generic || generic.operands.length === 0 || generic.operands.includes(UNKNOWN)) return undefined;
    const action = /^(kill|stop|terminate)$/i.test(generic.verb) ? "stop" : "delete";
    const type = basename(words[0]);
    return generic.operands.map((name) => ({ kind: "resource" as Target["kind"], value: name, action, entity: { kind: "resource", name, type } as Target["entity"], ...(cwd ? { dir: cwd } : {}) }));
  }

  /** The action classes the line's unreadable dangers could be, for a consent to say (explicit.ts): the danger keys' families. */
  private unreadActions(line: LineAssessment): string[] {
    const actions = new Set<string>();
    for (const [index, item] of line.commands.entries()) {
      const assessment = line.assessments?.[index];
      if (assessment?.tier !== "danger") continue;
      const tool = readTool(item.words);
      if (tool && !("unknown" in tool)) actions.add(tool.action);
      const kind = `${assessment.kind ?? ""} ${assessment.key ?? ""}`;
      for (const [action, pattern] of UNREAD_ACTIONS) if (pattern.test(kind)) actions.add(action);
    }
    if (line.parsed.kind === "opaque" && /\b(push|publish|release|tag)\b/.test(line.kind ?? "")) actions.add("push");
    return [...actions];
  }

  /** A `git push`'s refspecs in their short spelling (risk.ts `normalRefspec`), so every spelling names the same target. */
  private shortRefspecs(words: readonly string[], cwd: string | undefined): readonly string[] {
    if (basename(words[0] ?? "") !== "git") return words;
    const at = words.indexOf("push");
    if (at < 0) return words;
    let remote = false;
    const current = cwd === undefined ? undefined : this.gitAt(cwd)?.current;
    return words.map((word, index) => {
      if (index <= at || word === UNKNOWN || word.startsWith("-")) return word;
      if (!remote) {
        remote = true;
        return word;
      }
      const forced = word.startsWith("+");
      const short = normalRefspec(forced ? word.slice(1) : word, current);
      return forced ? `+${short}` : short;
    });
  }

  private targetFacts(): TargetFacts {
    return {
      git: (dir) => this.gitAt(dir),
      tagExists: (dir, name) => this.tagExists(dir, name),
      readFile: (path) => {
        try {
          return readFileSync(path, "utf8");
        } catch {
          return undefined;
        }
      },
    };
  }

  private explicitFacts(): ExplicitFacts {
    return {
      originRepo: (dir) => this.originRepo(dir),
      release: (dir, commit, version) => this.releaseFacts(dir, commit, version),
      pullRequestsWithHead: (dir, branch) => this.pullRequestsWithHead(dir, branch),
      worktreeBranch: (path) => this.catalogueFacts().worktreeBranch?.(path),
      currentBranch: (dir) => this.gitAt(dir)?.current,
      own: () => this.ownEntities(),
    };
  }

  /**
   * This task's own branches and pull requests, by code-checked facts: the
   * branch checked out in each of the chat's project folders (not a default
   * one), the branches the ledger says it made, the heads of pull requests
   * it opened; the pull requests it opened, and those gh lists with its one
   * branch as their head. A consent that names an action and no target binds
   * to the one of these that fits (explicit.ts `ownTarget`).
   */
  private ownEntities(): { branches: string[]; prs: PullRequestFact[] } {
    const entries = this.created.ledger.all();
    const prs: PullRequestFact[] = entries
      .filter((entry) => entry.kind === "pr" && (entry.action ?? "create") === "create")
      .map((entry) => ({ number: entry.name, ...(entry.head ? { head: entry.head } : {}), ...(entry.repo && !entry.repo.startsWith("/") ? { repo: entry.repo.toLowerCase() } : {}) }));
    const branches = new Set<string>([
      ...this.workspaceBranches(),
      ...entries.filter((entry) => entry.kind === "branch" && entry.remote === undefined && (entry.action ?? "create") === "create").map((entry) => entry.name),
      ...prs.flatMap((pr) => (pr.head ? [pr.head] : [])),
    ]);
    if (branches.size === 1) for (const pr of this.pullRequestsWithHead(this.cwd, [...branches][0])) if (!prs.some((known) => known.number === pr.number)) prs.push(pr);
    return { branches: [...branches], prs };
  }

  /** Pull requests by head branch, as gh last said (per directory and branch). */
  private readonly prsByHead = new Map<string, PullRequestFact[]>();

  /** The pull requests whose head is `branch`: the ledger's, and gh's for the repository at `dir`. */
  private pullRequestsWithHead(dir: string | undefined, branch: string): PullRequestFact[] {
    const own = this.created.ledger
      .all()
      .filter((entry) => entry.kind === "pr" && entry.head === branch)
      .map((entry): PullRequestFact => ({ number: entry.name, head: branch, ...(entry.repo ? { repo: entry.repo.toLowerCase() } : {}) }));
    // gh only where origin is a hosted repository it can ask.
    if (branch === UNKNOWN || branch.startsWith("-") || this.originRepo(dir ?? this.cwd) === undefined) return own;
    const key = `${dir ?? this.cwd}\0${branch}`;
    if (!this.prsByHead.has(key)) {
      const result = spawnSync("gh", ["pr", "list", "--head", branch, "--state", "all", "--limit", "5", "--json", "number,headRefName,baseRefName,state"], {
        cwd: dir ?? this.cwd,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
        timeout: 8000,
      });
      let listed: PullRequestFact[] = [];
      try {
        const parsed = result.status === 0 ? (JSON.parse(result.stdout) as { number?: unknown; headRefName?: unknown; baseRefName?: unknown; state?: unknown }[]) : [];
        listed = parsed
          .filter((item) => typeof item.number === "number" && item.headRefName === branch)
          .map((item) => ({ number: String(item.number), head: branch, ...(typeof item.baseRefName === "string" ? { base: item.baseRefName } : {}), ...(typeof item.state === "string" ? { state: item.state.toLowerCase() } : {}) }));
      } catch {
        listed = [];
      }
      this.prsByHead.set(key, listed);
    }
    return [...own, ...(this.prsByHead.get(key) ?? [])];
  }

  /** A tag of that name exists in the repository at `dir`. */
  tagExists(dir: string, name: string): boolean {
    if (name === UNKNOWN || name.startsWith("-")) return false;
    return spawnSync("git", ["-C", dir, "rev-parse", "-q", "--verify", `refs/tags/${name}`], { stdio: "ignore", timeout: 3000 }).status === 0;
  }

  /** The `owner/name` of the repository `origin` points at in `dir`. */
  originRepo(dir: string): string | undefined {
    const result = spawnSync("git", ["-C", dir, "remote", "get-url", "origin"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
    return result.status === 0 ? repoName(result.stdout.trim()) : undefined;
  }

  /**
   * Whether a release tag's commit is on the remote's default branch (as
   * last fetched), and holds the tag's version in its files: "release a
   * new version" then covers a tag of that version there and no other.
   */
  releaseFacts(dir: string, commit: string, version: string): { onDefault?: boolean; hasVersion?: boolean } {
    const git = (...args: string[]) => spawnSync("git", ["-C", dir, ...args], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 5000 });
    if (commit === UNKNOWN || commit.startsWith("-")) return {};
    const resolved = git("rev-parse", "-q", "--verify", `${commit}^{commit}`);
    if (resolved.status !== 0) return {};
    const sha = resolved.stdout.trim();
    let onDefault: boolean | undefined;
    for (const branch of this.gitAt(dir)?.defaults ?? []) {
      const ref = `refs/remotes/origin/${branch}`;
      if (git("rev-parse", "-q", "--verify", ref).status !== 0) continue;
      onDefault = git("merge-base", "--is-ancestor", sha, ref).status === 0;
      if (onDefault) break;
    }
    const grep = git("grep", "-q", "-F", "-w", "-e", version, sha, "--");
    return { onDefault, hasVersion: grep.status === 0 ? true : grep.status === 1 ? false : undefined };
  }

  /** A chat or task grant to run commands like these outside the sandbox. */
  private addCommandGrant(pattern: Offered, scope: "chat" | "task", by: "judge" | "user", label: string | undefined): void {
    const grant = newGrant({
      capability: "read",
      paths: [],
      pattern: pattern.match,
      label,
      sha256: pattern.sha256,
      ...(pattern.env.length ? { env: [...pattern.env] } : {}),
      scope,
      approvedBy: by,
      chatId: this.chatId,
      ...(scope === "task" ? { turn: this.grants.turn, dangers: [...pattern.dangers] } : {}),
    });
    this.grants.add(grant);
    if (scope === "chat") this.appendEntry?.(GRANT_ENTRY, grant);
  }

  // --- Migrating rules from before risk tiers ----------------------------------

  /**
   * Once per user file (`rulesVersion` 2): drop the command rules the risk
   * tiers make useless or too broad, after a backup. Inspections and
   * filters need no rule now; a dangerous one would no longer vouch (it
   * asks every task); a one-off command as written never matches again.
   * Returns what was removed and why, for one notice.
   */
  migrateRules(): { removed: { rule: string; why: string }[]; backup: string } | undefined {
    const root = readUserConfig(this.agentDir);
    if (!root || root.rulesVersion === 2) return undefined;
    const commands = Array.isArray(root.commands) ? (root.commands as CommandRule[]) : [];
    if (commands.length === 0) {
      editUserConfig(this.agentDir, (file) => {
        file.rulesVersion = 2;
      });
      this.configStamp = this.readStamp();
      return undefined;
    }
    const removed: { rule: string; why: string }[] = [];
    const facts = this.riskFacts();
    const kept = commands.filter((rule) => {
      const why = rule.decision === "allow" && Array.isArray(rule.match) && rule.acknowledgeDanger !== true ? this.ruleFate(rule, facts) : undefined;
      if (why) removed.push({ rule: describePattern(rule.match), why });
      return !why;
    });
    const stamp = new Date().toISOString().slice(0, 10);
    const backup = join(this.agentDir, `${CONFIG_FILE}.bak-${stamp}`);
    try {
      writeFileSync(backup, `${JSON.stringify(root, null, 2)}\n`, { mode: 0o600 });
    } catch {
      return undefined;
    }
    editUserConfig(this.agentDir, (file) => {
      file.commands = kept;
      file.rulesVersion = 2;
    });
    this.config.commands = this.config.commands.filter((rule) => !removed.some((item) => item.rule === describePattern(rule.match)));
    this.configStamp = this.readStamp();
    return { removed, backup };
  }

  private ruleFate(rule: CommandRule, facts: RiskFacts): string | undefined {
    const literal = rule.match.filter((word) => word !== "**");
    const open = rule.match.includes("**");
    if (literal.some((word) => /[*?]/.test(word))) return undefined;
    if (!open && filterInputs(literal) !== undefined) return "a filter, which needs no rule";
    const worst = assessCommand({ words: open ? [...literal, UNKNOWN] : literal, cwd: this.cwd, reads: [], writes: [] }, facts);
    if (worst.tier === "danger") return `dangerous (${worst.kind}): it asks each task now`;
    const exact = assessCommand({ words: literal, cwd: this.cwd, reads: [], writes: [] }, facts);
    if (exact.tier === "inspect" && INSPECT.some((entry) => inspectMatches(entry, viewOf(literal)))) return "it only inspects, which runs without a rule now";
    if (!open && literal.slice(1).some((word) => word.startsWith("/"))) return "a one-off command as written";
    return undefined;
  }

  // --- Facts for the risk classifier -----------------------------------------

  private readonly gitFacts = new Map<string, GitFacts | undefined>();
  private kubeCurrent: string | null | undefined;

  /** What risk.ts needs to know about this machine. */
  riskFacts(): RiskFacts {
    return {
      classify: (path, cwd) => (cwd === undefined && !isAbsolute(path) && !path.startsWith("~") ? undefined : this.classify(path, cwd ?? this.cwd)),
      disposable: (path) => this.paths.inWorkspace(path) || this.paths.inScratch(path) || this.paths.isWritableSupport(path),
      protectedTargets: this.config.protectedTargets,
      inspect: this.config.inspect,
      ownsProcesses: (target) => this.ownsProcesses(target),
      processFacts: (target) => this.processFacts(target),
      created: {
        has: (path) => this.created.has(path),
        worktree: (path) => this.created.worktree(path),
        branch: (name) => this.created.branch(name),
        named: (name) => this.created.named(name),
        pullRequest: (number) => this.created.all().entities.some((entry) => entry.kind === "pr" && (entry.action ?? "create") === "create" && entry.name === number),
        destroys: (command) => {
          const found = this.created.destroys({ ...command, cwd: command.cwd }, this.catalogueFacts());
          return found ? { own: found.own, op: found.op, why: found.why, kinds: found.kinds } : undefined;
        },
      },
      permissionSystem: (path) => this.paths.isPermissionSystem(path) !== undefined,
      couldBe: (pattern, cwd, options) => this.couldBe(pattern, cwd, options),
      tainted: (path) => this.taintedPath(path),
      taintedRepo: (dir) => this.taintedRepo(dir),
      secretInside: (dir) => this.secretInside(dir),
      permissionPlaces: () => this.permissionPlaces(),
      installedApp: (path) => this.installedApp(path),
      git: (dir) => this.gitAt(dir),
      inWorkspace: (dir) => this.paths.inWorkspace(this.paths.canonical(dir, this.cwd)),
      workspaceBranches: () => this.workspaceBranches(),
      kubeContext: (args) => this.kubeContext(args),
      programIsLocal: (words) => {
        const resolved = this.resolveIn(words[0] ?? "", this.cwd);
        return resolved !== undefined && this.writableRoots().some((root) => isUnder(resolved, root));
      },
      projectTool: (name, cwd) =>
        name !== UNKNOWN &&
        /^[@\w./-]+$/.test(name) &&
        this.toolDirs(cwd).some((root) => [join(root, "node_modules", ".bin", basename(name)), join(root, ".venv", "bin", basename(name))].some((path) => existsSync(path))),
    };
  }

  /**
   * Where a project tool may resolve from (`npx` walks up from where it runs): the chat's cwd and
   * project folders, and every directory from `cwd` up to the project folder holding it.
   */
  /**
   * What a word known only when it runs could be (round 5): every glob it stands for (resolve.ts
   * `resolvePattern`: braces, the environment, `~`, the directory, `..`, symlinks), classified by what it
   * matches now and by the patterns it could match (paths.ts `classifyPattern`).
   */
  couldBe(pattern: string, cwd: string | undefined, options: { subtree?: boolean } = {}): PatternFinding {
    const at = cwd === undefined ? undefined : this.paths.canonical(cwd, this.cwd);
    const resolved = resolvePattern(pattern, { home: this.home, cwd: at });
    const finding: PatternFinding & { matches: Classification[]; fixed: string[] } = { anywhere: resolved.anywhere, fixed: [], matches: [] };
    for (const glob of resolved.globs) {
      const fixed = globBase(glob.replace(/\\(.)/g, "$1"));
      if (!finding.fixed.includes(fixed)) finding.fixed.push(fixed);
      const found = this.paths.classifyPattern(glob, { ...options, unknownText: resolved.unknownText });
      finding.matches.push(...found.matches);
      finding.sensitive ??= found.sensitive;
      finding.permissionSystem ??= found.permissionSystem;
      if (found.guard === "self" || (found.guard && !finding.guard)) finding.guard = found.guard;
      const installed = [...found.matches.map((item) => item.path), ...(resolved.anywhere ? [] : [fixed])].find((path) => this.installedApp(path) && !installedProgram(path));
      finding.installed ??= installed;
    }
    return finding;
  }

  private toolDirs(cwd: string | undefined): string[] {
    const dirs = [this.cwd, ...this.roots];
    if (cwd === undefined) return dirs;
    let dir = this.paths.canonical(cwd, this.cwd);
    const root = this.roots.find((candidate) => isUnder(dir, candidate));
    if (root === undefined) return dirs;
    for (let depth = 0; depth < 32 && dir !== root && isUnder(dir, root); depth++) {
      dirs.push(dir);
      dir = dirname(dir);
    }
    return dirs;
  }

  private gitAt(dir: string): GitFacts | undefined {
    if (this.gitFacts.has(dir)) return this.gitFacts.get(dir);
    const git = (...args: string[]) => {
      const result = spawnSync("git", ["-C", dir, ...args], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
      return result.status === 0 ? result.stdout.trim() : undefined;
    };
    let facts: GitFacts | undefined;
    if (git("rev-parse", "--git-dir") !== undefined) {
      const remoteHead = git("rev-parse", "--abbrev-ref", "origin/HEAD");
      const defaults = [...new Set(["main", "master", ...(remoteHead ? [remoteHead.replace(/^origin\//, "")] : [])])];
      const branches = new Set((git("for-each-ref", "--format=%(refname:short)", "refs/heads") ?? "").split("\n").filter(Boolean));
      facts = { current: git("symbolic-ref", "--short", "-q", "HEAD"), defaults, isBranch: (name) => branches.has(name) };
    }
    this.gitFacts.set(dir, facts);
    return facts;
  }

  private kubeContext(args: readonly string[]): string | undefined {
    for (let i = 0; i < args.length; i++) {
      if (args[i] === "--context") return args[i + 1] === UNKNOWN ? undefined : args[i + 1];
      if (args[i].startsWith("--context=")) return args[i].slice("--context=".length);
    }
    if (this.kubeCurrent === undefined) {
      const result = spawnSync("kubectl", ["config", "current-context"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
      this.kubeCurrent = result.status === 0 ? result.stdout.trim() : null;
    }
    return this.kubeCurrent ?? undefined;
  }

  /** The variable every command the agent runs carries, so its processes can be told from the user's. */
  get processMarker(): { name: string; value: string } | undefined {
    return this.chatId ? { name: PROCESS_MARKER, value: this.chatId } : undefined;
  }

  /**
   * Whether every process a `kill` would stop carries this chat's marker in
   * its environment (it started them, or they descend from its commands).
   * Undefined when that cannot be read.
   */
  ownsProcesses(target: KillTarget): boolean | undefined {
    const marker = this.processMarker;
    if (!marker) return undefined;
    let pids: number[];
    if ("pids" in target) pids = target.pids;
    else {
      const result = spawnSync("pgrep", target.pgrep, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
      if (result.status !== 0 && result.status !== 1) return undefined;
      pids = result.stdout.split("\n").filter(Boolean).map(Number);
    }
    const want = `${marker.name}=${marker.value}`;
    let unreadable = false;
    for (const pid of pids) {
      // Gone: stopping it does nothing. Another user's: never this chat's.
      try {
        process.kill(pid, 0);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === "ESRCH") continue;
        return false;
      }
      const environment = processEnvironment(pid);
      if (environment === undefined) unreadable = true;
      else if (!environment.split("\n").includes(want)) return false;
    }
    return unreadable ? undefined : true;
  }

  /**
   * The installed Cypher app or its data: `/Applications/Cypher.app`, and
   * `~/.cypher` outside the chats' worktrees (its data, Runtime and
   * settings), but never this chat's own folders (a workspace bot works in
   * its folder under `~/.cypher`). A canonical path.
   */
  installedApp(path: string): boolean {
    const data = join(this.home, ".cypher");
    if (this.paths.inWorkspace(path)) return false;
    return isUnder(path, INSTALLED_APP) || (isUnder(path, data) && !isUnder(path, join(data, "worktrees")));
  }

  /**
   * What a kill's targets are: this chat's (its marker, `ownsProcesses`),
   * running in its project folders (their working directory: a dev
   * instance or test server it started, also through `env -i`, which drops
   * the marker, or before an engine restart), or the installed app's (its
   * program, such as the installed Runtime's node, or its data as their
   * folder; what the chat started stays its own). Read live, so a reused pid
   * counts as what runs now.
   */
  processFacts(target: KillTarget): ProcessFacts {
    const owned = this.ownsProcesses(target);
    let pids: number[];
    if ("pids" in target) pids = target.pids;
    else {
      const result = spawnSync("pgrep", target.pgrep, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: 3000 });
      if (result.status !== 0 && result.status !== 1) return { owned };
      pids = result.stdout.split("\n").filter(Boolean).map(Number);
    }
    let inRoots: boolean | undefined = true;
    let inCreated: boolean | undefined = true;
    let installedApp = false;
    for (const pid of pids) {
      try {
        process.kill(pid, 0);
      } catch (error) {
        // Gone: stopping it does nothing.
        if ((error as NodeJS.ErrnoException).code === "ESRCH") continue;
        inRoots = false;
        inCreated = false;
        continue;
      }
      const place = processPlace(pid);
      const program = place.program ? this.paths.canonical(place.program, "/") : undefined;
      const cwd = place.cwd ? this.paths.canonical(place.cwd, "/") : undefined;
      if ((program && this.installedApp(program)) || (cwd && this.installedApp(cwd))) installedApp = true;
      if (cwd === undefined) {
        inRoots = inRoots === false ? false : undefined;
        inCreated = inCreated === false ? false : undefined;
        continue;
      }
      if (!this.paths.inWorkspace(cwd)) inRoots = false;
      // Its data directory, when it says (a dev instance this task started), else where it runs.
      const data = this.processDataDir(pid);
      if (!this.created.has(data ?? cwd)) inCreated = false;
    }
    return { owned, inRoots, inCreated, installedApp };
  }

  /**
   * The branches checked out in the chat's project folders: the work this
   * chat does (Cypher gives a chat a worktree on its own branch), which it
   * may delete when it is done with it. Never a default branch.
   */
  private workspaceBranches(): string[] {
    const names: string[] = [];
    for (const root of this.roots) {
      const git = this.gitAt(root);
      if (git?.current && !git.defaults.includes(git.current)) names.push(git.current);
    }
    return names;
  }

  /** What this task made, for the reviewing model's prompt (newest last, bounded). */
  /** A ledger entity, in words. */
  private describeLedger(entity: LedgerEntity & { action?: string }): string {
    const where = entity.remote ? ` on ${entity.remote}` : "";
    switch (entity.kind) {
      case "path":
        return displayPath(entity.name, this.home);
      case "worktree":
        return `the worktree ${displayPath(entity.name, this.home)}`;
      case "pr":
        return `${entity.action === "merge" ? "the merged " : ""}pull request #${entity.name}${entity.repo && !entity.repo.startsWith("/") ? ` of ${entity.repo}` : ""}${entity.head ? ` (head ${entity.head})` : ""}`;
      default:
        return `${entity.action === "start" ? "the started " : "the "}${entity.kind} ${entity.name}${where}`;
    }
  }

  /**
   * What the ledger says, for the reviewing model: first how this line's
   * targets follow from what this chat did (lifecycle.ts), then the rest,
   * newest first. Cypher recorded it from the results of the chat's own
   * commands; the agent can't write it.
   */
  private createdSummary(line?: LineAssessment): string[] {
    const lines: string[] = [];
    if (line?.parsed.kind === "simple") {
      const facts = this.catalogueFacts();
      for (const command of line.commands) {
        let found;
        try {
          found = this.created.destroys(command, facts);
        } catch {
          found = undefined;
        }
        if (!found) continue;
        for (const alternatives of found.targets) {
          for (const entity of alternatives) {
            const follow = this.created.followUp(found.op === "stop" ? "stop" : "delete", entity, facts);
            if (!follow) continue;
            const by = (follow.antecedent.entity as { by?: string }).by;
            lines.push(`\`${command.words.join(" ").slice(0, 160)}\` ${found.op === "stop" ? "stops" : "removes"} ${this.describeLedger(entity)}: ${follow.rule.why}; this chat did ${this.describeLedger(follow.antecedent.entity)}${by ? ` (\`${by.slice(0, 120)}\`)` : ""}`);
            break;
          }
        }
      }
    }
    for (const entry of [...this.created.ledger.all()].reverse()) {
      if (lines.length >= 30) break;
      lines.push(this.describeLedger(entry));
    }
    return [...new Set(lines)];
  }

  /** The data directory a process says it uses (`CYPHER_DATA_DIR`), canonical; undefined when it names none. */
  private processDataDir(pid: number): string | undefined {
    const environment = processEnvironment(pid);
    const value = environment
      ?.split("\n")
      .find((line) => line.startsWith("CYPHER_DATA_DIR="))
      ?.slice("CYPHER_DATA_DIR=".length);
    return value ? this.paths.canonical(value, "/") : undefined;
  }

  statusText(): string {
    const lines: string[] = [];
    if (!this.status.available) {
      lines.push(`Permission control is OFF on this device: ${this.status.error ?? "the OS sandbox does not work"}.`);
      return lines.join("\n");
    }
    const levelName = { "read-only": "Ask before edits", standard: "Edit project", "full-access": "Full access (no restrictions)" }[this.level];
    lines.push(`Level: ${levelName} (change it with /permissions)`);
    lines.push(`Sandbox: Landstrip (${this.status.implementation ?? "available"})`);
    lines.push(`Project: ${this.project}`);
    const paused = this.judgePaused();
    if (paused) lines.push(paused);
    lines.push("", "Project folders:", ...this.roots.map((root) => `  ${root}`));
    const matrix = this.config.levels[this.level];
    lines.push("", "Defaults:");
    for (const zone of ["workspace", "external", "sensitive"] as const) {
      lines.push(`  ${zone.padEnd(10)} read ${matrix[zone].read.padEnd(8)} write ${matrix[zone].write}`);
    }
    const network =
      this.config.network.mode === "filtered"
        ? `filtered: ${this.config.network.allowDomains.length} allowed domains, others ask`
        : "on";
    lines.push("  protected  write elevate", `  network    ${network}`);
    lines.push("", "Approval:");
    for (const key of ["external.write", "protected.write", "self.write", "sensitive.read", "sensitive.write", "network", "tools", "escalate"]) {
      lines.push(`  ${key.padEnd(16)} ${approvalModeFor(this.config, key)}`);
    }
    lines.push("", this.grantsText());
    if (this.warnings.length) lines.push("", "Warnings:", ...this.warnings.map((warning) => `  ${warning}`));
    return lines.join("\n");
  }

  grantsText(): string {
    const grants = this.grants.all();
    const lines = [grants.length ? "Grants:" : "No grants in this chat."];
    for (const grant of grants) {
      const what = grant.pattern
        ? `outside the sandbox: ${describePattern(grant.pattern)}${grant.label ? ` (${grant.label})` : ""}`
        : `${grant.capability} ${grant.paths.join(", ")}`;
      lines.push(`  ${grant.id.slice(0, 8)}  ${grant.scope.padEnd(5)} ${what} (by ${grant.approvedBy})`);
    }
    if (this.config.rules.length) {
      lines.push("", "Always-allow rules (permissions.json):");
      for (const rule of this.config.rules) lines.push(`  ${rule.capability} ${rule.path}`);
    }
    if (this.config.commands.length) {
      lines.push("", "Command rules (permissions.json and the project's):");
      for (const rule of this.config.commands) lines.push(`  ${rule.decision.padEnd(5)} ${describePattern(rule.match)}${rule.label ? ` (${rule.label})` : ""}`);
    }
    return lines.join("\n");
  }

  // --- Sandbox inputs -------------------------------------------------------

  private writableSupportDirs(): string[] {
    return this.config.writableSupport.map((pattern) => {
      const expanded = pattern.startsWith("~/") ? join(this.home, pattern.slice(2)) : pattern;
      return expanded.endsWith("/**") ? expanded.slice(0, -3) : globBase(expanded);
    });
  }

  /** Concrete sensitive, protected and self paths for a launch. */
  async concretePaths(signal?: AbortSignal): Promise<ConcretePaths> {
    const key = JSON.stringify([this.roots, this.paths.ownFolders, this.config.sensitive, this.config.protected, this.config.self]);
    if (this.sensitiveCache && this.sensitiveCache.key === key && Date.now() - this.sensitiveCache.at < 1500) {
      return this.sensitiveCache.paths;
    }
    const rg = findRipgrep(this.agentDir);
    const roots = this.roots;
    const sensitive = splitPatterns(this.config.sensitive, roots, this.home);
    const sensitiveRead: string[] = [];
    const sensitiveWrite: string[] = [];
    const deferred: { base: string; glob: string }[] = [];
    for (const pattern of sensitive.absolute) {
      const read = expandAbsolute(pattern, false);
      const write = expandAbsolute(pattern, true);
      sensitiveRead.push(...read.paths);
      sensitiveWrite.push(...write.paths);
      if (read.deferred) deferred.push(read.deferred);
    }
    const globs = ripgrepGlobs(sensitive.anywhere);
    const indexKey = JSON.stringify(globs);
    let homeIndex = this.homeIndex.read(indexKey);
    if (!this.homeIndex.isFresh(homeIndex)) {
      const refresh = this.homeIndex.refresh(indexKey, globs, rg);
      // The first launch ever waits for the index; later ones use the stale copy.
      if (!homeIndex) {
        await refresh;
        homeIndex = this.homeIndex.read(indexKey);
      }
    }
    const [workspaceHits, ...deferredHits] = await Promise.all([
      listMatching(globs, roots, { rg, signal, timeoutMs: 15_000 }),
      ...deferred.map((item) => listMatching([item.glob], [item.base], { rg, signal, timeoutMs: 15_000 })),
    ]);
    const anywhere = anchorDirectories([...workspaceHits, ...(homeIndex?.files ?? [])], sensitive.anywhere);
    // A sensitive directory with an exception inside (~/.ssh and its config)
    // is denied child by child, around the exception.
    const inside = sensitive.exceptions.filter((exception) => exception.startsWith("/"));
    const carve = (list: string[]) => list.flatMap((path) => splitSelf(path, inside));
    // The chat's own folder in Cypher's data (paths.ts `ownFolders`) is carved
    // out of the directories denied for where they are (`~/.cypher/orgs`), and
    // only of those: a secret found inside it by name stays denied.
    const own = this.paths.ownFolders.filter((root) => !hasGlob(root)).map((root) => `${root}/**`);
    const carveOwn = (list: string[]) => list.flatMap((path) => splitSelf(path, [...inside, ...own]));
    const found = [...anywhere, ...deferredHits.flat()];
    const allSensitive = dropExceptions([...carveOwn(sensitiveRead), ...carve(found)], sensitive.exceptions, roots, this.home);
    const allSensitiveWrite = dropExceptions([...carveOwn(sensitiveWrite), ...carve(found)], sensitive.exceptions, roots, this.home);

    const guarded = splitPatterns(this.config.protected, roots, this.home);
    const protectedPaths: string[] = [];
    for (const pattern of guarded.absolute) protectedPaths.push(...expandAbsolute(pattern, true).paths);
    // `.git` lives where git says (worktrees share the main repository's).
    for (const gitDir of this.gitDirs) {
      // `commondir` and `gitdir` say where the repository is: rewritten, they could point git at a config the agent wrote.
      for (const name of ["config", "config.worktree", "hooks", "commondir", "gitdir", "info"]) protectedPaths.push(join(gitDir, name));
      // Submodules keep their repositories under modules/: each its own config and hooks.
      for (const module of submoduleGitDirs(gitDir)) for (const name of ["config", "hooks", "info"]) protectedPaths.push(join(module, name));
    }
    const nested = guarded.anywhere.length ? await listMatching(ripgrepGlobs(guarded.anywhere), roots, { rg, signal, timeoutMs: 15_000 }) : [];
    protectedPaths.push(...anchorDirectories(nested, guarded.anywhere));

    const self = splitPatterns(this.config.self, roots, this.home);
    const selfRoots: string[] = [];
    for (const pattern of self.absolute) {
      const base = pattern.endsWith("/**") ? pattern.slice(0, -3) : pattern;
      selfRoots.push(...expandAbsolute(base, false).paths);
    }

    const paths: ConcretePaths = {
      sensitiveRead: [...new Set(allSensitive)],
      sensitiveWrite: [...new Set(allSensitiveWrite)],
      protectedPaths: dropExceptions([...new Set(protectedPaths)], guarded.exceptions, roots, this.home),
      selfRoots: [...new Set(selfRoots)],
      selfExceptions: [...self.exceptions, ...own],
    };
    this.sensitiveCache = { at: Date.now(), key, paths };
    return paths;
  }

  /**
   * What git writes for a linked worktree outside it: the worktree's own
   * git directory (index, HEAD, logs) and the repository's objects, refs and
   * reflogs. The repository's config, hooks and `info/` stay protected
   * (`concretePaths`), so a commit, a branch or a merge is local work.
   */
  private gitWritable(): string[] {
    const git = this.worktreeGit;
    if (!git || this.config.levels[this.level].workspace.write !== "allow") return [];
    return [git.gitDir, ...["objects", "refs", "logs", "packed-refs", "packed-refs.lock"].map((name) => join(git.common, name))];
  }

  /** The Landstrip policy for one bash command or one ripgrep run. */
  async compile(options: { readOnly?: boolean; signal?: AbortSignal; proxyPort?: number }): Promise<CompiledPolicy> {
    const paths = await this.concretePaths(options.signal);
    const grants = this.grants.forSandbox();
    return compilePolicy({
      config: this.config,
      level: this.level,
      roots: this.roots,
      supportDirs: this.writableSupportDirs(),
      tempDirs: tempDirs(),
      paths,
      grants: [
        ...grants,
        // Persistent rules count as unbound non-sensitive grants.
        ...this.config.rules
          .filter((rule) => this.ruleApplies(rule))
          .map((rule) => newGrant({ capability: rule.capability, paths: [rule.path], scope: "always", approvedBy: "rule" })),
      ],
      gitWritable: options.readOnly ? [] : this.gitWritable(),
      readOnly: options.readOnly,
      proxyPort: options.proxyPort,
      home: this.home,
    });
  }

  /** The hint appended to a sandboxed command that failed. */
  denialHint(compiled: CompiledPolicy, denials: { kind: string; operation: string; target: string }[]): string {
    const lines = ["[permissions] This command ran in a sandbox."];
    if (denials.length) {
      lines.push("Blocked:");
      for (const denial of denials.slice(0, 10)) lines.push(`- ${denial.operation} ${denial.target}`);
    } else {
      lines.push(
        "Writes outside the project folders, protected files (.git/config, git hooks, shell startup files), Cypher's own data and sensitive files (.env, keys, ~/.ssh and other credentials) are blocked.",
      );
    }
    if (compiled.policy.network.httpProxyPort !== undefined) {
      lines.push("The network goes through a filter (HTTP_PROXY/HTTPS_PROXY): allowed domains pass, new ones ask for approval.");
    }
    lines.push(ESCALATE_HINT);
    return lines.join("\n");
  }

  // --- Filtered network -----------------------------------------------------

  /** The filter proxy's port in filtered mode (started on first use), else undefined. */
  async proxyPort(): Promise<number | undefined> {
    if (this.config.network.mode !== "filtered" || !this.enforcing) return undefined;
    this.proxy ??= startFilterProxy({
      isDomainAllowed: (domain) => this.domainAllowed(domain),
      authorization: `Basic ${Buffer.from(`cypher:${this.proxyToken}`).toString("base64")}`,
    }).catch(() => undefined);
    return (await this.proxy)?.port;
  }

  /** Proxy variables for a command that reaches the network through the filter. */
  proxyEnv(port: number): Record<string, string> {
    const url = `http://cypher:${this.proxyToken}@127.0.0.1:${port}`;
    const local = "localhost,127.0.0.1,::1";
    return { HTTP_PROXY: url, HTTPS_PROXY: url, ALL_PROXY: url, http_proxy: url, https_proxy: url, all_proxy: url, NO_PROXY: local, no_proxy: local };
  }

  beginCommand(id: string, command: string, ctx: RequestContext): void {
    this.commands.set(id, { command, ctx, domains: new Set() });
  }

  endCommand(id: string): void {
    this.commands.delete(id);
  }

  /** The filter proxy's question: may a running command reach `domain`? */
  async domainAllowed(domain: string): Promise<boolean> {
    const name = domain.toLowerCase().replace(/\.$/, "");
    if (this.config.network.allowDomains.some((pattern) => domainMatches(name, pattern))) return true;
    const key = `net:${name}`;
    const grant = this.grants.findForFile("read", key);
    if (grant) {
      this.grants.used(grant);
      return true;
    }
    if (this.config.rules.some((rule) => rule.path === key && this.ruleApplies(rule))) return true;
    for (const running of this.commands.values()) if (running.domains.has(name)) return true;
    const running = [...this.commands.values()].at(-1);
    if (!running) return false;
    const mode = approvalModeFor(this.config, "network");
    if (mode === "deny") {
      this.audit.write({ tool: "bash", command: running.command, capability: "network", paths: [name], decision: "deny", decidedBy: "policy" });
      return false;
    }
    const classification: Classification = { path: key, zone: "external", match: "network" };
    const outcome = await this.serialized(() =>
      this.elevate({ capability: "read", classifications: [classification], key: "network", mode, tool: "bash", command: running.command, domain: name }, running.ctx),
    );
    if (!outcome.allowed) return false;
    if (outcome.scope === "once") running.domains.add(name);
    else this.rememberPseudoPath(key, outcome.scope, outcome.by);
    return true;
  }

  async stopProxy(): Promise<void> {
    const proxy = await this.proxy;
    this.proxy = undefined;
    await proxy?.stop();
  }

  // --- Unavailable host -----------------------------------------------------

  /** Once per chat: tell the user permission control is off on this device. */
  noticeUnavailable(entries: readonly Entry[], ui: UiLike): void {
    if (this.status.available) return;
    if (entries.some((entry) => entry.type === "custom" && entry.customType === UNAVAILABLE_ENTRY)) return;
    ui.notify(`Permission control is unavailable on this device: ${this.status.error ?? "the OS sandbox does not work"}. Commands and file tools run without restrictions.`, "warning");
    this.appendEntry?.(UNAVAILABLE_ENTRY, { chatId: this.chatId, error: this.status.error });
  }
}

function describeAll(classifications: readonly Classification[]): string {
  return classifications.map((item) => describe(item)).join(", ");
}

/**
 * The dialog text: `question\n\nContext:\n` and one `Label: value` line per
 * fact, a `Change:` or `Input:` block last. Cypher's question cards read the
 * labels (the desktop lays them out as rows); any other client shows clean
 * text. The labels are a contract with `crates/ui/src/composer/permission_card.rs`,
 * which also recognises a permission request by its "Deny and stop" option.
 */
function approvalTitle(
  request: { capability: Capability; classifications: Classification[]; tool: string; command?: string; justification?: string; diff?: string; toolInput?: string; domain?: string },
  judgeNote: string | undefined,
  home: string,
  dangerous = false,
): string {
  const lines: string[] = [];
  const review = () => {
    if (judgeNote) lines.push(fact("Review", /^(Automatic review is paused|Only you can approve this)/.test(judgeNote) ? judgeNote : `The reviewing model couldn't approve this automatically. ${judgeNote}`));
  };
  if (request.domain !== undefined) {
    lines.push(fact("Domain", request.domain));
    if (request.command) lines.push(fact("Command", request.command));
    lines.push("Note: Commands reach the network only through the filter; other domains still ask.");
    review();
    return `Allow the agent to reach ${request.domain}?\n\nContext:\n${lines.join("\n")}`;
  }
  if (request.toolInput !== undefined) {
    lines.push(fact("Tool", request.tool));
    review();
    lines.push("Input:", request.toolInput);
    return `Allow the agent to use ${request.tool}?\n\nContext:\n${lines.join("\n")}`;
  }
  const first = request.classifications[0];
  const what =
    first?.zone === "sensitive"
      ? `${request.capability} a sensitive file`
      : first?.guard === "self"
        ? "change Cypher's own data"
        : first?.guard === "protected"
          ? "write a protected file"
          : `${request.capability} outside the project folders`;
  if (dangerous) lines.push(first?.guard === "self" ? "Note: Dangerous: it changes Cypher's own data." : "Note: Dangerous: a write here runs code later.");
  for (const item of request.classifications) lines.push(fact("Path", displayPath(item.path, home)));
  const matches = [...new Set(request.classifications.map((item) => item.match).filter((match): match is string => Boolean(match)))];
  if (matches.length) lines.push(fact("Matches", matches.join(", ")));
  if (request.tool !== "request_permission") lines.push(fact("Tool", request.tool));
  if (request.justification) lines.push(fact("Reason", request.justification));
  const sensitive = request.classifications.some((item) => item.zone === "sensitive");
  if (sensitive && request.capability === "read") lines.push("Note: The contents will be sent to the model provider.");
  if (request.tool === "request_permission") lines.push("Note: For the agent's file tools only; commands such as ssh never get it.");
  review();
  if (request.diff) lines.push("Change:", request.diff.length > 1200 ? `${request.diff.slice(0, 1200)}\n…` : request.diff);
  return `Allow the agent to ${what}?\n\nContext:\n${lines.join("\n")}`;
}

/** Paths inside a sentence as the user knows them. */
function homeRelative(text: string, home: string): string {
  return text.split(`${home}/`).join("~/");
}

/** A path as the user knows it: the home directory as `~`. */
function displayPath(path: string, home: string): string {
  if (path === home) return "~";
  return path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path;
}

// --- Checked filesystem operations -------------------------------------------

async function verifiedOpen(path: string, flags: number) {
  const handle = await open(path, flags | constants.O_NOFOLLOW, 0o666);
  try {
    // The check ran on `path`; make sure the file opened is still the file
    // there, reached without a symlink (a swapped parent would differ).
    const opened = await handle.stat();
    const real = realpathSync.native(path);
    const now = statSync(real);
    if (real !== path || now.ino !== opened.ino || now.dev !== opened.dev) {
      throw new PermissionDenied(`Permission denied: ${path} changed while it was being opened.`);
    }
    return handle;
  } catch (error) {
    await handle.close();
    throw error;
  }
}

export async function checkedReadFile(permissions: Permissions, path: string, approved: Approved | undefined): Promise<Buffer> {
  const canonical = permissions.assert("read", path, approved);
  const handle = await verifiedOpen(canonical, constants.O_RDONLY);
  try {
    return await handle.readFile();
  } finally {
    await handle.close();
  }
}

export async function checkedWriteFile(permissions: Permissions, path: string, content: string | Buffer, approved: Approved | undefined): Promise<void> {
  const canonical = permissions.assert("write", path, approved);
  const handle = await verifiedOpen(canonical, constants.O_WRONLY | constants.O_CREAT);
  try {
    await handle.truncate(0);
    await handle.writeFile(content, typeof content === "string" ? "utf8" : undefined);
  } finally {
    await handle.close();
  }
}

export async function checkedMkdir(permissions: Permissions, path: string, approved: Approved | undefined): Promise<void> {
  const canonical = permissions.assertMkdir(path, approved);
  await mkdir(canonical, { recursive: true });
}

export async function checkedAccess(permissions: Permissions, capability: Capability, path: string, mode: number, approved: Approved | undefined): Promise<void> {
  const canonical = permissions.assert(capability, path, approved);
  await fsAccess(canonical, mode);
}

export async function checkedReaddir(permissions: Permissions, path: string, approved: Approved | undefined): Promise<string[]> {
  const canonical = permissions.assert("read", path, approved);
  return fsReaddir(canonical);
}

export async function checkedStat(path: string) {
  return fsStat(path);
}

const IMAGE_SIGNATURES: [string, number[]][] = [
  ["image/png", [0x89, 0x50, 0x4e, 0x47]],
  ["image/jpeg", [0xff, 0xd8, 0xff]],
  ["image/gif", [0x47, 0x49, 0x46, 0x38]],
];

export async function checkedImageType(permissions: Permissions, path: string, approved: Approved | undefined): Promise<string | undefined> {
  const canonical = permissions.assert("read", path, approved);
  const handle = await verifiedOpen(canonical, constants.O_RDONLY);
  try {
    const buffer = Buffer.alloc(16);
    const { bytesRead } = await handle.read(buffer, 0, 16, 0);
    const head = buffer.subarray(0, bytesRead);
    for (const [type, signature] of IMAGE_SIGNATURES) {
      if (signature.every((byte, index) => head[index] === byte)) return type;
    }
    if (head.subarray(0, 4).toString("latin1") === "RIFF" && head.subarray(8, 12).toString("latin1") === "WEBP") return "image/webp";
    return undefined;
  } finally {
    await handle.close();
  }
}

/** Matches `name` against a tool override key (`name` or `mcp:server`). */
export function toolOverride(config: PermissionsConfig, name: string, server: string | undefined): "allow" | "elevate" | "deny" {
  return config.tools.overrides[name] ?? (server ? config.tools.overrides[`mcp:${server}`] : undefined) ?? config.tools.mcp;
}

/** Paths an apply_patch input touches (add, update, delete, move target). */
export function patchPaths(patch: string): string[] {
  const out: string[] = [];
  for (const line of patch.split(/\r?\n/)) {
    const match = line.trim().match(/^\*\*\* (?:Add File|Delete File|Update File|Move to): (.+)$/);
    if (match) out.push(match[1].trim());
  }
  return out;
}

/** `example.com` matches itself; `*.example.com` matches its subdomains. */
export function domainMatches(domain: string, pattern: string): boolean {
  const want = pattern.trim().toLowerCase();
  if (!want) return false;
  if (want === "*") return true;
  if (want.startsWith("*.")) return domain.endsWith(want.slice(1));
  return domain === want;
}
