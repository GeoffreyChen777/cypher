/**
 * The chat's permission level while it runs (docs/design/permission-control.md §5.3, "Changing the level
 * mid-run").
 *
 * A run starts at the level the engine passed (`CYPHER_PERMISSION_LEVEL`). When the owner changes it — the
 * chat's `/permissions`, or a workspace plan's level, which reaches every session the plan started — the
 * engine writes the new one to this chat's level file, and the extension re-reads it before each decision
 * (a stat when nothing changed). The file is a doorbell, not the authority:
 *
 * - It lives in the agent directory's `cypher-permissions/`, the permission system's own
 *   (config.ts `permissionSystemPatterns`): outside every sandbox's writable roots, and on the floor for a
 *   file tool or a command that would write it, at every level (core.ts `fullAccessCommand`).
 * - A level above the one the run is at is taken only once the engine confirms it (`PermissionLevel` on
 *   the agent socket, `Permissions.levelNow`), so a file the agent forged gains it nothing. A stricter
 *   level is taken from the file alone: it can only take away.
 *
 * Written by the engine as `{"level": "<sandbox wire name>", "at": <ms>}` (crates/harness/src/pi/spawn.rs
 * `write_level_file`), at the same path (`levelFile`).
 */

import { readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { LevelName } from "./config.ts";

/** Strictest first. */
export const LEVEL_RANK: Readonly<Record<LevelName, number>> = { "read-only": 0, standard: 1, "full-access": 2 };

const NAMES: Readonly<Record<string, LevelName>> = {
  "read-only": "read-only",
  "workspace-write": "standard",
  "danger-full-access": "full-access",
  standard: "standard",
  "full-access": "full-access",
};

/** A level from Cypher's sandbox wire name or the extension's own; undefined for anything else. */
export function levelName(value: unknown): LevelName | undefined {
  return typeof value === "string" ? NAMES[value.trim()] : undefined;
}

/** Whether `a` allows more than `b`. */
export function above(a: LevelName, b: LevelName): boolean {
  return LEVEL_RANK[a] > LEVEL_RANK[b];
}

/** Where the engine keeps a chat's live level. */
export function levelFile(agentDir: string, chatId: string | undefined): string | undefined {
  if (!agentDir || !chatId) return undefined;
  return join(agentDir, "cypher-permissions", "levels", `${chatId.replace(/[^A-Za-z0-9._-]/g, "_")}.json`);
}

/** The file's identity as of now: changes with every write; empty when there is none. */
export function levelStamp(path: string): string {
  try {
    const stat = statSync(path, { bigint: true });
    return `${stat.mtimeNs}:${stat.ctimeNs}:${stat.ino}:${stat.size}`;
  } catch {
    return "";
  }
}

/** The level the file names; undefined when it is missing or unreadable. */
export function readLevelFile(path: string): LevelName | undefined {
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8")) as { level?: unknown };
    return levelName(parsed?.level);
  } catch {
    return undefined;
  }
}
