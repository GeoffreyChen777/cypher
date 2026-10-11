/**
 * Zone × capability → decision. Grants are checked by the caller; this only
 * answers what the level says and which approval mode an elevation uses.
 */

import { approvalMode } from "./config.ts";
import type { ApprovalMode, Capability, LevelName, PermissionsConfig } from "./config.ts";
import type { Classification } from "./paths.ts";

export type Verdict =
  | { kind: "allow" }
  | { kind: "deny"; reason: string }
  | { kind: "elevate"; key: string; mode: ApprovalMode };

const RANK = { allow: 0, elevate: 1, deny: 2 } as const;

export function decide(
  config: PermissionsConfig,
  level: LevelName,
  classification: Classification,
  capability: Capability,
): Verdict {
  if (level === "full-access") return { kind: "allow" };
  const { zone, guard } = classification;
  let decision = config.levels[level][zone][capability];
  let key = `${zone}.${capability}`;
  if (capability === "write" && guard && zone !== "sensitive") {
    if (RANK[decision] <= RANK.elevate) {
      decision = "elevate";
      key = guard === "self" ? "self.write" : "protected.write";
    }
  }
  if (decision === "allow") return { kind: "allow" };
  if (decision === "deny") {
    return { kind: "deny", reason: `${describe(classification)} is not ${capability === "read" ? "readable" : "writable"} at the ${level} level` };
  }
  const mode = approvalMode(config, key);
  if (mode === "deny") {
    return { kind: "deny", reason: `${describe(classification)}: ${capability} access is set to always deny` };
  }
  return { kind: "elevate", key, mode };
}

export function describe(classification: Classification): string {
  const { path, zone, guard } = classification;
  if (zone === "sensitive") return `${path} (sensitive)`;
  if (guard === "self") return `${path} (Cypher's own data)`;
  if (guard === "protected") return `${path} (protected: a write here runs code later)`;
  return `${path} (${zone === "workspace" ? "inside" : "outside"} the workspace)`;
}
