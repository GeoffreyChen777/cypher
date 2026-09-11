// Closed shape descriptor shared with native Rust and the iOS bundle.
import schema from "../../apps/ios/Cypher/Sync/Sync3CommandSchema.json";

export type CommandStatus = "pending" | "applied" | "rejected" | "expired" | "superseded" | "cancelled";
export interface Command {
  id: string; issuedBy: string; issuedAt: number; sentAt?: number;
  basedOn: { turnId: string | null; frontier: null } | null;
  expiresAt: number | null; status: CommandStatus; resolution: string | null;
  payload:
    | { kind: "run"; messageId: string; agentPrompt?: string; request: {
      prompt: string; harness?: string; model: string | null; reasoning: string | null;
      modelOptions: Record<string, unknown>; cwd: string; sandbox: string;
      autoApprove: boolean; resume: string | null; attachments?: string[];
      pendingAttachments?: { uploadId: string; fileName: string }[];
      worktree?: { repoPath: string; baseRef: string; nameHint?: string };
    } }
    | { kind: "steer"; prompt: string; messageId: string | null; agentPrompt?: string }
    | { kind: "interrupt" }
    | { kind: "respondInput"; requestId: string; answers: { questionId: string; labels: string[] }[] };
}
export function isEntityId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_.:#~-]{1,200}$/.test(value);
}
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function matches(value: unknown, rule: unknown): boolean {
  if (typeof rule === "string") {
    switch (rule) {
      case "id": return typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
      case "entityId": return isEntityId(value);
      case "string": return typeof value === "string";
      case "nonemptyString": return typeof value === "string" && value.length > 0;
      case "uint": return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
      case "bool": return typeof value === "boolean";
      case "null": return value === null;
      case "json": return true; // validateOperation checks depth, Unicode and number bounds.
      default: return false;
    }
  }
  if (!object(rule)) return false;
  if (Object.hasOwn(rule, "nullable")) return value === null || matches(value, rule.nullable);
  if (Array.isArray(rule.oneOf)) return rule.oneOf.filter(s => matches(value, s)).length === 1;
  if (Array.isArray(rule.enum)) return rule.enum.includes(value);
  const min = typeof rule.min === "number" ? rule.min : 0, max = typeof rule.max === "number" ? rule.max : 256;
  if (Object.hasOwn(rule, "array")) {
    return Array.isArray(value) && value.length >= min && value.length <= max && value.every(v => matches(v, rule.array));
  }
  if (Object.hasOwn(rule, "map")) {
    return object(value) && Object.keys(value).length <= max && Object.values(value).every(v => matches(v, rule.map));
  }
  if (object(rule.object)) {
    const fields = rule.object, optional = Array.isArray(rule.optional) ? rule.optional : [];
    return object(value) && Object.keys(value).every(k => Object.hasOwn(fields, k))
      && Object.entries(fields).every(([k, s]) => Object.hasOwn(value, k) ? matches(value[k], s) : optional.includes(k));
  }
  return false;
}
export function validCommand(value: unknown): value is Command { return matches(value, schema); }
