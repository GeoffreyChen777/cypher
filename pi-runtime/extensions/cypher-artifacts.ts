/**
 * Artifacts: tells the model it can show the user a page (an HTML
 * explainer, an interactive demo, an SVG diagram) by writing it in a fence
 * tagged `artifact`. Cypher's desktop chat renders such a fence as a card
 * that opens the page in the side panel (crates/ui/src/markdown/artifact.rs);
 * anywhere else it reads as an ordinary `html` code block.
 *
 * The guide rides every turn as its own `<artifacts>` system prompt section,
 * which Pi renders after its built-in ones on every model (pi-claude-bridge
 * forwards custom sections to Claude Code too).
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const SECTION = "artifacts";

export const GUIDE = [
  "When a visual or interactive page would explain something better than prose (a diagram, a simulation, a chart, a small UI demo), you can show it to the user as an artifact: write the complete page in one fenced code block whose info string is `html artifact title=\"Short title\"`, or `svg artifact title=\"…\"` for a standalone SVG. The user's chat shows a card that opens the page in a side panel.",
  "- Make it self-contained: inline CSS and JavaScript, libraries only from a CDN over https. The page cannot read local files or the project.",
  "- It renders on a white page unless it sets its own background; `color-scheme: light dark` follows the user's appearance.",
  "- To revise an artifact, write the whole page again under the same title; the open panel reloads.",
  "- Keep the explanation in your reply, outside the fence. Use files (your tools) rather than artifacts for code the user should keep in the project.",
  "- Most answers need no artifact; use one when seeing or trying it helps.",
].join("\n");

export default function (pi: ExtensionAPI) {
  pi.on("before_agent_start", (event) => {
    event.systemPromptOptions.sections[SECTION] = GUIDE;
  });
}
