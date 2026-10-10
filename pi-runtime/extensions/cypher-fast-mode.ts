/**
 * GPT Fast mode: `/fast` adds OpenAI's `service_tier: "priority"` to requests
 * for GPT models from 5.4 on.
 *
 * A model is an OpenAI model when its provider or id says "openai" or "gpt",
 * and it qualifies when the version after "gpt" is at least
 * MINIMUM_GPT_VERSION, so new releases (gpt-6.1-sol, gpt-7, …) qualify
 * without a change here. Ids without a GPT version (o3, gpt-oss-120b, an
 * Azure deployment name) are left alone: there is nothing to compare.
 *
 * Replaces the gpt-fast-pi package and keeps its state formats, so chats
 * toggled under it keep their setting and the engine's `/` menu
 * (crates/engine/src/pi/session_modes.rs) reads both alike:
 * - each toggle appends a `gpt-fast-pi.state` custom entry; the last one on
 *   the session's branch wins;
 * - without one, `"pi-gpt-fast-mode": {"enabled": true}` in the agent's
 *   settings.json turns it on.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

export const FAST_SERVICE_TIER = "priority";
export const MINIMUM_GPT_VERSION = [5, 4];
export const STATE_ENTRY_TYPE = "gpt-fast-pi.state";
const STATE_VERSION = 1;
const DEFAULT_FIELD = "pi-gpt-fast-mode";

type ModelRef = { provider?: string; id?: string };
type SessionEntry = { type?: string; customType?: string; data?: unknown };

export function isOpenAiModel(model: ModelRef): boolean {
  return /openai|gpt/i.test(`${model.provider ?? ""}/${model.id ?? ""}`);
}

/** The version a model id names after "gpt": `gpt-6.1-sol` is [6, 1]. */
export function gptVersion(id: string): number[] | undefined {
  const match = /gpt-?(\d+(?:\.\d+)*)/i.exec(id);
  return match?.[1].split(".").map(Number);
}

/** Part by part, a missing part counting as 0: [6] > [5, 4] > [5]. */
export function versionAtLeast(version: number[], minimum: number[]): boolean {
  for (let index = 0; index < Math.max(version.length, minimum.length); index++) {
    const part = version[index] ?? 0;
    const floor = minimum[index] ?? 0;
    if (part !== floor) return part > floor;
  }
  return true;
}

export function supportsFastMode(model: ModelRef | undefined): boolean {
  if (!model?.id || !isOpenAiModel(model)) return false;
  const version = gptVersion(model.id);
  return !!version && versionAtLeast(version, MINIMUM_GPT_VERSION);
}

/**
 * Decide from the serialized request, which names the model that will
 * actually run. That differs from the selected model when a routing provider
 * resolves the selection per turn; the selection's provider only counts when
 * the request runs the selected model.
 */
export function shouldApplyFastMode(selected: ModelRef | undefined, payload: unknown): boolean {
  if (!payload || typeof payload !== "object") return false;
  const id = (payload as Record<string, unknown>).model;
  if (typeof id !== "string") return false;
  return supportsFastMode({ provider: id === selected?.id ? selected?.provider : undefined, id });
}

function defaultEnabled(): boolean {
  const agentDir = process.env.PI_CODING_AGENT_DIR;
  if (!agentDir) return false;
  try {
    const settings = JSON.parse(readFileSync(join(agentDir, "settings.json"), "utf8"));
    return settings?.[DEFAULT_FIELD]?.enabled === true;
  } catch {
    return false;
  }
}

/** The last toggle on the session's branch, if any. */
export function sessionState(entries: SessionEntry[]): boolean | undefined {
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index];
    if (entry?.type !== "custom" || entry.customType !== STATE_ENTRY_TYPE) continue;
    const data = entry.data as { version?: unknown; enabled?: unknown } | undefined;
    if ((data?.version === undefined || data.version === STATE_VERSION) && typeof data?.enabled === "boolean") {
      return data.enabled;
    }
  }
  return undefined;
}

function modelName(model: ModelRef | undefined): string {
  return model ? `${model.provider ?? "unknown"}/${model.id ?? "unknown"}` : "the current model";
}

export default function cypherFastMode(pi: ExtensionAPI): void {
  let enabled = defaultEnabled();

  pi.registerCommand("fast", {
    description: "Toggle GPT Fast mode (service_tier: priority)",
    handler: async (_args, ctx: ExtensionContext) => {
      enabled = !enabled;
      pi.appendEntry(STATE_ENTRY_TYPE, { version: STATE_VERSION, enabled });
      if (!enabled) {
        ctx.ui.notify("GPT Fast mode disabled.", "info");
      } else if (supportsFastMode(ctx.model)) {
        ctx.ui.notify(`GPT Fast mode enabled (service_tier: ${FAST_SERVICE_TIER}).`, "info");
      } else {
        ctx.ui.notify(`GPT Fast mode enabled, but ${modelName(ctx.model)} is not a supported GPT model.`, "warning");
      }
    },
  });

  pi.on("session_start", (_event, ctx) => {
    enabled = sessionState(ctx.sessionManager.getBranch()) ?? defaultEnabled();
  });

  pi.on("before_provider_request", (event, ctx) => {
    if (!enabled || !shouldApplyFastMode(ctx.model, event.payload)) return undefined;
    return { ...(event.payload as Record<string, unknown>), service_tier: FAST_SERVICE_TIER };
  });
}
