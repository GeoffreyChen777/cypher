// Agent-bot tools (docs/design/agent-bots.md §7). The engine embeds this file,
// writes it beside engine-client.mjs and loads it with `-e` into bot runs
// only; the run's `--tools` allowlist is exactly these names
// (cypher_proto::BOT_TOOLS). Every tool is a thin call to the engine's
// `BotTool` RPC: the engine resolves the bot from CYPHER_CHAT_ID, enforces
// its scope and formats the answer, so nothing here decides what is visible.
import { pathToFileURL } from "node:url";
import { Type } from "typebox";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const CALL_TIMEOUT_MS = 60_000;

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

async function botTool(tool: string, args: unknown, timeoutMs = CALL_TIMEOUT_MS): Promise<string> {
  const client = await engine();
  try {
    const reply = await client.call(
      "BotTool",
      { chatId: process.env.CYPHER_CHAT_ID, tool, args },
      { timeoutMs },
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

type ToolDef = {
  name: string;
  label: string;
  description: string;
  parameters: unknown;
  /** Calls that wait (on a session, an approval, another bot) need longer. */
  timeoutMs?: number;
};

// Every bot tool is registered; the run's `--tools` allowlist (decided by the
// engine per thread and capabilities) is what the model is actually offered.
const TOOLS: ToolDef[] = [
  {
    name: "list_projects",
    label: "List projects",
    description:
      "List the projects you can see: name, device, folder, session counts and last activity. Use the project id with the other tools.",
    parameters: Type.Object({}),
  },
  {
    name: "list_sessions",
    label: "List sessions",
    description:
      "List sessions you can see, newest activity first: id, title, project, device, status (working, waiting, idle, …), last activity and a preview of the latest message.",
    parameters: Type.Object({
      project: Type.Optional(Type.String({ description: "Project id or name to filter by" })),
      status: Type.Optional(
        Type.Union([Type.Literal("active"), Type.Literal("all")], {
          description: "active = working or waiting on someone; all (default) = every session",
        }),
      ),
      since_hours: Type.Optional(
        Type.Number({ minimum: 0, description: "Only sessions active within this many hours" }),
      ),
      limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 200, description: "Default 50" })),
    }),
  },
  {
    name: "session_activity",
    label: "Recent activity",
    description:
      "What changed recently: sessions started, sessions with new messages (with their status now — working, waiting for input, errored, idle — and latest message) and runs that finished, grouped by project. Use it for \"what happened today/this week\" or catch-up questions before reading single sessions.",
    parameters: Type.Object({
      since_hours: Type.Optional(
        Type.Number({ minimum: 0, maximum: 336, description: "Window in hours (default 24, at most 336 = 14 days)" }),
      ),
      project: Type.Optional(Type.String({ description: "Project id or name to limit to" })),
    }),
  },
  {
    name: "search_sessions",
    label: "Search sessions",
    description:
      "Search session titles and message text for words or phrases. Returns matching sessions with excerpts and message indexes for read_session.",
    parameters: Type.Object({
      query: Type.String({ description: "Words to find (all must match a message or title)" }),
      project: Type.Optional(Type.String({ description: "Project id or name to search in" })),
      limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 50, description: "Default 20" })),
    }),
  },
  {
    name: "read_session",
    label: "Read session",
    description:
      "Read messages from one session. Without `from` it returns the latest messages; pass `from` (a message index) to page. Tool calls are shown as one-line summaries.",
    parameters: Type.Object({
      session_id: Type.String({ description: "Session id from list_sessions or search_sessions" }),
      from: Type.Optional(Type.Integer({ minimum: 0, description: "First message index to read" })),
      limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 60, description: "Default 20" })),
    }),
  },
  {
    name: "project_git",
    label: "Read git state",
    description:
      "Read one project's git state: current branch, ahead/behind its upstream, the last 10 commits (sha, subject, author, time) and uncommitted working-tree changes. Use it for questions about code state — what was committed, what is unpushed or uncommitted — rather than what was said in sessions. A project on another device needs that device online.",
    parameters: Type.Object({
      project: Type.String({ description: "Project id or name (an id when the name is on several devices)" }),
    }),
  },

  // ── coordination (P2): plans, orchestration threads, worker sessions ──

  // ── end coordination ──

  // ── memory and routines (P3): notebook, schedule ──
  {
    name: "read_notes",
    label: "Read notes",
    description:
      "Read your notebook: short notes you or your owner kept across conversations, newest first, each with where it came from. Your prompt already shows the newest notes; use this to search older ones.",
    parameters: Type.Object({
      query: Type.Optional(Type.String({ description: "Words a note must contain (any case)" })),
    }),
  },
  {
    name: "write_note",
    label: "Write note",
    description:
      "Add one note to your notebook: a short fact or decision worth remembering in later conversations (\"the release is blocked on the migration\", \"Alice owns auth\"). One fact per note, at most 1000 characters. The note records which thread wrote it.",
    parameters: Type.Object({
      text: Type.String({ description: "The note" }),
    }),
  },
  {
    name: "list_schedule",
    label: "Checked schedule",
    description:
      "List your routines and the recurring tasks of your projects: name, id, schedule, next run and the last run's result. Times are the host device's local time.",
    parameters: Type.Object({}),
  },
  {
    name: "save_routine",
    label: "Save routine",
    description:
      "Create or change a routine: a job you run on a schedule, unattended, in its own thread; its result is posted in this chat. Use it when your owner asks for something recurring (\"every morning, tell me what's stuck\"). Pass `id` to change an existing routine (list_schedule shows ids). The schedule is one of {\"kind\":\"daily\",\"time\":\"09:00\",\"days\":[1,2,3,4,5]} (days 1 = Monday … 7 = Sunday; leave out for every day), {\"kind\":\"interval\",\"minutes\":60} (at least 15) or {\"kind\":\"manual\"} (only when run by hand), in the host device's local time. Returns what was saved and the next run.",
    parameters: Type.Object({
      id: Type.Optional(Type.String({ description: "The routine to change; leave out to create one" })),
      name: Type.String({ description: "A short name, e.g. Morning brief" }),
      prompt: Type.String({ description: "What each run should do and report, written as an instruction to yourself" }),
      schedule: Type.Object(
        {
          kind: Type.Union([Type.Literal("daily"), Type.Literal("interval"), Type.Literal("manual")]),
          time: Type.Optional(Type.String({ description: "daily: HH:MM, 24-hour" })),
          days: Type.Optional(Type.Array(Type.Integer({ minimum: 1, maximum: 7 }), { description: "daily: 1 = Monday … 7 = Sunday" })),
          minutes: Type.Optional(Type.Integer({ minimum: 15, description: "interval: minutes between runs" })),
        },
        { description: "When it runs" },
      ),
      write_notes: Type.Optional(Type.Boolean({ description: "Whether its runs may write to your notebook (default off)" })),
      enabled: Type.Optional(Type.Boolean({ description: "Off keeps it but runs it only by hand (default on)" })),
    }),
  },
  {
    name: "save_project_task",
    label: "Save project task",
    description:
      "Create or change a recurring task on one of your projects: every run starts a fresh session there, unattended, on the project's device (\"every night, run the tests and fix anything flaky\"). Needs the Coordinate permission. Pass `id` to change an existing task. The schedule takes the same shapes as save_routine: {\"kind\":\"daily\",\"time\":\"02:00\",\"days\":[1,2,3,4,5]}, {\"kind\":\"interval\",\"minutes\":60} or {\"kind\":\"manual\"}, in that device's local time.",
    parameters: Type.Object({
      id: Type.Optional(Type.String({ description: "The task to change; leave out to create one" })),
      project: Type.String({ description: "Project id or name" }),
      name: Type.String({ description: "A short name; sessions are titled \"<name> · <date>\"" }),
      prompt: Type.String({ description: "The instruction each run's session gets" }),
      schedule: Type.Object(
        {
          kind: Type.Union([Type.Literal("daily"), Type.Literal("interval"), Type.Literal("manual")]),
          time: Type.Optional(Type.String({ description: "daily: HH:MM, 24-hour" })),
          days: Type.Optional(Type.Array(Type.Integer({ minimum: 1, maximum: 7 }), { description: "daily: 1 = Monday … 7 = Sunday" })),
          minutes: Type.Optional(Type.Integer({ minimum: 15, description: "interval: minutes between runs" })),
        },
        { description: "When it runs" },
      ),
      worktree: Type.Optional(Type.Boolean({ description: "Run each session in a new worktree (default off)" })),
    }),
  },

  // ── end memory ──

  // ── network (P4): connections, ask_bot ──

  // ── end network ──
];

export default function cypherBotTools(pi: ExtensionAPI): void {
  if (!process.env.CYPHER_BOT_ID) return;
  for (const tool of TOOLS) {
    pi.registerTool({
      ...tool,
      execute: async (_id: string, params: unknown) => {
        try {
          return text(await botTool(tool.name, params ?? {}, tool.timeoutMs));
        } catch (error) {
          return text(`${tool.label} failed: ${error instanceof Error ? error.message : String(error)}`);
        }
      },
    } as any);
  }
}
