/**
 * Codemode as its own per-chat switch: `/scripts` turns Pi's `codemode` tool
 * on or off for the chat, and it starts on. Users know it as Scripts, the name
 * the transcript gives a codemode call; "codemode" stays Pi's internal name.
 *
 * Pi on its own activates codemode only when an MCP server with `codemode`
 * exposure is configured, and re-activates it whenever its MCP extension
 * looks again (session start, sign-in, reconnect). So the chat's setting is
 * enforced before every prompt, not only when it changes: whatever Pi turned
 * on in between, the request goes out with what the chat chose.
 *
 * With codemode off, MCP tools that are not `direct` can only be reached
 * through `tool_search`, so it is turned on for them (and off again when
 * codemode comes back, if this extension turned it on). Without either Pi
 * only warns that the tools cannot be called.
 *
 * A run started with an explicit tool list (`--tools`, as subagents are) is
 * left alone until that chat itself runs `/scripts`: the list is the
 * subagent's definition.
 *
 * Each toggle appends a `cypher-codemode.state` custom entry; the last one on
 * the session's branch wins, and the engine reads the same entries for the
 * composer's `/` menu (crates/engine/src/pi/session_modes.rs).
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

export const STATE_ENTRY_TYPE = "cypher-codemode.state";
const STATE_VERSION = 1;
export const CODEMODE_TOOL = "codemode";
export const TOOL_SEARCH_TOOL = "tool_search";

type SessionEntry = { type?: string; customType?: string; data?: unknown };
type ToolRef = { name: string; exposure?: string };

/** The last toggle on the session's branch, if any. */
export function sessionState(entries: SessionEntry[]): boolean | undefined {
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index];
    if (entry?.type !== "custom" || entry.customType !== STATE_ENTRY_TYPE) continue;
    const data = entry.data as { version?: unknown; enabled?: unknown } | undefined;
    if (data?.version === STATE_VERSION && typeof data.enabled === "boolean") return data.enabled;
  }
  return undefined;
}

/** Whether Pi was started with an explicit tool list. */
export function hasToolAllowlist(argv: string[]): boolean {
  return argv.some((arg) => ["--tools", "-t", "--no-tools", "-nt"].includes(arg) || arg.startsWith("--tools="));
}

const indirect = (exposure: unknown) => exposure !== "direct" && exposure !== "hidden";

/**
 * Whether an MCP server in the agent's mcp.json has tools only codemode or
 * tool_search reach. Read from the file because servers connect in the
 * background: their tools are not registered yet when the first prompt starts.
 */
export function configuredIndirectServers(mcpJson: unknown): boolean {
  const servers = (mcpJson as { mcpServers?: unknown } | undefined)?.mcpServers;
  if (!servers || typeof servers !== "object") return false;
  return Object.values(servers as Record<string, unknown>).some((server) => {
    if (!server || typeof server !== "object") return false;
    const { enabled, exposure, toolExposure } = server as Record<string, unknown>;
    if (enabled === false) return false;
    if (indirect(exposure ?? "codemode")) return true;
    return !!toolExposure && typeof toolExposure === "object" && Object.values(toolExposure).some(indirect);
  });
}

function readMcpJson(): unknown {
  const agentDir = process.env.PI_CODING_AGENT_DIR;
  if (!agentDir) return undefined;
  try {
    return JSON.parse(readFileSync(join(agentDir, "mcp.json"), "utf8"));
  } catch {
    return undefined;
  }
}

/**
 * The active tool list that carries out `enabled`, or undefined when nothing
 * changes. `addedToolSearch` says whether tool_search is on because of an
 * earlier call; the second value says whether it is now.
 */
export function nextActiveTools(
  enabled: boolean,
  active: string[],
  all: ToolRef[],
  mcpNeedsDiscovery: boolean,
  addedToolSearch: boolean,
): [string[] | undefined, boolean] {
  const registered = (name: string) => all.some((tool) => tool.name === name);
  const next = new Set(active);
  let added = addedToolSearch;
  if (enabled) {
    if (registered(CODEMODE_TOOL)) next.add(CODEMODE_TOOL);
    if (added) {
      next.delete(TOOL_SEARCH_TOOL);
      added = false;
    }
  } else {
    next.delete(CODEMODE_TOOL);
    const discovery = mcpNeedsDiscovery || all.some((tool) => tool.name.startsWith("mcp__") && indirect(tool.exposure));
    if (discovery && registered(TOOL_SEARCH_TOOL) && !next.has(TOOL_SEARCH_TOOL)) {
      next.add(TOOL_SEARCH_TOOL);
      added = true;
    }
  }
  const changed = next.size !== active.length || active.some((name) => !next.has(name));
  return [changed ? [...next] : undefined, added];
}

export default function cypherCodemode(pi: ExtensionAPI): void {
  const allowlisted = hasToolAllowlist(process.argv);
  // undefined: leave the tool list as the run was started with it.
  let enabled: boolean | undefined;
  let addedToolSearch = false;

  const apply = () => {
    if (enabled === undefined) return;
    const [tools, added] = nextActiveTools(
      enabled,
      pi.getActiveTools(),
      pi.getAllTools(),
      configuredIndirectServers(readMcpJson()),
      addedToolSearch,
    );
    addedToolSearch = added;
    if (tools) pi.setActiveTools(tools);
  };

  pi.registerCommand("scripts", {
    description: "Let the agent run several tools in one script. Faster, and uses less context.",
    handler: async (args: string, ctx: ExtensionContext) => {
      const requested = args.trim().toLowerCase();
      const current = enabled ?? pi.getActiveTools().includes(CODEMODE_TOOL);
      const next = requested === "on" ? true : requested === "off" ? false : !current;
      if (next && !pi.getAllTools().some((tool) => tool.name === CODEMODE_TOOL)) {
        // Pi registers no codemode tool when its built-in extension is off or
        // the run's --tools list leaves it out.
        ctx.ui.notify("Scripts are not available in this chat.", "warning");
        return;
      }
      enabled = next;
      pi.appendEntry(STATE_ENTRY_TYPE, { version: STATE_VERSION, enabled });
      apply();
      ctx.ui.notify(
        enabled
          ? "Scripts on."
          : addedToolSearch
            ? "Scripts off. MCP tools are found through tool search instead."
            : "Scripts off.",
        "info",
      );
    },
  });

  pi.on("session_start", (_event, ctx) => {
    enabled = sessionState(ctx.sessionManager.getBranch()) ?? (allowlisted ? undefined : true);
    addedToolSearch = false;
    apply();
  });

  pi.on("before_agent_start", () => {
    apply();
  });
}
