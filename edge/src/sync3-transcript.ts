import schema from "../../apps/ios/Cypher/Sync/Sync3PartSchema.json";
import { matches } from "./sync3-command";

export type Part =
  | { kind: "text"; id: string; text: string }
  | { kind: "error"; id: string; message: string }
  | { kind: "input"; id: string; requestId: string; questions: unknown[]; resolved: boolean }
  | { kind: "tool"; id: string; call: Record<string, unknown>; resolved: boolean; isError: boolean;
      output?: string; progress?: string; diff?: unknown; outputRef?: string;
      outputBytes?: number; diffRef?: string; diffStats?: unknown[] };
export interface Entry {
  id: string; role: "user" | "assistant" | "system"; createdAt: number; deviceId: string;
  parts: Part[]; status?: "streaming" | "complete" | "aborted"; continuationOf?: string;
}
export function validPart(value: unknown): value is Part {
  if (!matches(value, schema)) return false;
  const part = value as Part;
  if (part.kind === "tool" && part.call.kind === "unknown" && part.call.name === "subagent") {
    const input = part.call.input as { task?: unknown } | undefined;
    return typeof input?.task === "string" && Array.from(input.task).length <= 500;
  }
  return true;
}
