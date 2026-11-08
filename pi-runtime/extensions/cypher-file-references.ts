/**
 * File references: Cypher's default instructions for how a model writes the
 * files it mentions — inline code, relative to the working directory, with
 * an optional `:42` or `:42-58` line suffix. The desktop transcript turns
 * exactly that shape into a chip that previews and opens the file
 * (crates/ui/src/markdown/code_ref.rs), so a reply that follows it links
 * every file it names; one that writes bare names or paths relative to some
 * other folder leaves the app guessing.
 *
 * The block goes at the front of Pi's appended system prompt (the
 * `<addendum>` section) on every turn, ahead of the user's own
 * `APPEND_SYSTEM.md` text, which it never writes to. Of the places an
 * extension can reach, the addendum is the one every model gets:
 * - it keeps its place in the prompt, where a custom section would not —
 *   a transcript replays sections in the order they first appeared, and
 *   pi-claude-bridge refuses a prompt whose custom sections replay out of
 *   the builder's order, which a section first added to an older session
 *   could cause;
 * - it is rendered when a subagent replaces the base prompt, and the bridge
 *   forwards it to Claude Code (Pi's own rules section is not).
 * The text never changes, so the prompt stays cache-stable across turns.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const FILE_REFERENCES = `<file_references>
When your reply mentions a file, write its path as inline code; the user's app turns it into a link that previews and opens the file.
- A file: \`src/app/main.py\`. Specific lines: \`src/app/main.py:42\` or \`src/app/main.py:42-58\`. A folder: \`src/app/\`.
- Paths are relative to your working directory; use an absolute path only for a file outside it.
- Write the whole path every time, also when you mention the same file again: never just the file name, never a path relative to some other folder.
- One path per inline code span, with nothing else in it. Give lines only as the \`:42\` or \`:42-58\` suffix, not as "line 42", \`#L42\` or a Markdown link.
- This is for files and folders that exist or that you created. Commands, identifiers, globs, and example or placeholder paths stay ordinary inline code.
</file_references>`;

/** The appended prompt with Cypher's block in front of the user's text. */
export function withFileReferences(append: string): string {
  if (append.includes(FILE_REFERENCES)) return append;
  const own = append.trim();
  return own ? `${FILE_REFERENCES}\n\n${own}` : FILE_REFERENCES;
}

export default function fileReferences(pi: ExtensionAPI) {
  pi.on("before_agent_start", (event) => {
    const options = event.systemPromptOptions;
    options.appendSystemPrompt = withFileReferences(options.appendSystemPrompt ?? "");
  });
}
