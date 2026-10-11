/**
 * Permission control: zones (workspace, external, sensitive), an OS sandbox
 * for AI shell commands (Landstrip), and one approval pipeline for anything
 * outside the defaults. Design: docs/design/permission-control.md.
 *
 * The built-in file tools are re-registered under their own names, built
 * from Pi's factories with filesystem operations that check every path they
 * actually touch; `bash` and the ripgrep behind `grep` run inside Landstrip
 * with a policy compiled per launch. The chat's level comes from Cypher's
 * per-chat sandbox setting (`CYPHER_PERMISSION_LEVEL`).
 *
 * On a host where Landstrip cannot sandbox (Linux without Landlock), the
 * extension stands down: tools run as they would without it, and the chat is
 * told once.
 */

import { constants, existsSync, statSync } from "node:fs";
import * as nodeFs from "node:fs/promises";
import { relative } from "node:path";
import {
  createBashToolDefinition,
  createEditToolDefinition,
  createFindToolDefinition,
  createGrepToolDefinition,
  createLsToolDefinition,
  createReadToolDefinition,
  createWriteToolDefinition,
  getAgentDir,
} from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import {
  Approved,
  checkedAccess,
  checkedImageType,
  checkedMkdir,
  checkedReaddir,
  checkedReadFile,
  checkedWriteFile,
  patchPaths,
  PermissionDenied,
  Permissions,
  toolOverride,
} from "./cypher-permissions/core.ts";
import type { AccessRequest, RequestContext } from "./cypher-permissions/core.ts";
import { DENIAL_TEXT, parseDenials, scrubEnv, wrapCommand, writePolicyFile } from "./cypher-permissions/sandbox.ts";
import { isUnder } from "./cypher-permissions/paths.ts";
import { liveApprovalsSupported, trappedExec } from "./cypher-permissions/live.ts";

const FS_HOOK = Symbol.for("cypher.permissions.fs");
const APPLY_PATCH = "apply_patch";
const OWN_TOOLS = new Set(["read", "write", "edit", "ls", "grep", "find", "bash", "request_permission"]);

type AnyResult = { content: { type: string; text?: string }[]; details?: unknown; isError?: boolean; structuredContent?: unknown };

export default function cypherPermissions(pi: ExtensionAPI): void {
  let permissions: Permissions | undefined;

  const state = (ctx: { cwd: string; sessionManager?: ExtensionContext["sessionManager"] }): Permissions => {
    if (!permissions) {
      permissions = new Permissions({
        agentDir: getAgentDir(),
        chatId: process.env.CYPHER_CHAT_ID || undefined,
        cwd: ctx.cwd,
        levelEnv: process.env.CYPHER_PERMISSION_LEVEL,
      });
      permissions.appendEntry = (type, data) => pi.appendEntry(type, data);
      permissions.reload(ctx.cwd, ctx.sessionManager?.getBranch() ?? []);
    }
    return permissions;
  };

  const requestContext = (ctx: ExtensionContext, signal: AbortSignal | undefined): RequestContext => ({
    cwd: ctx.cwd,
    hasUI: ctx.hasUI,
    ui: ctx.ui,
    signal: signal ?? ctx.signal,
    branch: () => ctx.sessionManager.getBranch(),
    abort: () => ctx.abort(),
    complete: async (system, prompt, completionSignal) => {
      const configured = permissions?.config.judge.model;
      let model = ctx.model;
      if (configured) {
        const slash = configured.indexOf("/");
        if (slash > 0) model = ctx.modelRegistry.find(configured.slice(0, slash), configured.slice(slash + 1)) ?? model;
      }
      if (!model) return undefined;
      try {
        const stream = ctx.modelRegistry.stream(
          model,
          { systemPrompt: system, messages: [{ role: "user", content: prompt, timestamp: Date.now() }] },
          { signal: completionSignal },
        );
        for await (const _event of stream) {
          // Drained for the result.
        }
        const result = await stream.result();
        if (result.stopReason === "error" || result.stopReason === "aborted") return undefined;
        return result.content
          .filter((part: { type: string }) => part.type === "text")
          .map((part: { type: string; text?: string }) => part.text ?? "")
          .join("");
      } catch {
        return undefined;
      }
    },
  });

  const shellSettings = () => {
    const settings = pi.getSettings() as { shellPath?: string; shellCommandPrefix?: string };
    const shell = settings.shellPath && existsSync(settings.shellPath) ? settings.shellPath : existsSync("/bin/bash") ? "/bin/bash" : "bash";
    return { shellPath: settings.shellPath, commandPrefix: settings.shellCommandPrefix, shell };
  };

  const fail = (message: string): never => {
    throw new Error(message);
  };

  /** Authorize, then run the real tool with checked operations. */
  const guarded = async <T>(
    ctx: ExtensionContext,
    signal: AbortSignal | undefined,
    tool: string,
    toolCallId: string,
    requests: AccessRequest[],
    run: (approved: Approved | undefined) => Promise<T>,
  ): Promise<T> => {
    const st = state(ctx);
    if (!st.enforcing) return run(undefined);
    const auth = await st.authorize(requests, { tool, toolCallId }, requestContext(ctx, signal));
    if (!auth.ok) return fail(auth.reason);
    try {
      return await run(auth.approved);
    } catch (error) {
      if (error instanceof PermissionDenied) return fail(error.message);
      throw error;
    }
  };

  // --- File tools --------------------------------------------------------------

  const baseRead = createReadToolDefinition(process.cwd());
  pi.registerTool({
    ...baseRead,
    async execute(id, params, signal, onUpdate, ctx) {
      return guarded(ctx, signal, "read", id, [{ capability: "read", path: params.path }], (approved) => {
        const st = state(ctx);
        const tool = approved
          ? createReadToolDefinition(ctx.cwd, {
              operations: {
                readFile: (path) => checkedReadFile(st, path, approved),
                access: (path) => checkedAccess(st, "read", path, constants.F_OK, approved),
                detectImageMimeType: (path) => checkedImageType(st, path, approved),
              },
            })
          : createReadToolDefinition(ctx.cwd);
        return tool.execute(id, params, signal, onUpdate, ctx);
      });
    },
  });

  const baseWrite = createWriteToolDefinition(process.cwd());
  pi.registerTool({
    ...baseWrite,
    async execute(id, params, signal, onUpdate, ctx) {
      const diff = typeof params.content === "string" ? `+++ ${params.path} (whole file)\n${params.content.slice(0, 4000)}` : undefined;
      return guarded(ctx, signal, "write", id, [{ capability: "write", path: params.path, diff }], (approved) => {
        const st = state(ctx);
        const tool = approved
          ? createWriteToolDefinition(ctx.cwd, {
              operations: {
                writeFile: (path, content) => checkedWriteFile(st, path, content, approved),
                mkdir: (dir) => checkedMkdir(st, dir, approved),
              },
            })
          : createWriteToolDefinition(ctx.cwd);
        return tool.execute(id, params, signal, onUpdate, ctx);
      });
    },
  });

  const baseEdit = createEditToolDefinition(process.cwd());
  pi.registerTool({
    ...baseEdit,
    async execute(id, params, signal, onUpdate, ctx) {
      const edits = Array.isArray((params as { edits?: unknown }).edits) ? (params as { edits: { oldText: string; newText: string }[] }).edits : [];
      const diff = edits
        .map((edit) => `${edit.oldText.split("\n").map((line) => `- ${line}`).join("\n")}\n${edit.newText.split("\n").map((line) => `+ ${line}`).join("\n")}`)
        .join("\n…\n");
      return guarded(ctx, signal, "edit", id, [{ capability: "write", path: params.path, diff }], (approved) => {
        const st = state(ctx);
        const tool = approved
          ? createEditToolDefinition(ctx.cwd, {
              operations: {
                readFile: (path) => checkedReadFile(st, path, approved),
                writeFile: (path, content) => checkedWriteFile(st, path, content, approved),
                access: (path) => checkedAccess(st, "write", path, constants.R_OK | constants.W_OK, approved),
              },
            })
          : createEditToolDefinition(ctx.cwd);
        return tool.execute(id, params, signal, onUpdate, ctx);
      });
    },
  });

  const baseLs = createLsToolDefinition(process.cwd());
  pi.registerTool({
    ...baseLs,
    async execute(id, params, signal, onUpdate, ctx) {
      const path = (params as { path?: string }).path || ".";
      return guarded(ctx, signal, "ls", id, [{ capability: "read", path }], (approved) => {
        const st = state(ctx);
        const tool = approved
          ? createLsToolDefinition(ctx.cwd, {
              operations: {
                exists: (target) => existsSync(target),
                stat: (target) => nodeFs.stat(target),
                readdir: (target) => checkedReaddir(st, target, approved),
              },
            })
          : createLsToolDefinition(ctx.cwd);
        return tool.execute(id, params, signal, onUpdate, ctx);
      });
    },
  });

  const baseFind = createFindToolDefinition(process.cwd());
  pi.registerTool({
    ...baseFind,
    async execute(id, params, signal, onUpdate, ctx) {
      // File names are not secrets: the search root is checked, fd runs as is.
      const path = (params as { path?: string }).path || ".";
      return guarded(ctx, signal, "find", id, [{ capability: "read", path }], () =>
        createFindToolDefinition(ctx.cwd).execute(id, params, signal, onUpdate, ctx),
      );
    },
  });

  const baseGrep = createGrepToolDefinition(process.cwd());
  pi.registerTool({
    ...baseGrep,
    async execute(id, params, signal, onUpdate, ctx) {
      const searchPath = (params as { path?: string }).path || ".";
      return guarded(ctx, signal, "grep", id, [{ capability: "read", path: searchPath }], async (approved) => {
        const st = state(ctx);
        if (!approved) return createGrepToolDefinition(ctx.cwd).execute(id, params, signal, onUpdate, ctx);
        const compiled = await st.compile({ readOnly: true, signal });
        const root = st.paths.canonical(searchPath, ctx.cwd);
        // Keep ripgrep away from denied files (it would exit 2 on the first
        // one); Landstrip enforces the same list underneath.
        const exclusions = compiled.policy.filesystem.denyRead
          .filter((path) => isUnder(path, root) && path !== root)
          .flatMap((path) => {
            const rel = relative(root, path).replace(/([[\]*?{}!\\])/g, "\\$1");
            return ["--glob", `!/${rel}`, "--glob", `!/${rel}/**`];
          });
        const policy = writePolicyFile(compiled.policy);
        const binary = st.status.binary!;
        try {
          const tool = createGrepToolDefinition(ctx.cwd, {
            operations: {
              isDirectory: async (target: string) => (await nodeFs.stat(target)).isDirectory(),
              readFile: async (target: string) => (await checkedReadFile(st, target, approved)).toString("utf8"),
            },
            spawnHook: ({ command, args }: { command: string; args: string[] }) => ({
              command: binary,
              args: ["run", "-p", policy.path, "--", command, ...exclusions, ...args],
            }),
          } as Parameters<typeof createGrepToolDefinition>[1]);
          const result = (await tool.execute(id, params, signal, onUpdate, ctx)) as AnyResult;
          return dropSensitiveLines(result, st, root, approved) as never;
        } finally {
          policy.dispose();
        }
      });
    },
  });

  /** Defense in depth: drop result lines from files the agent may not read. */
  function dropSensitiveLines(result: AnyResult, st: Permissions, root: string, approved: Approved): AnyResult {
    const isFile = !isDirectory(root);
    const cache = new Map<string, boolean>();
    const allowed = (rel: string) => {
      if (cache.has(rel)) return cache.get(rel)!;
      const target = isFile ? root : st.paths.canonical(rel, root);
      let ok = true;
      try {
        st.assert("read", target, approved);
      } catch {
        ok = false;
      }
      cache.set(rel, ok);
      return ok;
    };
    let dropped = 0;
    const content = result.content.map((part) => {
      if (part.type !== "text" || typeof part.text !== "string") return part;
      const lines = part.text.split("\n").filter((line) => {
        const match = line.match(/^(.+?)[:-](\d+)[:-] /);
        if (!match) return true;
        if (allowed(match[1])) return true;
        dropped++;
        return false;
      });
      if (dropped) lines.push(`[permissions] ${dropped} result line(s) from files the agent may not read were omitted.`);
      return { ...part, text: lines.join("\n") };
    });
    return { ...result, content };
  }

  // --- Bash ---------------------------------------------------------------------

  const baseBash = createBashToolDefinition(process.cwd());
  pi.registerTool({
    ...baseBash,
    async execute(id, params, signal, onUpdate, ctx) {
      const st = state(ctx);
      const { shellPath, commandPrefix, shell } = shellSettings();
      if (!st.enforcing) {
        return createBashToolDefinition(ctx.cwd, { shellPath, commandPrefix }).execute(id, params, signal, onUpdate, ctx);
      }
      const command = String(params.command ?? "");
      // Filtered network: the command reaches the network only through the
      // proxy, which asks about new domains in this call's context.
      const proxyPort = await st.proxyPort();
      const compiled = await st.compile({ command, signal, proxyPort });
      const viaProxy = compiled.policy.network.httpProxyPort;
      const policy = writePolicyFile(compiled.policy);
      const binary = st.status.binary!;
      st.beginCommand(id, command, requestContext(ctx, signal), compiled);
      const env = (base: NodeJS.ProcessEnv) => ({
        ...scrubEnv(base, st.config.envScrub, compiled.envAllowed),
        ...(viaProxy !== undefined ? st.proxyEnv(viaProxy) : {}),
      });
      // Linux: Landstrip holds a denied syscall and asks (live.ts), so an
      // approval lets the command go on. macOS: request, then retry.
      const live = liveApprovalsSupported();
      const tool = createBashToolDefinition(ctx.cwd, {
        shellPath,
        commandPrefix,
        ...(live
          ? {
              operations: { exec: trappedExec(binary, policy.path, shell, (query) => st.answerTrap(id, query)) },
              spawnHook: (context) => ({ ...context, env: env(context.env) }),
            }
          : {
              spawnHook: (context) => ({
                command: wrapCommand(binary, policy.path, shell, context.command),
                cwd: context.cwd,
                env: env(context.env),
              }),
            }),
      });
      const forward = onUpdate
        ? (update: Parameters<NonNullable<typeof onUpdate>>[0]) =>
            onUpdate({
              ...update,
              content: update.content.map((part) => (part.type === "text" ? { ...part, text: parseDenials(part.text).rest } : part)),
            })
        : undefined;
      try {
        const result = (await tool.execute(id, params, signal, forward, ctx)) as AnyResult;
        return annotateBash(result, st, compiled) as never;
      } catch (error) {
        if (error instanceof Error) {
          const { denials, rest } = parseDenials(error.message);
          if (denials.length) error.message = `${rest}\n\n${st.denialHint(compiled, denials)}`;
        }
        throw error;
      } finally {
        st.endCommand(id);
        policy.dispose();
        st.launched(command, compiled);
      }
    },
  });

  function annotateBash(result: AnyResult, st: Permissions, compiled: Awaited<ReturnType<Permissions["compile"]>>): AnyResult {
    const text = result.content.map((part) => (part.type === "text" ? (part.text ?? "") : "")).join("\n");
    const { denials, rest } = parseDenials(text);
    const structured = result.structuredContent as { output?: string } | undefined;
    const structuredContent = structured && typeof structured.output === "string" ? { ...structured, output: parseDenials(structured.output).rest } : result.structuredContent;
    const failed = result.isError === true;
    const denied = denials.length > 0 || (failed && DENIAL_TEXT.test(rest));
    if (!denied) {
      return denials.length || rest !== text ? { ...result, content: [{ type: "text", text: rest }], structuredContent } : result;
    }
    for (const denial of denials) {
      st.audit.write({ tool: "bash", capability: denial.kind === "network" ? "network" : denial.operation, paths: [denial.target], decision: "deny", decidedBy: "sandbox" });
    }
    return { ...result, content: [{ type: "text", text: `${rest}\n\n${st.denialHint(compiled, denials)}` }], structuredContent };
  }

  // --- request_permission -----------------------------------------------------------

  pi.registerTool({
    name: "request_permission",
    label: "Request permission",
    description:
      "Ask for access the permission rules do not give by default: writing outside the workspace, writing protected files (.git/config, git hooks, shell startup files), or reading/writing sensitive files (.env, keys, credentials). For bash, pass the exact command you will run next; the approval covers that command only. A command that can read a sensitive path runs offline unless network is also requested and approved.",
    promptSnippet: "Request access beyond the permission defaults (then retry the call)",
    promptGuidelines: [
      "File tools and bash run under permission rules: the workspace is readable and writable; other paths are readable but not writable; sensitive files (.env, keys, credentials) are neither. Protected files such as .git/config and git hooks need approval to write.",
      "When a call fails with a permission denial, call request_permission with the paths (and, for bash, the exact command), then retry the same call. Do not try to work around a denial.",
    ],
    parameters: Type.Object({
      capability: Type.Union([Type.Literal("read"), Type.Literal("write")], { description: "read, or write (write includes read)" }),
      paths: Type.Optional(
        Type.Array(Type.String(), {
          description: "The files or directories needed. Use env:NAME for an environment variable bash should keep. May be empty for a network-only request.",
        }),
      ),
      command: Type.Optional(Type.String({ description: "For bash: the exact command you will run next" })),
      network: Type.Optional(
        Type.Boolean({
          description: "For bash: the command needs direct network access (while reading a sensitive path, or past the network filter)",
        }),
      ),
      reason: Type.String({ description: "Why this access serves the user's request" }),
    }),
    async execute(_id, params, signal, _onUpdate, ctx) {
      const st = state(ctx);
      const outcome = await st.requestPermission(params, requestContext(ctx, signal));
      if (!outcome.ok) throw new Error(outcome.text);
      return { content: [{ type: "text", text: outcome.text }], details: undefined };
    },
  });

  // --- apply_patch (pi-codex-tools) and other tools ----------------------------------

  const inflightKey = (id: string) => `call:${id}`;

  (globalThis as Record<symbol, unknown>)[FS_HOOK] = {
    open: async (path: string, flags?: number | string, mode?: number) => {
      const st = permissions;
      if (!st || !st.enforcing) return nodeFs.open(path, flags as number, mode);
      const numeric = typeof flags === "number" ? flags : flags && flags !== "r" ? constants.O_WRONLY : constants.O_RDONLY;
      const writes = (numeric & (constants.O_WRONLY | constants.O_RDWR | constants.O_CREAT)) !== 0;
      const canonical = st.assert(writes ? "write" : "read", path, undefined);
      return nodeFs.open(canonical, flags as number, mode);
    },
    writeFile: async (path: string, data: string | Buffer, options?: unknown) => {
      const st = permissions;
      const canonical = st && st.enforcing ? st.assert("write", path, undefined) : path;
      return nodeFs.writeFile(canonical, data, options as never);
    },
    mkdir: async (path: string, options?: unknown) => {
      const st = permissions;
      if (st && st.enforcing) {
        let covered = false;
        for (const approved of st.inflight.values()) if (approved.coversParentOf(st.paths.canonical(path, process.cwd()))) covered = true;
        if (!covered) st.assert("write", path, undefined);
      }
      return nodeFs.mkdir(path, options as never);
    },
    unlink: async (path: string) => {
      const st = permissions;
      const canonical = st && st.enforcing ? st.assert("write", path, undefined) : path;
      return nodeFs.unlink(canonical);
    },
  };

  pi.on("session_start", async (_event, ctx) => {
    const st = state(ctx);
    st.reload(ctx.cwd, ctx.sessionManager.getBranch());
    st.noticeUnavailable(ctx.sessionManager.getBranch(), ctx.ui);
  });

  pi.on("tool_call", async (event, ctx) => {
    if (OWN_TOOLS.has(event.toolName)) return undefined;
    const st = state(ctx);
    if (!st.enforcing) return undefined;
    if (event.toolName === APPLY_PATCH) {
      const input = event.input as { input?: string; patch?: string };
      const patch = typeof input.input === "string" ? input.input : typeof input.patch === "string" ? input.patch : "";
      const requests = patchPaths(patch).map((path) => ({ capability: "write" as const, path, diff: patch.slice(0, 4000) }));
      const auth = await st.authorize(requests, { tool: APPLY_PATCH, toolCallId: event.toolCallId }, requestContext(ctx, undefined));
      if (!auth.ok) return { block: true, reason: auth.reason, terminate: auth.abort };
      st.inflight.set(inflightKey(event.toolCallId), auth.approved);
      return undefined;
    }
    // MCP and other extension tools: a tool-level decision.
    const mcp = event.toolName.match(/^mcp__(.+?)__(.+)$/);
    const decision = toolOverride(st.config, event.toolName, mcp?.[1]);
    if (decision === "allow") return undefined;
    if (decision === "deny") return { block: true, reason: `Permission denied: the ${event.toolName} tool is disabled by the permission settings.` };
    const outcome = await st.requestToolUse(event.toolName, event.input, requestContext(ctx, undefined));
    if (!outcome.ok) return { block: true, reason: outcome.reason, terminate: outcome.abort };
    return undefined;
  });

  pi.on("tool_execution_end", async (event) => {
    permissions?.inflight.delete(inflightKey(event.toolCallId));
  });

  pi.on("session_shutdown", async () => {
    await permissions?.stopProxy();
  });

  // --- Commands ----------------------------------------------------------------------

  pi.registerCommand("perm", {
    description: "Show permission control status: level, workspace roots, approval modes, grants",
    handler: async (_args, ctx) => {
      const st = state(ctx);
      st.reload(ctx.cwd, ctx.sessionManager.getBranch());
      ctx.ui.notify(st.statusText(), st.status.available ? "info" : "warning");
    },
  });

  pi.registerCommand("perm-grants", {
    description: "List this chat's permission grants; `/perm-grants revoke <id>` or `/perm-grants clear` removes them",
    handler: async (args, ctx) => {
      const st = state(ctx);
      const [action, id] = (args ?? "").trim().split(/\s+/);
      if (action === "revoke" && id) {
        const grant = st.revoke(id);
        ctx.ui.notify(grant ? `Revoked ${id}` : `No grant ${id}`, grant ? "info" : "warning");
        return;
      }
      if (action === "clear") {
        for (const grant of [...st.grants.all()]) st.revoke(grant.id);
        ctx.ui.notify("All grants for this chat were revoked.", "info");
        return;
      }
      ctx.ui.notify(st.grantsText(), "info");
    },
  });

  pi.registerCommand("perm-root", {
    description: "Add or remove an extra workspace root for this chat: /perm-root add|remove <path>",
    handler: async (args, ctx) => {
      const st = state(ctx);
      const [action, ...rest] = (args ?? "").trim().split(/\s+/);
      const target = rest.join(" ");
      if ((action !== "add" && action !== "remove") || !target) {
        ctx.ui.notify(`Workspace roots:\n${st.roots.join("\n")}\n\nUsage: /perm-root add|remove <path>`, "info");
        return;
      }
      const canonical = st.paths.canonical(target, ctx.cwd);
      if (action === "add") {
        // A slash command is typed by the user, so it is itself the approval.
        st.addRoot(canonical);
      } else {
        st.removeRoot(canonical);
      }
      st.reload(ctx.cwd, ctx.sessionManager.getBranch());
      ctx.ui.notify(`Workspace roots:\n${st.roots.join("\n")}`, "info");
    },
  });

  pi.registerCommand("perm-log", {
    description: "Show this chat's recent permission decisions",
    handler: async (_args, ctx) => {
      const st = state(ctx);
      const records = st.audit.recent(20);
      ctx.ui.notify(
        records.length
          ? records
              .map((record) => `${record.time?.slice(11, 19) ?? ""} ${record.decision} by ${record.decidedBy}: ${record.tool ?? ""} ${record.capability ?? ""} ${(record.paths ?? []).join(", ")}${record.reason ? ` — ${record.reason}` : ""}`)
              .join("\n")
          : "No permission decisions recorded for this chat yet.",
        "info",
      );
    },
  });
}

function isDirectory(path: string): boolean {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
}
