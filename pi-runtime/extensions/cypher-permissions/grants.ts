/**
 * Grants: what an approval covers and for how long.
 *
 * - `once`: consumed by the next matching use, held in memory: Pi runs one
 *   process per turn and a once grant is asked for and used inside the same
 *   turn.
 * - `chat`: persisted as custom entries in the Pi session file, tagged with
 *   the chat id so a forked copy of the file does not inherit them.
 * - `task`: in memory, until the user's next message (`turn`): what a
 *   dangerous approval covers, so a release's push and tags ask once and
 *   nothing dangerous carries over to the next request.
 * - `always`: a rule in the user's config file (non-sensitive only),
 *   usually for one project.
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
export type GrantScope = "once" | "task" | "chat" | "always";
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
  /** Variables the command may see assigned in front of it. */
  env?: string[];
  /** At least one path is sensitive. */
  sensitive?: boolean;
  scope: GrantScope;
  approvedBy: ApprovedBy;
  createdAt: string;
  chatId?: string;
  /** A task grant: the user messages seen when it was given; it lapses at the next one. */
  turn?: number;
  /** A task grant for a dangerous command: the kinds of danger it was approved for (risk.ts keys). */
  dangers?: string[];
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
  /** The user messages on the branch now: task grants from an earlier count have lapsed. */
  turn = 0;

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
    const kept = this.grants.filter((grant) => grant.scope === "once" || grant.scope === "task");
    this.grants = [...restored.filter((grant) => !revoked.has(grant.id)), ...kept];
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

  /** The grants in force: task grants only in the turn they were given. */
  all(): readonly Grant[] {
    return this.grants.filter((grant) => grant.scope !== "task" || grant.turn === this.turn);
  }

  /** A grant usable by a file tool. */
  findForFile(capability: GrantCapability, path: string): Grant | undefined {
    return this.all().find((grant) => grant.pattern === undefined && grantCovers(grant, capability, path));
  }

  /** Consume a once grant after its use. Chat grants stay. */
  used(grant: Grant): void {
    if (grant.scope === "once") this.remove(grant.id);
  }

  /** Grants a sandboxed command runs with: the non-sensitive path grants. */
  forSandbox(): Grant[] {
    return this.all().filter((grant) => grant.pattern === undefined && !grant.sensitive);
  }

  /** This chat's approvals to run commands outside the sandbox, as rules; a task grant also vouches for the dangers it was approved for. */
  commandRules(): (CommandRule & { dangers?: string[] })[] {
    return this.all().flatMap((grant) =>
      grant.pattern
        ? [{ match: grant.pattern, decision: "allow" as const, label: grant.label, sha256: grant.sha256, env: grant.env, ...(grant.dangers ? { dangers: grant.dangers } : {}) }]
        : [],
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
