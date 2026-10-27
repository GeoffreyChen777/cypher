// Tells the model what Cypher's chat renders, so it reaches for a diagram, or
// a page of its own (an artifact), when one explains better than prose. The
// engine writes this file beside its engine client and loads it into every Pi
// it starts (`-e`), so it ships with the app rather than the Runtime.
//
// Like the Runtime's cypher-file-references extension, the block goes at the
// front of Pi's appended system prompt (the `<addendum>` section) on every
// turn, ahead of the user's APPEND_SYSTEM.md text, which it never writes to.
// A custom prompt section would replay out of order once it first appeared
// in an older session, which pi-claude-bridge refuses; the addendum keeps its
// place and reaches every model. The text never changes, so the prompt stays
// cache-stable across turns.

export const CHAT_RENDERING = `<cypher_rendering>
Your replies are shown in Cypher, which renders GitHub-flavored Markdown, LaTeX math ($…$ inline, $$…$$ display), Mermaid diagrams and artifacts.
- When a diagram explains something better than prose — a flow or decision, calls between components, a state machine, an architecture or data model, a timeline — draw it in a \`\`\`mermaid fenced block instead of ASCII art.
- Keep each diagram small and focused; split a large one. Quote labels that contain punctuation: A["Parse (fast path)"].
- Don't add a diagram where a sentence or a short list does the job.
- When seeing or trying something helps more than a diagram can (a simulation, a chart, a small interactive demo), write it as an artifact: the complete page in one fenced block tagged \`\`\`html artifact title="Short title" (or svg artifact for a standalone SVG). The user's app shows a card that opens the page.
- Make an artifact self-contained (inline CSS and JavaScript, libraries only from an https CDN); it can't read local files or the project. To revise one, write the whole page again under the same title. Keep your explanation outside the fence, and put code the user should keep in files, not artifacts.
</cypher_rendering>`;

/** The appended prompt with Cypher's block in front of the rest. */
export function withChatRendering(append: string): string {
  if (append.includes(CHAT_RENDERING)) return append;
  const rest = append.trim();
  return rest ? `${CHAT_RENDERING}\n\n${rest}` : CHAT_RENDERING;
}

type BeforeAgentStart = {
  systemPromptOptions?: { appendSystemPrompt?: string };
};

export default function chatRendering(pi: {
  on(event: "before_agent_start", handler: (event: BeforeAgentStart) => void): void;
}) {
  pi.on("before_agent_start", (event) => {
    // Runtimes without mutable prompt options run on without the guidance.
    const options = event.systemPromptOptions;
    if (options) options.appendSystemPrompt = withChatRendering(options.appendSystemPrompt ?? "");
  });
}
