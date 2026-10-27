/**
 * The permission engine for one Pi process: configuration, zones, grants, the
 * approval pipeline (policy → grant → judge → human) and the per-call checks
 * the file tools run at the filesystem boundary. Nothing here imports Pi
 * values, so the whole pipeline runs under `node --test`.
 */

import { execFileSync, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { constants, existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { open, mkdir, access as fsAccess, stat as fsStat, readdir as fsReaddir } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve as resolvePath } from "node:path";
import { AuditLog } from "./audit.ts";
import type { AuditRecord } from "./audit.ts";
import { appendCommandRule, appendRule, approvalMode as approvalModeFor, CONFIG_FILE, editUserConfig, loadConfig, readUserConfig, resolveLevel } from "./config.ts";
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
import { globBase } from "./glob.ts";
import { GRANT_ENTRY, GrantStore, newGrant, REVOKE_ENTRY, ROOT_ENTRY } from "./grants.ts";
import type { Grant, GrantScope } from "./grants.ts";
import { conversationContext, judgePrompt, JUDGE_SYSTEM_PROMPT, parseVerdict } from "./judge.ts";
import type { JudgeRequest } from "./judge.ts";
import { isUnder, PathContext } from "./paths.ts";
import type { Classification } from "./paths.ts";
import { decide, describe } from "./policy.ts";
import type { Verdict } from "./policy.ts";
import { compilePolicy, landstripStatus, splitSelf, startFilterProxy, tempDirs } from "./sandbox.ts";
import type { CompiledPolicy, ConcretePaths, FilterProxy, LandstripStatus } from "./sandbox.ts";
import { benignEnv, quietSsh, categoryPattern, describePattern, fileSha256, filterInputs, isInert, patternError, patternMatches, resolveProgram, ruleCannotVouch, unwrap } from "./commands.ts";
import type { CommandRule } from "./commands.ts";
import { assessCommand, assessOpaque, downloadsAndRuns, redirectDanger, TIER_RANK } from "./risk.ts";
import type { Assessment, GitFacts, KillTarget, RiskFacts, Tier } from "./risk.ts";
import { INSPECT, inspectMatches, viewOf } from "./packs.ts";
import { parseShell, UNKNOWN } from "./shell.ts";
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

/** How sure the reviewing model must be to approve a danger. */
const DANGER_CONFIDENCE = 0.9;

/** A reviewing model that failed to sign in (an expired session, a missing key). */
const AUTH_FAILURE = /authenticat|unauthori[sz]ed|\b401\b|\b403\b|expired|log ?in|sign ?in|credential|api key|forbidden/i;

/** The variable each command the agent runs carries (`ownsProcesses`). */
export const PROCESS_MARKER = "CYPHER_AGENT_CHAT";

/** How long an approval lasts: a grant's scope, or a rule for this project, or an existing grant or rule. */
export type Scope = GrantScope | "project" | "rule";

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

const ESCALATE_HINT =
  "If the command needs what the sandbox withholds (files outside the workspace, credentials, keys, the keychain, other processes, system services), run it again with `escalate`: a reviewing model or the user decides whether it runs outside the sandbox.";

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
  inert: boolean;
  /** The wrappers that only run it (`nohup`, `timeout 30`). */
  wrappers: string[];
}

const DIRECTORY_BUILTINS: ReadonlySet<string> = new Set(["cd", "pushd", "popd"]);

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
}

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
  readonly status: LandstripStatus;
  config!: PermissionsConfig;
  level!: LevelName;
  paths!: PathContext;
  warnings: string[] = [];
  /** A command prefix from the settings runs before every command (it may export variables). */
  shellPrefix = false;
  private readonly levelEnv: string | undefined;
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
    this.cwd = init.cwd;
    this.audit = new AuditLog(init.agentDir, init.chatId);
    this.grants = new GrantStore(init.chatId);
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
      scratch: this.scratchDirs,
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
    return this.config.rules.some((rule) => (capability === "read" || rule.capability === "write") && isUnder(path, rule.path) && this.ruleApplies(rule));
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
    const grant = this.grants.findForFile(capability, canonical);
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
      if (verdict.kind === "allow") continue;
      if (verdict.kind === "deny") {
        this.audit.write({ tool: call.tool, capability: request.capability, zone: classification.zone, guard: classification.guard, paths: [canonical], decision: "deny", decidedBy: "policy", reason: verdict.reason });
        return { ok: false, reason: `Permission denied: ${verdict.reason}.` };
      }
      const grant = this.grants.findForFile(request.capability, canonical);
      if (grant) {
        this.audit.write({ tool: call.tool, capability: request.capability, zone: classification.zone, guard: classification.guard, paths: [canonical], decision: "allow", decidedBy: "grant", grantId: grant.id, scope: grant.scope });
        this.grants.used(grant);
        approved.add(request.capability, canonical);
        used("rule");
        continue;
      }
      if (classification.zone !== "sensitive" && this.ruleCovers(request.capability, canonical)) {
        this.audit.write({ tool: call.tool, capability: request.capability, zone: classification.zone, guard: classification.guard, paths: [canonical], decision: "allow", decidedBy: "rule" });
        approved.add(request.capability, canonical);
        used("rule");
        continue;
      }
      const outcome = await this.serialized(async () => {
        // An earlier dialog in the queue may have allowed it meanwhile.
        if (this.grants.findForFile(request.capability, canonical) || (classification.zone !== "sensitive" && this.ruleCovers(request.capability, canonical))) {
          return { allowed: true as const, scope: "rule" as const, by: "rule" as const };
        }
        return this.elevate(
          {
            capability: request.capability,
            classifications: [classification],
            key: verdict.key,
            mode: verdict.mode === "assisted" ? "assisted" : "ask",
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
   * Ask the reviewing model about one request. It approves, or says why it
   * could not; a model that cannot run (none here, paused, an error, a
   * timeout) never approves.
   */
  private async review(
    request: Omit<JudgeRequest, "level" | "workspaceRoots" | "userMessages" | "recentToolCalls">,
    ctx: RequestContext,
    minConfidence = this.config.judge.minConfidence,
  ): Promise<{ decision: "approve" | "ask-user"; reason: string; state: "approved" | "asked" | "unavailable" | "paused" }> {
    if (!ctx.complete) return { decision: "ask-user", reason: "No reviewing model is available here.", state: "unavailable" };
    const paused = this.judgePaused();
    if (paused) return { decision: "ask-user", reason: paused, state: "paused" };
    const { userMessages, recentToolCalls } = conversationContext(ctx.branch());
    const judgeRequest: JudgeRequest = { ...request, level: this.level, workspaceRoots: this.roots, userMessages, recentToolCalls };
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, this.config.judge.timeoutMs);
    const onAbort = () => controller.abort();
    ctx.signal?.addEventListener("abort", onAbort, { once: true });
    let text: string | undefined;
    let failure: string | undefined;
    try {
      text = await ctx.complete(JUDGE_SYSTEM_PROMPT, judgePrompt(judgeRequest), controller.signal);
    } catch (error) {
      text = undefined;
      failure = error instanceof Error ? error.message.trim() : undefined;
    } finally {
      clearTimeout(timer);
      ctx.signal?.removeEventListener("abort", onAbort);
    }
    if (text === undefined) {
      if (timedOut) {
        this.noteReview("timeout");
        return { decision: "ask-user", reason: `The reviewing model did not answer within ${Math.round(this.config.judge.timeoutMs / 1000)} seconds.`, state: "unavailable" };
      }
      const short = failure && failure.length > 200 ? `${failure.slice(0, 200)}…` : failure;
      if (failure && AUTH_FAILURE.test(failure)) this.noteReview("auth", short);
      const why = short ? `: ${short}` : ".";
      return { decision: "ask-user", reason: `The reviewing model failed${why}`, state: "unavailable" };
    }
    this.noteReview("ok");
    const verdict = parseVerdict(text, minConfidence);
    return { ...verdict, state: verdict.decision === "approve" ? "approved" : "asked" };
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
    },
    ctx: RequestContext,
  ): Promise<ElevateOutcome> {
    const started = Date.now();
    const paths = request.classifications.map((item) => item.path);
    const sensitive = request.classifications.some((item) => item.zone === "sensitive");
    const self = request.classifications.some((item) => item.guard === "self");
    const guarded = request.classifications.some((item) => item.guard === "protected");
    const file = request.domain === undefined && request.toolInput === undefined;
    // A protected write runs code later: dangerous, a human's alone.
    const tier: Tier = file && guarded && !sensitive && !self ? "danger" : "local";
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
    // Model first: the judge either approves automatically or puts the
    // question to the user with its reason.
    let judgeNote: string | undefined;
    if (request.mode === "assisted" && tier !== "danger") {
      const first = request.classifications[0];
      const verdict = await this.review(
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
        },
        ctx,
      );
      base.judge = verdict.state;
      if (verdict.decision === "approve") {
        // Remembered for the chat, so the same access is not reviewed again; a secret only once.
        const scope = sensitive ? "once" : "chat";
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
    const choice = await ctx.ui.select(title, options, {
      signal: ctx.signal,
      timeout: this.config.ask.timeoutMs ?? undefined,
    });
    const asked = { prompted: true, choice: choice ?? "dismissed" };
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
        finish({ decision: "abort", ...asked });
        ctx.abort?.();
        return { allowed: false, reason: "The user denied this and stopped the turn.", abort: true };
      default: {
        const reason = choice === HUMAN.deny ? "The user denied this request." : "No approval was given (the request was dismissed or timed out).";
        finish({ reason, ...asked });
        return { allowed: false, reason: `${reason} Do not retry the same access; continue without it or ask the user.` };
      }
    }
  }

  /** A tool-level approval for an MCP or extension tool whose setting is `elevate`. */
  async requestToolUse(name: string, input: unknown, ctx: RequestContext): Promise<{ ok: true } | { ok: false; reason: string; abort?: boolean }> {
    this.refresh();
    const key = `tool:${name}`;
    const covered = () => {
      const grant = this.grants.findForFile("read", key);
      if (grant) this.grants.used(grant);
      return grant !== undefined || this.config.rules.some((rule) => rule.path === key && this.ruleApplies(rule));
    };
    if (covered()) return { ok: true };
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
      covered()
        ? ({ allowed: true, scope: "rule", by: "rule" } as const)
        : this.elevate({ capability: "read", classifications: [classification], key: "tools", mode, tool: name, toolInput: rendered.slice(0, 2000) }, ctx),
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

  private parse(command: string): ParsedLine {
    return parseShell(command, this.home, { assignable: (name) => this.assignable(name), prefixAllowed: benignEnv });
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
    for (const command of commands) {
      if (command.loop && movesInLoop) here = undefined;
      const unwrapped = unwrap(command.words);
      const wrapped = unwrapped && unwrapped.wrappers.every((wrapper) => this.systemProgram(wrapper, here)) ? unwrapped : undefined;
      const words = wrapped?.words ?? command.words;
      out.push({
        ...command,
        words,
        env: [...command.env, ...(wrapped?.env ?? [])],
        wrappers: wrapped?.wrappers ?? [],
        cwd: here,
        inert: isInert(words) || this.harmlessFilter(words, here),
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

  /**
   * Why a command's redirections keep a rule from vouching for it: a file
   * that commands in the sandbox may not read or write without asking.
   */
  private redirectProblem(item: LineCommand): string | undefined {
    for (const [capability, paths] of [["read", item.reads], ["write", item.writes]] as const) {
      const verb = capability === "read" ? "reads" : "writes";
      for (const path of paths) {
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
  assess(command: string, cwd: string): LineAssessment {
    const parsed = this.parse(command);
    if (parsed.kind === "opaque") return { ...assessOpaque(command, this.riskFacts()), commands: [], parsed };
    const commands = this.commandsOf(parsed.commands, cwd);
    const facts = this.riskFacts();
    // An inert builtin or a filter needs no rule, but its redirections can still be dangerous (`echo x >> ~/.zshrc`).
    const assessments = commands.map((item): Assessment => {
      if (!item.inert) return assessCommand(item, facts);
      const redirected = redirectDanger(item, facts);
      return redirected ? { tier: "danger", kind: redirected, key: redirected } : { tier: "inspect" };
    });
    let worst: Assessment = { tier: "inspect" };
    for (const assessment of assessments) if (TIER_RANK[assessment.tier] > TIER_RANK[worst.tier]) worst = assessment;
    if (downloadsAndRuns(commands)) worst = { tier: "danger", kind: "downloads code and runs it", key: "download-run" };
    return { ...worst, commands, assessments, parsed };
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
    const lineDanger = line.key === "download-run";
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
   * The agent asks to run a bash call outside the sandbox. Who decides
   * depends on the line's risk tier (risk.ts):
   *
   * - dangerous: a human, every task: run once, or allow its patterns until
   *   the user's next message. A danger short of the most sensitive
   *   (risk.ts `reviewable`) goes to the reviewing model first, which may run
   *   it once when the user's own message asks for exactly it;
   * - local: the reviewing model, which remembers its approval for the chat;
   *   else a human: once, or always in this project by patterns for similar
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
    if (check.kind === "outside") {
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
      if (this.checkCommand(command, ctx.cwd, { escalated: true }).kind === "outside") {
        finish({ decision: "allow", decidedBy: "rule", prompted: false });
        return { ok: true, scope: "rule", by: "rule" } as const;
      }
      // Model first for what is not dangerous: it may run the command, and
      // remembers its approval for this chat. A danger short of the most
      // sensitive (risk.ts `reviewable`): it may run it once, when the user's
      // own message asks for exactly it. Otherwise the user decides, with its reason.
      const reviewable =
        dangerous &&
        !onceOnly &&
        line.parsed.kind !== "opaque" &&
        // Every word of it known before it runs: the model sees the command, not what `$(…)` makes of it.
        line.assessments!.every((assessment, index) => assessment.tier !== "danger" || (assessment.reviewable === true && !line.commands[index].words.includes(UNKNOWN)));
      let review: string | undefined;
      if (mode === "assisted" && (!dangerous || reviewable)) {
        const purpose = [proposal.label && `Purpose: ${proposal.label}`, proposal.reason && `Reason: ${proposal.reason}`].filter(Boolean).join("\n");
        // An approval is remembered for the commands' own categories, never for patterns the agent proposed; a danger's, never.
        const memo = dangerous ? [] : patterns.filter((pattern) => !pattern.proposed);
        const verdict = await this.review(
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
          },
          ctx,
          dangerous ? Math.max(this.config.judge.minConfidence, DANGER_CONFIDENCE) : undefined,
        );
        audit.judge = verdict.state;
        if (verdict.decision === "approve") {
          for (const pattern of memo) this.addCommandGrant(pattern, "chat", "judge", label);
          finish({ decision: "allow", decidedBy: "judge", reason: verdict.reason, scope: memo.length ? "chat" : "once", prompted: false });
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
      if (dangerous && line.kind) lines.push(fact("Note", `Dangerous: it ${homeRelative(line.kind, this.home)}.${reviewable ? "" : " This always asks."}`));
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
      const choice = await ctx.ui.select(title, options, { signal: ctx.signal, timeout: this.config.ask.timeoutMs ?? undefined });
      const asked = { prompted: true, choice: choice ?? "dismissed", decidedBy: "user" as const };
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
          finish({ decision: "abort", ...asked });
          ctx.abort?.();
          return { ok: false, reason: "The user denied this and stopped the turn.", abort: true } as const;
        default: {
          const reason = choice === HUMAN.deny ? "The user denied running this outside the sandbox." : "No approval was given (the request was dismissed or timed out).";
          finish({ ...asked, reason });
          return { ok: false, reason: `${reason} Continue in the sandbox or ask the user.` } as const;
        }
      }
    });
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
      git: (dir) => this.gitAt(dir),
      inWorkspace: (dir) => this.paths.inWorkspace(this.paths.canonical(dir, this.cwd)),
      kubeContext: (args) => this.kubeContext(args),
      programIsLocal: (words) => {
        const resolved = this.resolveIn(words[0] ?? "", this.cwd);
        return resolved !== undefined && this.writableRoots().some((root) => isUnder(resolved, root));
      },
      projectTool: (name) =>
        name !== UNKNOWN &&
        /^[@\w./-]+$/.test(name) &&
        [this.cwd, ...this.roots].some((root) => [join(root, "node_modules", ".bin", basename(name)), join(root, ".venv", "bin", basename(name))].some((path) => existsSync(path))),
    };
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

  statusText(): string {
    const lines: string[] = [];
    if (!this.status.available) {
      lines.push(`Permission control is OFF on this device: ${this.status.error ?? "the OS sandbox does not work"}.`);
      return lines.join("\n");
    }
    const levelName = { "read-only": "Ask before edits", standard: "Edit workspace", "full-access": "Full access (no restrictions)" }[this.level];
    lines.push(`Level: ${levelName} (change it with /permissions)`);
    lines.push(`Sandbox: Landstrip (${this.status.implementation ?? "available"})`);
    lines.push(`Project: ${this.project}`);
    const paused = this.judgePaused();
    if (paused) lines.push(paused);
    lines.push("", "Workspace roots:", ...this.roots.map((root) => `  ${root}`));
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
    const key = JSON.stringify([this.roots, this.config.sensitive, this.config.protected, this.config.self]);
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
    const allSensitive = dropExceptions(carve([...sensitiveRead, ...anywhere, ...deferredHits.flat()]), sensitive.exceptions, roots, this.home);
    const allSensitiveWrite = dropExceptions(carve([...sensitiveWrite, ...anywhere, ...deferredHits.flat()]), sensitive.exceptions, roots, this.home);

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
      selfExceptions: self.exceptions,
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
        "Writes outside the workspace, protected files (.git/config, git hooks, shell startup files), Cypher's own data and sensitive files (.env, keys, ~/.ssh and other credentials) are blocked.",
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
    if (judgeNote) lines.push(fact("Review", judgeNote.startsWith("Automatic review is paused") ? judgeNote : `The reviewing model couldn't approve this automatically. ${judgeNote}`));
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
          : `${request.capability} outside the workspace`;
  if (dangerous) lines.push("Note: Dangerous: a write here runs code later. This always asks.");
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
