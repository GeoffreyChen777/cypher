import test from "node:test";
import assert from "node:assert/strict";
import {
  MAX_ERROR_LEN, MAX_RASTER_PX, checkPalette, rasterScale, shortError, themeVariables, viewBoxSize,
} from "../src/policy.mjs";

const palette = {
  dark: true, canvas: "#1a1a1a", surface: "#262626", surfaceAlt: "#303030", surfaceMuted: "#202020",
  text: "#e7e7e7", textMuted: "#a0a0a0", border: "#454545", line: "#a0a0a0",
  noteBackground: "#3a3020", noteBorder: "#a08040", danger: "#ff6060", warning: "#f0c040",
  success: "#40d0a0", series: ["#818cf8", "#34d399", "#f59e0b", "#f472b6", "#60a5fa", "#f87171"],
};

test("palettes must be plain hex colors", () => {
  assert.equal(checkPalette(palette), palette);
  assert.throws(() => checkPalette({ ...palette, text: "red;}</style><script>" }), /palette.text/);
  assert.throws(() => checkPalette({ ...palette, series: palette.series.slice(1) }), /series/);
  assert.throws(() => checkPalette(null));
});

test("theme variables carry the palette", () => {
  const vars = themeVariables(palette, "system-ui");
  assert.equal(vars.darkMode, true);
  assert.equal(vars.primaryTextColor, palette.text);
  assert.equal(vars.pie1, palette.series[0]);
  assert.equal(vars.cScale5, palette.series[5]);
  assert.ok(Object.values(vars).every(value => typeof value !== "string" || !value.includes("<")));
});

test("errors are one bounded line", () => {
  assert.equal(shortError(new Error("Parse error on line 2:\n...A -->\n-----^")), "Parse error on line 2:");
  assert.equal(
    shortError(new Error("Parse error on line 3:\n...A -->\n-----^\nExpecting 'AMP', 'COLON', got 'EOF'")),
    "Parse error on line 3: unexpected end of input");
  assert.equal(
    shortError(new Error("Parse error on line 1:\nx\n^\nExpecting 'NODE', got 'PIPE'")),
    "Parse error on line 1: unexpected 'PIPE'");
  assert.equal(Array.from(shortError("x".repeat(400))).length, MAX_ERROR_LEN + 1);
  assert.equal(shortError(undefined), "Could not draw the diagram");
});

test("rasters stay under the cap", () => {
  assert.equal(rasterScale(300, 200, 3), 3);
  assert.equal(rasterScale(4000, 100, 3), MAX_RASTER_PX / 4000);
  assert.equal(rasterScale(0, 10, 2), 0);
});

test("viewBox sizes parse", () => {
  assert.deepEqual(viewBoxSize("-8 -8 335.5 399"), { width: 335.5, height: 399 });
  assert.deepEqual(viewBoxSize("0,0,10,20"), { width: 10, height: 20 });
  assert.equal(viewBoxSize("0 0 0 20"), null);
  assert.equal(viewBoxSize(null), null);
});
