/**
 * Grants: what an approval covers and for how long.
 *
 * - `once`: consumed by the next matching use, held in memory: Pi runs one
 *   process per turn and a once grant is asked for and used inside the same
 *   turn.
 * - `chat`: persisted as custom entries in the Pi session file, tagged with
 *   the chat id so a forked copy of the file does not inherit them.
 * - `always`: a rule in the user's config file (non-sensitive only).
 *
 * A grant covers paths (file tools; sandboxed commands get the non-sensitive
 * ones) or, with `pattern`, commands that run outside the sandbox. Sandboxed
 * commands never get a sensitive path: a command that needs a secret runs
 * outside the sandbox instead.
 */

import { randomUUID } from "node:crypto";
import { patternError } from "./commands.ts";
import type { CommandRule } from "./commands.ts";
import { isUnder } from "./paths.ts";

export const GRANT_ENTRY = "cypher-permissions.grant";
export const REVOKE_ENTRY = "cypher-permissions.revoke";
export const ROOT_ENTRY = "cypher-permissions.root";

export type GrantCapability = "read" | "write";
export type GrantScope = "once" | "chat" | "always";
export type ApprovedBy = "user" | "judge" | "rule";

export interface Grant {
  id: string;
  capability: GrantCapability;
  /** Canonical paths; a directory grant covers its subtree. `env:NAME` names an environment variable. */
  paths: string[];
  /** Commands matching this pattern run outside the sandbox (commands.ts). */
  pattern?: string[];
  /** What the pattern is for, as the agent described it. */
  label?: string;
  /** The program's contents, when it lives in a writable place. */
  sha256?: string;
  /** At least one path is sensitive. */
  sensitive?: boolean;
  scope: GrantScope;
  approvedBy: ApprovedBy;
  createdAt: string;
  chatId?: string;
}

export function newGrant(fields: Omit<Grant, "id" | "createdAt">): Grant {
  return { id: randomUUID(), createdAt: new Date().toISOString(), ...fields };
}

/** A write grant implies read: editing a file reads it first. */
export function grantCovers(grant: Grant, capability: GrantCapability, path: string): boolean {
  if (capability === "write" && grant.capability !== "write") return false;
  return grant.paths.some((granted) => (granted.startsWith("env:") ? granted === path : isUnder(path, granted)));
}

type Entry = { type?: string; customType?: string; data?: unknown };

export class GrantStore {
  private grants: Grant[] = [];
  private readonly chatId: string | undefined;

  constructor(chatId: string | undefined) {
    this.chatId = chatId;
  }

  /** Rebuild the chat grants from the session's custom entries. */
  restore(entries: readonly Entry[]): void {
    const revoked = new Set<string>();
    const restored: Grant[] = [];
    for (const entry of entries) {
      if (entry.type !== "custom") continue;
      const data = entry.data as Partial<Grant> & { id?: string; chatId?: string };
      if (!data || typeof data !== "object") continue;
      if (this.chatId && data.chatId && data.chatId !== this.chatId) continue;
      if (this.chatId && !data.chatId) continue;
      if (entry.customType === REVOKE_ENTRY && typeof data.id === "string") revoked.add(data.id);
      if (entry.customType === GRANT_ENTRY && isGrant(data) && data.scope === "chat") restored.push(data);
    }
    const once = this.grants.filter((grant) => grant.scope === "once");
    this.grants = [...restored.filter((grant) => !revoked.has(grant.id)), ...once];
  }

  add(grant: Grant): void {
    this.grants.push(grant);
  }

  remove(id: string): Grant | undefined {
    const index = this.grants.findIndex((grant) => grant.id === id);
    if (index < 0) return undefined;
    return this.grants.splice(index, 1)[0];
  }

  clear(): Grant[] {
    const removed = this.grants;
    this.grants = [];
    return removed;
  }

  all(): readonly Grant[] {
    return this.grants;
  }

  /** A grant usable by a file tool. */
  findForFile(capability: GrantCapability, path: string): Grant | undefined {
    return this.grants.find((grant) => grant.pattern === undefined && grantCovers(grant, capability, path));
  }

  /** Consume a once grant after its use. Chat grants stay. */
  used(grant: Grant): void {
    if (grant.scope === "once") this.remove(grant.id);
  }

  /** Grants a sandboxed command runs with: the non-sensitive path grants. */
  forSandbox(): Grant[] {
    return this.grants.filter((grant) => grant.pattern === undefined && !grant.sensitive);
  }

  /** This chat's approvals to run commands outside the sandbox, as rules. */
  commandRules(): CommandRule[] {
    return this.grants.flatMap((grant) =>
      grant.pattern ? [{ match: grant.pattern, decision: "allow" as const, label: grant.label, sha256: grant.sha256 }] : [],
    );
  }
}

function isGrant(value: Partial<Grant> & { command?: unknown }): value is Grant {
  return (
    typeof value.id === "string" &&
    // Grants bound to one command's text, from older Runtimes, are retired.
    value.command === undefined &&
    (value.pattern === undefined || patternError(value.pattern) === undefined) &&
    (value.capability === "read" || value.capability === "write") &&
    Array.isArray(value.paths) &&
    value.paths.every((path) => typeof path === "string") &&
    (value.scope === "once" || value.scope === "chat" || value.scope === "always")
  );
}
