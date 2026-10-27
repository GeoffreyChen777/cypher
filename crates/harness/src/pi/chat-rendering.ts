// Tells the model what Cypher's chat renders, so it reaches for a diagram when
// one explains better than prose. The engine writes this file beside its
// engine client and loads it into every Pi it starts (`-e`), so it ships with
// the app rather than the Runtime and leaves the user's APPEND_SYSTEM.md alone.
//
// It adds its own prompt section rather than editing the prompt text: the
// section is identical every turn, so it never invalidates a cached prefix.

const SECTION = "cypher_rendering";

export const GUIDANCE = `Your replies are shown in Cypher, which renders GitHub-flavored Markdown, LaTeX math ($…$ inline, $$…$$ display) and Mermaid diagrams.
- When a diagram explains something better than prose — a flow or decision, calls between components, a state machine, an architecture or data model, a timeline — draw it in a \`\`\`mermaid fenced block instead of ASCII art.
- Keep each diagram small and focused; split a large one. Quote labels that contain punctuation: A["Parse (fast path)"].
- Don't add a diagram where a sentence or a short list does the job.`;

type BeforeAgentStart = {
  systemPromptOptions?: { sections?: Record<string, string> };
};

export default function chatRendering(pi: {
  on(event: "before_agent_start", handler: (event: BeforeAgentStart) => void): void;
}) {
  pi.on("before_agent_start", (event) => {
    // Runtimes without prompt sections run on without the guidance.
    const sections = event.systemPromptOptions?.sections;
    if (sections) sections[SECTION] = GUIDANCE;
  });
}
