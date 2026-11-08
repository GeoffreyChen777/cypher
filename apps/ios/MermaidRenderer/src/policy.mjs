// Input bounds and palette mapping, kept free of the DOM so Node tests cover them.

/** Longer sources are not drawn (desktop: markdown/mermaid.rs MAX_SOURCE_LEN). */
export const MAX_SOURCE_LEN = 64 * 1024;
/** Longest raster side in device pixels. */
export const MAX_RASTER_PX = 4096;
/** Longest error kept for the fence header. */
export const MAX_ERROR_LEN = 160;

const HEX = /^#[0-9a-f]{6}$/i;
const ROLES = [
  "canvas", "surface", "surfaceAlt", "surfaceMuted", "text", "textMuted", "border",
  "line", "noteBackground", "noteBorder", "danger", "warning", "success",
];

/** The native palette, validated: every role a `#rrggbb`, six series colors. */
export function checkPalette(palette) {
  if (!palette || typeof palette !== "object") throw new Error("palette");
  for (const role of ROLES) {
    if (typeof palette[role] !== "string" || !HEX.test(palette[role])) throw new Error(`palette.${role}`);
  }
  if (!Array.isArray(palette.series) || palette.series.length !== 6
    || !palette.series.every(color => typeof color === "string" && HEX.test(color))) {
    throw new Error("palette.series");
  }
  return palette;
}

/** Mermaid `base` theme variables from the chat palette (the roles desktop
 *  hands Merman, mapped onto Mermaid's own names). */
export function themeVariables(p, fontFamily) {
  const vars = {
    darkMode: p.dark === true,
    fontFamily,
    background: p.canvas,
    primaryColor: p.surface,
    primaryTextColor: p.text,
    primaryBorderColor: p.border,
    secondaryColor: p.surfaceAlt,
    secondaryTextColor: p.text,
    secondaryBorderColor: p.border,
    tertiaryColor: p.surfaceMuted,
    tertiaryTextColor: p.text,
    tertiaryBorderColor: p.border,
    mainBkg: p.surface,
    nodeBorder: p.border,
    nodeTextColor: p.text,
    textColor: p.text,
    titleColor: p.text,
    lineColor: p.line,
    edgeLabelBackground: p.canvas,
    clusterBkg: p.surfaceMuted,
    clusterBorder: p.border,
    noteBkgColor: p.noteBackground,
    noteBorderColor: p.noteBorder,
    noteTextColor: p.text,
    actorBkg: p.surface,
    actorBorder: p.border,
    actorTextColor: p.text,
    actorLineColor: p.line,
    signalColor: p.text,
    signalTextColor: p.text,
    labelBoxBkgColor: p.surface,
    labelBoxBorderColor: p.border,
    labelTextColor: p.text,
    loopTextColor: p.text,
    activationBkgColor: p.surfaceAlt,
    activationBorderColor: p.border,
    sequenceNumberColor: p.canvas,
    classText: p.text,
    labelColor: p.text,
    errorBkgColor: p.danger,
    errorTextColor: p.canvas,
    pieTitleTextColor: p.text,
    pieSectionTextColor: p.canvas,
    pieLegendTextColor: p.text,
    pieStrokeColor: p.canvas,
    pieOuterStrokeColor: p.border,
    attributeBackgroundColorOdd: p.surfaceMuted,
    attributeBackgroundColorEven: p.surface,
    git0: p.series[0], git1: p.series[1], git2: p.series[2], git3: p.series[3],
    git4: p.series[4], git5: p.series[5], git6: p.series[0], git7: p.series[1],
    gitBranchLabel0: p.canvas, gitBranchLabel1: p.canvas, gitBranchLabel2: p.canvas,
    gitBranchLabel3: p.canvas, gitBranchLabel4: p.canvas, gitBranchLabel5: p.canvas,
    gitBranchLabel6: p.canvas, gitBranchLabel7: p.canvas,
  };
  p.series.forEach((color, ix) => {
    vars[`pie${ix + 1}`] = color;
    vars[`cScale${ix}`] = color;
    vars[`cScaleLabel${ix}`] = p.canvas;
  });
  return vars;
}

/** The first line of an error, bounded for a one-line header. */
export function shortError(error) {
  const text = String(error?.message ?? error ?? "Could not draw the diagram");
  let line = (text.split("\n").find(part => part.trim()) ?? text).trim();
  // Mermaid's parser puts the position first and what it hit last
  // ("Parse error on line 3:" … "Expecting 'A', 'B', got 'EOF'").
  const got = /got '([^']+)'\s*$/m.exec(text);
  if (line.endsWith(":") && got) {
    line += got[1] === "EOF" ? " unexpected end of input" : ` unexpected '${got[1]}'`;
  }
  const chars = Array.from(line);
  return chars.length > MAX_ERROR_LEN ? chars.slice(0, MAX_ERROR_LEN).join("") + "…" : line;
}

/** Raster scale: the device scale, reduced so neither side passes the cap. */
export function rasterScale(width, height, scale) {
  if (!(width > 0 && height > 0 && scale > 0)) return 0;
  return Math.max(0.1, Math.min(scale, MAX_RASTER_PX / width, MAX_RASTER_PX / height));
}

/** The root viewBox's size, or null. */
export function viewBoxSize(viewBox) {
  const parts = String(viewBox ?? "").trim().split(/[\s,]+/).map(Number);
  if (parts.length !== 4 || !parts.every(Number.isFinite)) return null;
  const [, , width, height] = parts;
  return width > 0 && height > 0 ? { width, height } : null;
}
