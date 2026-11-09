// Memory tools (docs/design/memory.md §3, §5). The engine embeds this file,
// writes it beside engine-client.mjs and loads it with `-e` into every Pi it
// starts; the tools register only when the engine gave the run memory
// (CYPHER_MEMORY) and the run belongs to a chat. Every tool is a thin call to
// the engine's `MemoryTool` RPC: the engine resolves which stores the chat
// sees, enforces read-only roots, holds writes from runs that read untrusted
// content and formats the answer, so nothing here decides what is visible.
import { pathToFileURL } from "node:url";
import { Type } from "typebox";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const CALL_TIMEOUT_MS = 30_000;

type EngineClient = {
  call(method: string, params?: unknown, options?: { timeoutMs?: number }): Promise<any>;
};

let connecting: Promise<EngineClient> | undefined;

function engine(): Promise<EngineClient> {
  connecting ??= (async () => {
    const module = process.env.CYPHER_ENGINE_CLIENT_MODULE;
    if (!module) throw new Error("the Cypher engine is not reachable from this run");
    const { connectEngine } = await import(pathToFileURL(module).href);
    return connectEngine() as Promise<EngineClient>;
  })().catch((error) => {
    connecting = undefined;
    throw error;
  });
  return connecting;
}

async function memoryTool(tool: string, args: unknown, toolCallId?: string): Promise<string> {
  const client = await engine();
  try {
    const reply = await client.call(
      "MemoryTool",
      { chatId: process.env.CYPHER_CHAT_ID, tool, args, toolCallId },
      { timeoutMs: CALL_TIMEOUT_MS },
    );
    return typeof reply?.text === "string" ? reply.text : JSON.stringify(reply);
  } catch (error) {
    // A dropped socket is redialed on the next call.
    if (error instanceof Error && error.message.startsWith("Engine IPC")) connecting = undefined;
    throw error;
  }
}

function text(value: string) {
  return { content: [{ type: "text" as const, text: value }], details: undefined };
}

const TOOLS = [
  {
    name: "memory",
    label: "Memory",
    description:
      "Your persistent memory: markdown files that carry what you learn across sessions. Each store you can see is a folder (/you, /project, /workspace, /role; /projects/<name> is read-only), and each store's /MEMORY.md is already in your instructions. Commands: view (a file, or a folder's listing), create (a new file, or replace one), str_replace (replace text that occurs once), insert (add text after a line; 0 = the top), delete, rename, search (rank files and lines by your words). Paths look like /project/MEMORY.md or /project/testing.md.",
    parameters: Type.Object({
      command: Type.Union(
        [
          Type.Literal("view"),
          Type.Literal("create"),
          Type.Literal("str_replace"),
          Type.Literal("insert"),
          Type.Literal("delete"),
          Type.Literal("rename"),
          Type.Literal("search"),
        ],
        { description: "What to do" },
      ),
      path: Type.Optional(Type.String({ description: "The file or folder, e.g. /project/MEMORY.md; / lists every store" })),
      file_text: Type.Optional(Type.String({ description: "create: the whole file" })),
      old_str: Type.Optional(Type.String({ description: "str_replace: text that occurs exactly once" })),
      new_str: Type.Optional(Type.String({ description: "str_replace: its replacement (may be empty)" })),
      insert_line: Type.Optional(Type.Integer({ minimum: 0, description: "insert: the line to insert after (0 = top)" })),
      insert_text: Type.Optional(Type.String({ description: "insert: the text to add" })),
      old_path: Type.Optional(Type.String({ description: "rename: the file now" })),
      new_path: Type.Optional(Type.String({ description: "rename: its new path, in the same store" })),
      view_range: Type.Optional(
        Type.Array(Type.Integer(), { minItems: 2, maxItems: 2, description: "view: [first, last] line, 1-based; -1 = the end" }),
      ),
      query: Type.Optional(Type.String({ description: "search: the words to look for" })),
    }),
  },
  {
    name: "search_history",
    label: "Search history",
    description:
      "Search past conversations you may read (this project's sessions; for a workspace, its own chat and threads and the sessions it can see) for words or phrases. Returns sessions with excerpts and message indexes for read_history.",
    parameters: Type.Object({
      query: Type.String({ description: "Words to find (all must match one message)" }),
      limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 30, description: "Sessions to return (default 10)" })),
    }),
  },
  {
    name: "read_history",
    label: "Read history",
    description:
      "Read messages of one past conversation from search_history. Without `from` it returns the latest messages; pass `from` (a message index) to page.",
    parameters: Type.Object({
      session_id: Type.String({ description: "Session id from search_history" }),
      from: Type.Optional(Type.Integer({ minimum: 0, description: "First message index to read" })),
      limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 60, description: "Default 20" })),
    }),
  },
];

export default function cypherMemory(pi: ExtensionAPI): void {
  if (!process.env.CYPHER_MEMORY || !process.env.CYPHER_CHAT_ID) return;
  for (const tool of TOOLS) {
    pi.registerTool({
      ...tool,
      execute: async (id: string, params: unknown) => {
        try {
          return text(await memoryTool(tool.name, params ?? {}, id));
        } catch (error) {
          return text(`${tool.label} failed: ${error instanceof Error ? error.message : String(error)}`);
        }
      },
    } as any);
  }
}
