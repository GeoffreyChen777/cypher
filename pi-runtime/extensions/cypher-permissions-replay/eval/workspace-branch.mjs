// The branch a case's folder had checked out, from git's own output in the conversation before the
// call: what production reads from git at the chat's folders (core.ts `workspaceBranches`), which the
// fake machine's repositories (all on main) can't know. Evidence is only git's output of a command
// that ran in the case's folder (no `cd` or `-C` elsewhere): `On branch X`, `## X...` (status -sb),
// `git branch --show-current` / `rev-parse --abbrev-ref HEAD` printing a branch. The latest wins.
// Never the agent's words, never a branch the call itself names.

const ON_BRANCH = /^On branch ([^\s]+)$/m;
const SHORT_STATUS = /^## ([^\s.][^\s]*?)(?:\.\.\.\S+)?(?: \[.*\])?$/m;
const SHOW_CURRENT = /\bgit\b[^|;&]*\b(branch --show-current|rev-parse --abbrev-ref HEAD|symbolic-ref --short HEAD)\b/;

function textOf(content) {
  if (typeof content === "string") return content;
  return (content ?? []).filter((part) => part?.type === "text").map((part) => part.text).join("\n");
}

/** Whether a command ran in `cwd`: it names no other directory to run in. */
function inFolder(command, cwd) {
  for (const match of command.matchAll(/(?:^|[;&|(]\s*)cd\s+("[^"]+"|'[^']+'|\S+)/g)) {
    const dir = match[1].replace(/^["']|["']$/g, "").replace(/\/+$/, "");
    if (dir !== cwd.replace(/\/+$/, "")) return false;
  }
  for (const match of command.matchAll(/\bgit\s+-C\s+("[^"]+"|'[^']+'|\S+)/g)) {
    const dir = match[1].replace(/^["']|["']$/g, "").replace(/\/+$/, "");
    if (dir !== cwd.replace(/\/+$/, "") && dir !== ".") return false;
  }
  return true;
}

export function workspaceBranchOf(branch, cwd) {
  if (!Array.isArray(branch) || !cwd) return undefined;
  const calls = new Map();
  let found;
  for (const entry of branch) {
    const message = entry?.message;
    if (!message) continue;
    if (message.role === "assistant" && Array.isArray(message.content)) {
      for (const part of message.content) if (part?.type === "toolCall" && part.name === "bash" && typeof part.arguments?.command === "string") calls.set(part.id, part.arguments.command);
    } else if (message.role === "toolResult") {
      const command = calls.get(message.toolCallId);
      if (!command || !/\bgit\b/.test(command) || !inFolder(command, cwd)) continue;
      const text = textOf(message.content);
      let name = text.match(ON_BRANCH)?.[1];
      if (!name && /\bgit\b[^|;&]*\bstatus\b[^|;&]*(-sb|-bs|--branch)/.test(command)) name = text.match(SHORT_STATUS)?.[1];
      if (!name && SHOW_CURRENT.test(command)) {
        const lines = text.split("\n").map((line) => line.trim()).filter(Boolean);
        if (lines.length >= 1 && /^[\w./-]+$/.test(lines[0]) && lines[0] !== "HEAD") name = lines[0];
      }
      if (name && name !== "HEAD" && !/^(main|master)$/.test(name)) found = name;
      else if (name) found = undefined;
    }
  }
  return found;
}
