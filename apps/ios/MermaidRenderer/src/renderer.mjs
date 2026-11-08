// One diagram per call: Mermaid source → SVG (laid out in this page, so text
// is measured in the font it is drawn in) → PNG for the native transcript.
// The source crosses as a callAsyncJavaScript argument, never as markup.
import mermaid from "mermaid";
import {
  MAX_SOURCE_LEN, checkPalette, rasterScale, shortError, themeVariables, viewBoxSize,
} from "./policy.mjs";

const FONT = 'system-ui, -apple-system, "Helvetica Neue", sans-serif';
const stage = () => document.getElementById("stage");
let serial = 0;

function configure(palette) {
  mermaid.initialize({
    startOnLoad: false,
    // Labels stay text: no HTML, no links, no click handlers.
    securityLevel: "strict",
    theme: "base",
    darkMode: palette.dark === true,
    themeVariables: themeVariables(palette, FONT),
    fontFamily: FONT,
    // A canvas can draw SVG text but not HTML labels.
    htmlLabels: false,
    flowchart: { htmlLabels: false },
    class: { htmlLabels: false },
    // ELK is not bundled (it is EPL-2.0); Mermaid falls back to dagre.
    layout: "dagre",
    look: "classic",
    maxTextSize: MAX_SOURCE_LEN,
    logLevel: 5,
  });
}

async function rasterize(svgText, scale, background) {
  const doc = new DOMParser().parseFromString(svgText, "image/svg+xml");
  const root = doc.documentElement;
  if (root.nodeName !== "svg") throw new Error("Mermaid produced no SVG");
  const size = viewBoxSize(root.getAttribute("viewBox"));
  if (!size) throw new Error("The diagram has no size");
  // An <img> sizes the SVG from these, not from the page's CSS.
  root.setAttribute("width", String(size.width));
  root.setAttribute("height", String(size.height));
  root.removeAttribute("style");
  const blob = new Blob([new XMLSerializer().serializeToString(root)], { type: "image/svg+xml" });
  const url = URL.createObjectURL(blob);
  try {
    const image = new Image();
    image.src = url;
    await image.decode();
    const factor = rasterScale(size.width, size.height, scale);
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(size.width * factor));
    canvas.height = Math.max(1, Math.round(size.height * factor));
    const context = canvas.getContext("2d");
    // The panel's own fill: Mermaid's translucent label plates then blend
    // exactly as they would over the panel.
    context.fillStyle = background;
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.scale(canvas.width / size.width, canvas.height / size.height);
    context.drawImage(image, 0, 0, size.width, size.height);
    const png = canvas.toDataURL("image/png");
    return { png: png.slice(png.indexOf(",") + 1), width: size.width, height: size.height };
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** `{ png, width, height }` (base64 PNG; natural size in points) or `{ error }`. */
async function render(input) {
  const id = `cypher-mermaid-${++serial}`;
  try {
    const source = typeof input?.source === "string" ? input.source : "";
    if (!source.trim()) return { error: "Empty diagram" };
    if (source.length > MAX_SOURCE_LEN) return { error: "Diagram source is too long to draw" };
    const scale = Number(input.scale) || 2;
    const palette = checkPalette(input.palette);
    configure(palette);
    const { svg } = await mermaid.render(id, source, stage());
    return await rasterize(svg, scale, palette.canvas);
  } catch (error) {
    return { error: shortError(error) };
  } finally {
    // Mermaid leaves its scratch nodes (and its error bomb) behind.
    document.getElementById(id)?.remove();
    document.getElementById(`d${id}`)?.remove();
    stage().replaceChildren();
  }
}

window.cypherMermaid = Object.freeze({ render });
