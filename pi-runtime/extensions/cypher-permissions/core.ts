/**
 * The permission engine for one Pi process: configuration, zones, grants, the
 * approval pipeline (policy → grant → judge → human) and the per-call checks
 * the file tools run at the filesystem boundary. Nothing here imports Pi
 * values, so the whole pipeline runs under `node --test`.
 */

import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { constants, realpathSync, statSync } from "node:fs";
import { open, mkdir, access as fsAccess, stat as fsStat, readdir as fsReaddir } from "node:fs/promises";
import { homedir } from "node:os";
import { join, resolve as resolvePath } from "node:path";
import { AuditLog } from "./audit.ts";
import type { AuditRecord } from "./audit.ts";
import { appendCommandRule, appendRule, approvalMode as approvalModeFor, loadConfig, resolveLevel } from "./config.ts";
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
import { describePattern, fileSha256, INERT_PROGRAMS, patternError, patternMatches, resolveProgram } from "./commands.ts";
import type { CommandRule } from "./commands.ts";
import { parseShell } from "./shell.ts";

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
  /** One completion from the judge model; undefined on any failure. */
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

const ESCALATE_HINT =
  "If the command needs what the sandbox withholds (files outside the workspace, credentials, keys, the keychain, other processes, system services), run it again with `escalate`: the user decides whether it runs outside the sandbox.";

/** What command rules say about one bash call. */
export type CommandCheck =
  /** A `deny` rule matches: the call never runs. */
  | { kind: "deny"; reason: string }
  /** Every command in it is allowed outside the sandbox. */
  | { kind: "outside"; rules: CommandRule[] }
  /** It runs in the sandbox unless an escalation is approved. */
  | { kind: "sandbox" };

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
}

function gitRoots(cwd: string): { top?: string; gitDirs: string[] } {
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
    return {
      top: resolveDir(top),
      gitDirs: [...new Set([resolveDir(gitDir), resolveDir(common)].filter((dir): dir is string => dir !== undefined))],
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
  private readonly levelEnv: string | undefined;
  private cwd: string;
  private extraRoots: string[] = [];
  private baseRoots: string[] = [];
  private gitDirs: string[] = [];
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
    this.baseRoots = [...new Set([canonicalCwd, ...(git.top ? [git.top] : [])])];
    this.extraRoots = this.rootsFromEntries(entries);
    const roots = [...new Set([...this.baseRoots, ...this.extraRoots])];
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
      home: this.home,
    });
    this.grants.restore(entries);
    this.sensitiveCache = undefined;
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
    return this.config.rules.some((rule) => (capability === "read" || rule.capability === "write") && isUnder(path, rule.path));
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

  /**
   * Authorize one tool call's accesses before it runs, asking when needed.
   * The returned approvals cover exactly the canonical paths requested.
   */
  async authorize(requests: readonly AccessRequest[], call: CallInfo, ctx: RequestContext): Promise<AuthResult> {
    const approved = new Approved();
    if (!this.enforcing) return { ok: true, approved };
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
      const outcome = await this.serialized(() =>
        this.elevate(
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
        ),
      );
      if (!outcome.allowed) return { ok: false, reason: outcome.reason, abort: outcome.abort };
      approved.add(request.capability, canonical);
      used(outcome.scope ?? "once");
      if (outcome.scope && outcome.scope !== "once") this.persistGrant(request.capability, [classification], outcome.scope, outcome.by);
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
    const sensitive = classifications.some((item) => item.zone === "sensitive");
    const scope = outcome.scope ?? "once";
    if (scope === "always" && !sensitive) {
      this.persistGrant(input.capability, classifications, "always", outcome.by);
    } else {
      const grant = newGrant({ capability: input.capability, paths: classifications.map((item) => item.path), sensitive: sensitive || undefined, scope, approvedBy: outcome.by, chatId: this.chatId });
      this.grants.add(grant);
      if (scope === "chat") this.appendEntry?.(GRANT_ENTRY, grant);
    }
    return {
      ok: true,
      record: { outcome: "allowed", scope },
      text: `Approved (${scope}) for the file tools (read, write, edit, ls, grep, find): ${classifications.map((item) => item.path).join(", ")}. Run the file tool now. Commands never get this: a command that needs it (ssh, git, a CLI reading credentials) runs again with \`escalate\`.`,
    };
  }

  /** Set by the extension: persist a custom session entry. */
  appendEntry?: (type: string, data: unknown) => void;

  private persistGrant(capability: Capability, classifications: Classification[], scope: GrantScope, by: Grant["approvedBy"]): void {
    const sensitive = classifications.some((item) => item.zone === "sensitive");
    const paths = classifications.map((item) => item.path);
    if (scope === "always") {
      if (sensitive) return;
      for (const path of paths) appendRule(this.agentDir, { capability, path, createdAt: new Date().toISOString() });
      this.config.rules.push(...paths.map((path) => ({ capability, path, createdAt: new Date().toISOString() })));
      return;
    }
    const grant = newGrant({ capability, paths, sensitive: sensitive || undefined, scope, approvedBy: by, chatId: this.chatId });
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
  ): Promise<{ allowed: true; scope?: GrantScope; by: Grant["approvedBy"] } | { allowed: false; reason: string; abort?: boolean }> {
    const paths = request.classifications.map((item) => item.path);
    const sensitive = request.classifications.some((item) => item.zone === "sensitive");
    const base: AuditRecord = {
      decision: "deny",
      decidedBy: "user",
      tool: request.tool,
      command: request.command,
      capability: request.capability,
      zone: request.classifications[0]?.zone,
      guard: request.classifications[0]?.guard,
      paths,
    };
    // Model first: the judge either approves automatically or puts the
    // question to the user with its reason. It never rejects, and a judge
    // that cannot run (no model, an error, a timeout) also asks the user.
    let judgeNote: string | undefined;
    if (request.mode === "assisted" && !sensitive && !ctx.complete) {
      judgeNote = "No reviewing model is available here.";
    }
    if (request.mode === "assisted" && !sensitive && ctx.complete) {
      // A domain request reads as "network" to the judge, with the domain as its path.
      const { userMessages, recentToolCalls } = conversationContext(ctx.branch());
      const first = request.classifications[0];
      const judgeRequest: JudgeRequest = {
        capability: request.domain !== undefined ? "network" : request.capability,
        paths: request.domain !== undefined ? [request.domain] : paths,
        zone: first?.zone ?? "external",
        guard: first?.guard,
        guardReason: first?.guard ? `matches ${first.match}` : undefined,
        command: request.command,
        tool: request.tool,
        level: this.level,
        workspaceRoots: this.roots,
        diff: request.diff,
        justification: request.justification,
        userMessages,
        recentToolCalls,
      };
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), this.config.judge.timeoutMs);
      const onAbort = () => controller.abort();
      ctx.signal?.addEventListener("abort", onAbort, { once: true });
      let text: string | undefined;
      try {
        text = await ctx.complete(JUDGE_SYSTEM_PROMPT, judgePrompt(judgeRequest), controller.signal);
      } catch {
        text = undefined;
      } finally {
        clearTimeout(timer);
        ctx.signal?.removeEventListener("abort", onAbort);
      }
      const verdict =
        text === undefined
          ? { decision: "ask-user" as const, reason: "The reviewing model did not answer (it failed or timed out).", confidence: 0 }
          : parseVerdict(text, this.config.judge.minConfidence);
      if (verdict.decision === "approve") {
        this.audit.write({ ...base, decision: "allow", decidedBy: "judge", reason: verdict.reason, scope: "once" });
        return { allowed: true, scope: "once", by: "judge" };
      }
      judgeNote = verdict.reason;
    }
    if (judgeNote) base.reviewer = judgeNote;
    if (!ctx.hasUI) {
      const reason = `Permission denied: ${describeAll(request.classifications)} needs approval and no one is here to approve it.`;
      this.audit.write({ ...base, decidedBy: "policy", reason });
      return { allowed: false, reason };
    }
    // Once for a domain lasts until the command that asked ends.
    const allowAlways = !sensitive && !request.classifications.some((item) => item.guard === "self");
    const options = [
      request.domain !== undefined ? DOMAIN_ONCE : HUMAN.once,
      HUMAN.chat,
      ...(allowAlways ? [HUMAN.always] : []),
      HUMAN.deny,
      HUMAN.stop,
    ];
    const kind = request.domain !== undefined ? "network" : request.toolInput !== undefined ? "tool" : "access";
    const title = approvalTitle(request, judgeNote, this.home) + promptLine(kind, ctx.toolCallId);
    const choice = await ctx.ui.select(title, options, {
      signal: ctx.signal,
      timeout: this.config.ask.timeoutMs ?? undefined,
    });
    switch (choice) {
      case HUMAN.once:
      case DOMAIN_ONCE:
        this.audit.write({ ...base, decision: "allow", scope: "once" });
        return { allowed: true, scope: "once", by: "user" };
      case HUMAN.chat:
        this.audit.write({ ...base, decision: "allow", scope: "chat" });
        return { allowed: true, scope: "chat", by: "user" };
      case HUMAN.always:
        this.audit.write({ ...base, decision: "allow", scope: "always" });
        return { allowed: true, scope: "always", by: "user" };
      case HUMAN.stop:
        this.audit.write({ ...base, decision: "abort" });
        ctx.abort?.();
        return { allowed: false, reason: "The user denied this and stopped the turn.", abort: true };
      default: {
        const reason = choice === HUMAN.deny ? "The user denied this request." : "No approval was given (the request was dismissed or timed out).";
        this.audit.write({ ...base, reason });
        return { allowed: false, reason: `${reason} Do not retry the same access; continue without it or ask the user.` };
      }
    }
  }

  /** A tool-level approval for an MCP or extension tool whose setting is `elevate`. */
  async requestToolUse(name: string, input: unknown, ctx: RequestContext): Promise<{ ok: true } | { ok: false; reason: string; abort?: boolean }> {
    const key = `tool:${name}`;
    const grant = this.grants.findForFile("read", key);
    if (grant) {
      this.grants.used(grant);
      return { ok: true };
    }
    if (this.config.rules.some((rule) => rule.path === key)) return { ok: true };
    const mode = approvalModeFor(this.config, "tools");
    if (mode === "deny") return { ok: false, reason: `Permission denied: the ${name} tool needs approval, which is set to always deny.` };
    let rendered = "";
    try {
      rendered = JSON.stringify(input, null, 2) ?? "";
    } catch {
      rendered = String(input);
    }
    const classification: Classification = { path: key, zone: "external", match: "tool" };
    const outcome = await this.serialized(() =>
      this.elevate({ capability: "read", classifications: [classification], key: "tools", mode, tool: name, toolInput: rendered.slice(0, 2000) }, ctx),
    );
    if (!outcome.allowed) return { ok: false, reason: outcome.reason, abort: outcome.abort };
    if (outcome.scope === "always") {
      appendRule(this.agentDir, { capability: "read", path: key, createdAt: new Date().toISOString() });
      this.config.rules.push({ capability: "read", path: key, createdAt: new Date().toISOString() });
    } else if (outcome.scope === "chat") {
      const chatGrant = newGrant({ capability: "read", paths: [key], scope: "chat", approvedBy: outcome.by, chatId: this.chatId });
      this.grants.add(chatGrant);
      this.appendEntry?.(GRANT_ENTRY, chatGrant);
    }
    return { ok: true };
  }

  // --- Running outside the sandbox ------------------------------------------------

  /** Every command rule in effect: the user's and project's, then this chat's. */
  commandRules(): CommandRule[] {
    return [...this.config.commands, ...this.grants.commandRules()];
  }

  /** Places a sandboxed command may write: a program there could be the agent's. */
  private writableRoots(): string[] {
    return [...this.roots, ...this.writableSupportDirs(), ...tempDirs()];
  }

  /**
   * Whether a rule may run this program outside the sandbox: the program
   * exists, and one in a writable place still has the contents the rule
   * pinned.
   */
  private programTrusted(program: string, cwd: string, rule: CommandRule): boolean {
    const resolved = resolveProgram(program, cwd);
    if (!resolved) return false;
    if (!this.writableRoots().some((root) => isUnder(resolved, root))) return true;
    return rule.sha256 !== undefined && fileSha256(resolved) === rule.sha256;
  }

  /** The simple commands of a line that need the sandbox's leave, with the directory each runs in. */
  private commandsOf(words: string[][], cwd: string): { words: string[]; cwd: string }[] {
    const out: { words: string[]; cwd: string }[] = [];
    let here = cwd;
    for (const command of words) {
      if (INERT_PROGRAMS.has(command[0])) {
        if (command[0] === "cd" && command.length === 2) here = resolvePath(here, command[1]);
        continue;
      }
      out.push({ words: command, cwd: here });
    }
    return out;
  }

  /** What the rules say about a bash call, before it runs. */
  checkCommand(command: string, cwd: string): CommandCheck {
    if (!this.enforcing) return { kind: "sandbox" };
    const parsed = parseShell(command, this.home);
    // A line the parser cannot read cannot match a rule: it runs in the
    // sandbox unless it is approved as written.
    if (parsed.kind === "opaque") return { kind: "sandbox" };
    const rules = this.commandRules();
    const commands = this.commandsOf(parsed.commands, cwd);
    const used: CommandRule[] = [];
    let outside = commands.length > 0;
    for (const item of commands) {
      const matching = rules.filter((rule) => patternMatches(rule.match, item.words));
      const deny = matching.find((rule) => rule.decision === "deny");
      if (deny) return { kind: "deny", reason: `Permission denied: \`${describePattern(deny.match)}\` is set to never run.` };
      if (matching.some((rule) => rule.decision === "ask")) {
        outside = false;
        continue;
      }
      const allow = matching.find((rule) => rule.decision === "allow" && this.programTrusted(item.words[0], item.cwd, rule));
      if (allow) used.push(allow);
      else outside = false;
    }
    return outside ? { kind: "outside", rules: used } : { kind: "sandbox" };
  }

  /**
   * The agent asks to run a bash call outside the sandbox, proposing
   * patterns for later calls. A human decides: once, for this chat or always
   * (the patterns), or no.
   */
  async requestEscalation(
    command: string,
    proposal: EscalationProposal,
    ctx: RequestContext,
  ): Promise<{ ok: true; scope: GrantScope } | { ok: false; reason: string; abort?: boolean }> {
    const mode = approvalModeFor(this.config, "escalate");
    const audit: AuditRecord = { tool: "bash", command, capability: "escalate", decision: "deny", decidedBy: "policy" };
    if (mode === "deny") {
      const reason = "Permission denied: running commands outside the sandbox is set to always deny.";
      this.audit.write({ ...audit, reason });
      return { ok: false, reason };
    }
    const check = this.checkCommand(command, ctx.cwd);
    if (check.kind === "deny") return { ok: false, reason: check.reason };
    const parsed = parseShell(command, this.home);
    // Patterns, each with the pin for a program in a writable place.
    const patterns: { match: string[]; sha256?: string }[] = [];
    if (parsed.kind === "simple" && proposal.patterns?.length) {
      const commands = this.commandsOf(parsed.commands, ctx.cwd);
      for (const pattern of proposal.patterns) {
        const error = patternError(pattern);
        if (error) return { ok: false, reason: `Invalid pattern ${JSON.stringify(pattern)}: ${error}` };
        const matched = commands.find((item) => patternMatches(pattern, item.words));
        if (!matched) return { ok: false, reason: `The pattern \`${describePattern(pattern)}\` does not match this command; propose patterns that cover it.` };
        const resolved = resolveProgram(matched.words[0], matched.cwd);
        if (!resolved) return { ok: false, reason: `\`${matched.words[0]}\` was not found.` };
        const pin = this.writableRoots().some((root) => isUnder(resolved, root)) ? fileSha256(resolved) : undefined;
        patterns.push({ match: pattern, sha256: pin });
      }
      const uncovered = commands.find((item) => !proposal.patterns!.some((pattern) => patternMatches(pattern, item.words)));
      if (uncovered) return { ok: false, reason: `No proposed pattern covers \`${uncovered.words.join(" ")}\`; propose one for every command in the line, or none to run it once.` };
    }
    if (!ctx.hasUI) {
      const reason = "Permission denied: running outside the sandbox needs approval and no one is here to approve it.";
      this.audit.write({ ...audit, reason });
      return { ok: false, reason };
    }
    const lines: string[] = [];
    if (proposal.label) lines.push(fact("Purpose", proposal.label));
    lines.push(fact("Command", command));
    for (const pattern of patterns) lines.push(fact("Pattern", describePattern(pattern.match)));
    if (proposal.reason) lines.push(fact("Reason", proposal.reason));
    lines.push("Note: It runs as you, with your files, keys, keychain and network.");
    if (patterns.some((pattern) => pattern.sha256)) lines.push("Note: A program here is in a writable place; approval covers its current contents only.");
    if (parsed.kind === "opaque") lines.push(fact("Note", `It uses ${parsed.reason}, which rules can't check, so it can only run once as written.`));
    const title = `Run this command outside the sandbox?\n\nContext:\n${lines.join("\n")}${promptLine("escalate", ctx.toolCallId)}`;
    const options = patterns.length ? [RUN_ONCE, HUMAN.chat, HUMAN.always, HUMAN.deny, HUMAN.stop] : [RUN_ONCE, HUMAN.deny, HUMAN.stop];
    const choice = await this.serialized(() => ctx.ui.select(title, options, { signal: ctx.signal, timeout: this.config.ask.timeoutMs ?? undefined }));
    const label = proposal.label?.trim() || undefined;
    switch (choice) {
      case RUN_ONCE:
        this.audit.write({ ...audit, decision: "allow", decidedBy: "user", scope: "once" });
        return { ok: true, scope: "once" };
      case HUMAN.chat:
        for (const pattern of patterns) {
          const grant = newGrant({ capability: "read", paths: [], pattern: pattern.match, label, sha256: pattern.sha256, scope: "chat", approvedBy: "user", chatId: this.chatId });
          this.grants.add(grant);
          this.appendEntry?.(GRANT_ENTRY, grant);
        }
        this.audit.write({ ...audit, decision: "allow", decidedBy: "user", scope: "chat" });
        return { ok: true, scope: "chat" };
      case HUMAN.always:
        for (const pattern of patterns) {
          const rule: CommandRule = { match: pattern.match, decision: "allow", label, sha256: pattern.sha256, createdAt: new Date().toISOString() };
          appendCommandRule(this.agentDir, rule);
          this.config.commands.push(rule);
        }
        this.audit.write({ ...audit, decision: "allow", decidedBy: "user", scope: "always" });
        return { ok: true, scope: "always" };
      case HUMAN.stop:
        this.audit.write({ ...audit, decision: "abort", decidedBy: "user" });
        ctx.abort?.();
        return { ok: false, reason: "The user denied this and stopped the turn.", abort: true };
      default: {
        const reason = choice === HUMAN.deny ? "The user denied running this outside the sandbox." : "No approval was given (the request was dismissed or timed out).";
        this.audit.write({ ...audit, decidedBy: "user", reason });
        return { ok: false, reason: `${reason} Continue in the sandbox or ask the user.` };
      }
    }
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
      for (const name of ["config", "config.worktree", "hooks"]) protectedPaths.push(join(gitDir, name));
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
        ...this.config.rules.map((rule) =>
          newGrant({ capability: rule.capability, paths: [rule.path], scope: "always", approvedBy: "rule" }),
        ),
      ],
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
    if (this.config.rules.some((rule) => rule.path === key)) return true;
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
    if (outcome.scope === "always") {
      appendRule(this.agentDir, { capability: "read", path: key, createdAt: new Date().toISOString() });
      this.config.rules.push({ capability: "read", path: key, createdAt: new Date().toISOString() });
    } else if (outcome.scope === "chat") {
      const chatGrant = newGrant({ capability: "read", paths: [key], scope: "chat", approvedBy: outcome.by, chatId: this.chatId });
      this.grants.add(chatGrant);
      this.appendEntry?.(GRANT_ENTRY, chatGrant);
    } else {
      running.domains.add(name);
    }
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
): string {
  const lines: string[] = [];
  const review = () => {
    if (judgeNote) lines.push(fact("Review", `The reviewing model couldn't approve this automatically. ${judgeNote}`));
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
