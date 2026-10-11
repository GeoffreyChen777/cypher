/**
 * Grants: what an approval covers and for how long.
 *
 * - `once`: consumed by the next matching use (a bash grant by the next run
 *   of its exact command), held in memory: Pi runs one process per turn and a
 *   once grant is asked for and used inside the same turn.
 * - `chat`: persisted as custom entries in the Pi session file, tagged with
 *   the chat id so a forked copy of the file does not inherit them.
 * - `always`: a rule in the user's config file (non-sensitive only).
 *
 * A grant on a sensitive path used by bash is always bound to one command: a
 * command that can read a secret runs offline unless the same approval also
 * allowed the network (see sandbox.ts).
 */

import { randomUUID } from "node:crypto";
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
  /** Bash: the exact command the grant is bound to. */
  command?: string;
  /** Bash: the command may also use the network although it touches a sensitive path. */
  network?: boolean;
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

  /** A grant usable by a file tool (not bound to a command). */
  findForFile(capability: GrantCapability, path: string): Grant | undefined {
    return this.grants.find((grant) => grant.command === undefined && grantCovers(grant, capability, path));
  }

  /** Consume a once grant after its use. Chat grants stay. */
  used(grant: Grant): void {
    if (grant.scope === "once") this.remove(grant.id);
  }

  /** Grants a bash command runs with: unbound non-sensitive grants plus those bound to this exact command. */
  forCommand(command: string): Grant[] {
    return this.grants.filter((grant) =>
      grant.command === undefined ? !grant.sensitive : grant.command === command,
    );
  }

  consumeForCommand(command: string): void {
    this.grants = this.grants.filter((grant) => !(grant.scope === "once" && grant.command === command));
  }
}

function isGrant(value: Partial<Grant>): value is Grant {
  return (
    typeof value.id === "string" &&
    (value.capability === "read" || value.capability === "write") &&
    Array.isArray(value.paths) &&
    value.paths.every((path) => typeof path === "string") &&
    (value.scope === "once" || value.scope === "chat" || value.scope === "always")
  );
}
