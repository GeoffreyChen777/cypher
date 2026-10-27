// `/explain`: a page that explains what this session's work does, kept up to
// date as the chat goes on. The command asks for the page now; while it is on,
// every turn's appended system prompt asks the model to end each reply that
// changed the work by writing the page again under the same title, which the
// chat reads as a revision: the page's open tab reloads and every card with
// that title opens the newest version. Like chat-rendering.ts, the engine
// loads it into every Pi it starts (`-e`), so it ships with the app.
//
// The state is a custom entry in the chat's Pi session, the last one on the
// branch winning, so the engine's `/` menu (crates/engine/src/pi/session_modes.rs)
// can read it like `/fast`'s:
// `cypher-explain.state` `{version: 1, enabled, focus?}`.

export const EXPLAIN_TITLE = "Session explainer";
export const STATE_ENTRY_TYPE = "cypher-explain.state";
const STATE_VERSION = 1;

export type ExplainState = { enabled: boolean; focus?: string };
type SessionEntry = { type?: string; customType?: string; data?: unknown };
export type ExplainAction = { action: "on"; focus?: string } | { action: "off" } | { action: "status" };

/** The last state on the session's branch; off without one. */
export function sessionState(entries: SessionEntry[]): ExplainState {
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index];
    if (entry?.type !== "custom" || entry.customType !== STATE_ENTRY_TYPE) continue;
    const data = entry.data as { version?: unknown; enabled?: unknown; focus?: unknown } | undefined;
    if (data?.version !== STATE_VERSION || typeof data.enabled !== "boolean") continue;
    const focus = typeof data.focus === "string" && data.focus.trim() ? data.focus.trim() : undefined;
    return { enabled: data.enabled, focus };
  }
  return { enabled: false };
}

/** `/explain`, `/explain on`, `/explain off`, `/explain status`, or
 * `/explain <what to focus on>`. */
export function parseArgs(args: string): ExplainAction {
  const text = args.trim();
  switch (text.toLowerCase()) {
    case "":
    case "on":
      return { action: "on" };
    case "off":
      return { action: "off" };
    case "status":
      return { action: "status" };
    default:
      return { action: "on", focus: text };
  }
}

function focusLine(focus: string | undefined): string {
  return focus ? `\nThe user asked it to focus on: ${focus}` : "";
}

/** What `/explain` asks for: the page, now. */
export function firstPrompt(focus?: string): string {
  return `Write the session explainer now: one artifact, \`\`\`html artifact title="${EXPLAIN_TITLE}"\`\`\`. It explains the work done in this session so far to someone who wasn't following: what was built or changed and the features it adds, how the parts fit together (a diagram where it helps), notable decisions and trade-offs, and what is still open. Base it on this conversation and the files worked on; read files if you need details, but change nothing. If no work has been done yet, write a short page saying so. Write the page in the language the user writes in, but keep the title exactly as given. Keep the reply outside the artifact to a sentence or two.${focusLine(focus)}`;
}

/** The standing instruction while the explainer is on. */
export function trackingBlock(focus?: string): string {
  return `<cypher_explain>
The user turned on a live explainer for this session (/explain): an artifact titled "${EXPLAIN_TITLE}" that explains the session's work: what was built or changed and the features it adds, how the parts fit together, notable decisions, and what is still open.${focusLine(focus)}
- At the end of every reply that changes the work (files written or changed, a design decision, a bug found or fixed, a change of plan), write the whole "${EXPLAIN_TITLE}" artifact again with the changes folded in, under exactly that title so it replaces the earlier version. Put it after everything else in the reply.
- Skip it when a reply changes nothing about the work, such as an answer to a question.
- Write it in the user's language; keep the title as given.
</cypher_explain>`;
}

/** The appended prompt with the explainer's block after the rest. */
export function withTracking(append: string, focus?: string): string {
  const block = trackingBlock(focus);
  if (append.includes(block)) return append;
  const rest = append.trim();
  return rest ? `${rest}\n\n${block}` : block;
}

type Context = {
  isIdle(): boolean;
  ui: { notify(message: string, level?: "info" | "warning" | "error"): void };
  sessionManager: { getBranch(): SessionEntry[] };
};

type Pi = {
  registerCommand(
    name: string,
    options: {
      description?: string;
      getArgumentCompletions?: (prefix: string) => { value: string; label: string; description?: string }[] | null;
      handler: (args: string, ctx: Context) => Promise<void>;
    },
  ): void;
  on(event: "session_start", handler: (event: unknown, ctx: Context) => void): void;
  on(
    event: "before_agent_start",
    handler: (event: { systemPromptOptions?: { appendSystemPrompt?: string } }) => void,
  ): void;
  appendEntry(customType: string, data?: unknown): void;
  sendUserMessage(content: string, options?: { deliverAs?: "steer" | "followUp" }): void;
};

const CHOICES = [
  { value: "on", label: "on", description: "Write the explainer now and keep it up to date" },
  { value: "off", label: "off", description: "Stop updating the explainer" },
  { value: "status", label: "status", description: "Show whether the explainer is on" },
];

export default function explain(pi: Pi): void {
  let state: ExplainState = { enabled: false };

  const save = (next: ExplainState) => {
    state = next;
    pi.appendEntry(STATE_ENTRY_TYPE, { version: STATE_VERSION, ...next });
  };

  pi.registerCommand("explain", {
    description: "Explain this session's work in a page that keeps up with the chat",
    getArgumentCompletions: (prefix) => {
      const matches = CHOICES.filter((choice) => choice.value.startsWith(prefix.trim().toLowerCase()));
      return matches.length ? matches : null;
    },
    handler: async (args, ctx) => {
      const request = parseArgs(args);
      if (request.action === "status") {
        ctx.ui.notify(
          state.enabled
            ? `The session explainer is on${state.focus ? `, focused on: ${state.focus}` : ""}.`
            : "The session explainer is off.",
          "info",
        );
        return;
      }
      if (request.action === "off") {
        save({ ...state, enabled: false });
        ctx.ui.notify("Stopped updating the session explainer.", "info");
        return;
      }
      save({ enabled: true, focus: request.focus ?? state.focus });
      pi.sendUserMessage(firstPrompt(state.focus), ctx.isIdle() ? undefined : { deliverAs: "followUp" });
    },
  });

  pi.on("session_start", (_event, ctx) => {
    state = sessionState(ctx.sessionManager.getBranch());
  });

  pi.on("before_agent_start", (event) => {
    // Runtimes without mutable prompt options run on without the tracking.
    const options = event.systemPromptOptions;
    if (state.enabled && options) {
      options.appendSystemPrompt = withTracking(options.appendSystemPrompt ?? "", state.focus);
    }
  });
}
