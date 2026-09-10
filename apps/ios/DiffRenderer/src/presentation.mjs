// App-owned, offline presentation. Repository data never supplies SVG or CSS.
const paths = {
  chevron: "m7.5 5 5 5-5 5",
  file: "M11.5 2.5h-6a1 1 0 0 0-1 1v13a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1v-10zm0 0v4h4M7.5 10h5m-5 3h5",
  info: "M10 9v5m0-8v.01M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0",
  loading: "M18 10a8 8 0 1 1-8-8",
  binary: "m10 2 7 4v8l-7 4-7-4V6zm-7 4 7 4 7-4m-7 4v8",
};

export function icon(name, className = "") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  for (const [key, value] of Object.entries({
    viewBox: "0 0 20 20", width: "20", height: "20", fill: "none",
    stroke: "currentColor", "stroke-width": "1.5", "stroke-linecap": "round",
    "stroke-linejoin": "round", "aria-hidden": "true", focusable: "false", class: className,
  })) svg.setAttribute(key, value);
  const path = document.createElementNS(svg.namespaceURI, "path");
  path.setAttribute("d", paths[name] ?? paths.info);
  svg.append(path);
  return svg;
}

export function notice(node, text, kind = "info", action) {
  node.replaceChildren();
  node.dataset.state = kind;
  const label = document.createElement("span");
  label.className = "notice-label";
  label.textContent = text;
  node.append(icon(kind === "loading" ? "loading" : "info", "notice-icon"), label);
  if (action) {
    const button = document.createElement("button");
    button.textContent = action.title;
    button.onclick = action.run;
    node.append(button);
  }
}

export function fileState(node, title, detail, kind = "info") {
  node.replaceChildren();
  const row = document.createElement("div");
  row.className = "file-state";
  const text = document.createElement("div");
  const heading = document.createElement("div");
  heading.className = "state-title";
  heading.textContent = title;
  const caption = document.createElement("div");
  caption.className = "state-detail";
  caption.textContent = detail;
  text.append(heading, caption);
  row.append(icon(kind), text);
  node.append(row);
}

const compact = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });
export const countText = value => value >= 10_000 ? compact.format(value) : String(value ?? 0);

export function languageLabel(lang) {
  return ({
    swift: "Swift", rust: "Rust", typescript: "TypeScript", tsx: "TSX",
    javascript: "JavaScript", jsx: "JSX", python: "Python", go: "Go",
    json: "JSON", css: "CSS", html: "HTML", shellscript: "Shell",
    yaml: "YAML", toml: "TOML", c: "C", cpp: "C++", "objective-c": "Objective-C",
  })[lang] ?? "Plain text";
}

export const sourceCSS = `
  [data-content], [data-line] { -webkit-user-select: text; user-select: text; -webkit-touch-callout: default; }
  [data-gutter], [data-column-number] { -webkit-user-select: none; user-select: none; }
`;

// Muted syntax colors follow the native reader's warm keywords, green strings
// and ochre numbers. Token scopes remain distinct in both appearances.
export function theme(dark) {
  const colors = dark
    ? ["#dedede", "#060606", "#d9958d", "#8cc8ae", "#d3b67b", "#9fb8d5", "#b5a3d4", "#898989"]
    : ["#292929", "#ffffff", "#a34940", "#316d58", "#82621f", "#426589", "#765991", "#757575"];
  const [fg, bg, keyword, string, number, callable, type, comment] = colors;
  return {
    name: dark ? "cypher-dark" : "cypher-light", type: dark ? "dark" : "light", fg, bg,
    colors: {
      "editor.background": bg, "editor.foreground": fg,
      "gitDecoration.addedResourceForeground": dark ? "#7fc3a0" : "#28744f",
      "gitDecoration.deletedResourceForeground": dark ? "#db9693" : "#b34d48",
      "gitDecoration.modifiedResourceForeground": callable,
    },
    tokenColors: [
      { scope: ["keyword", "storage"], settings: { foreground: keyword } },
      { scope: ["string", "constant.other.symbol"], settings: { foreground: string } },
      { scope: ["constant.numeric", "constant.language"], settings: { foreground: number } },
      { scope: ["entity.name.function", "support.function"], settings: { foreground: callable } },
      { scope: ["entity.name.type", "support.type", "support.class"], settings: { foreground: type } },
      { scope: ["keyword.operator", "punctuation"], settings: { foreground: fg } },
      { scope: ["comment", "punctuation.definition.comment"], settings: { foreground: comment } },
    ],
  };
}

// Shadow-DOM styles must remain static. Keep Pierre's 32px separator geometry
// and 20px code rows intact so the virtualizer's estimates stay accurate.
export const diffCSS = `
  :host {
    --diffs-fg-number-override: light-dark(#787878, #929292);
    --diffs-bg-separator-override: light-dark(#f5f5f5, #1a1a1a);
    --diffs-bg-addition-emphasis-override: light-dark(#d2e9da, #274b35);
    --diffs-bg-deletion-emphasis-override: light-dark(#f0d4d1, #57302e);
  }
  [data-separator-wrapper] { font: 11px -apple-system, BlinkMacSystemFont, sans-serif; }
  [data-expand-button] { border-right: 1px solid light-dark(#e9e9e9, #2b2b2b); }
  [data-expand-button] [data-icon] { width: 14px; height: 14px; }
  [data-expand-button]:focus-visible { outline: 2px solid light-dark(#737373, #aaa); outline-offset: -2px; }
  [data-separator-content] { padding-inline: 10px; }
  [data-line-number] { font-variant-numeric: tabular-nums; }
  [data-additions] { box-shadow: -1px 0 light-dark(#e9e9e9, #272727); }
`;
