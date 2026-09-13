import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const root = new URL("../", import.meta.url);
const publicDir = new URL("apps/landing/public/", root);
const html = readFileSync(new URL("index.html", publicDir), "utf8");
const source = readFileSync(new URL("site.js", publicDir), "utf8");
const version = readFileSync(new URL("Cargo.toml", root), "utf8").match(/^version = "([^"]+)"/m)[1];

test("landing fallback download points at this release and gallery assets exist", () => {
  assert.ok(html.includes(`href="https://edge.letscypher.app/releases/cypher-${version}-macos-arm64.dmg"`));
  assert.ok(html.includes('src="/site.js"'));
  for (const name of ["workspace", "sessions", "diff"]) {
    assert.ok(html.includes(`data-shot="${name}"`));
    assert.ok(existsSync(new URL(`assets/app-${name}.png`, publicDir)));
  }
});

test("compact landing initializes without missing-element errors and cached screenshots stay visible", async () => {
  const classes = new Set(["is-ready"]);
  const image = {
    classList: { add: key => classes.add(key), remove: key => classes.delete(key) },
    set src(value) { this.lastSrc = value; this.onload?.(); },
  };
  const tabs = ["workspace", "sessions", "diff"].map(shot => ({
    dataset: { shot }, selected: false,
    addEventListener(event, handler) { this[event] = handler; },
    setAttribute(name, value) { if (name === "aria-selected") this.selected = value === "true"; },
  }));
  const visibleVersion = {};
  const elements = {
    "preview-image": image, "preview-description": {},
    copy: { addEventListener() { assert.fail("do not duplicate the compact layout's copy handler"); } },
    "copy-status": {},
  };
  vm.runInNewContext(source, {
    document: {
      getElementById: id => elements[id] ?? null,
      querySelector: () => null,
      querySelectorAll: selector => selector === "[data-shot]" ? tabs
        : selector === "[data-version]" ? [visibleVersion] : [],
    },
    window: {},
    fetch: async () => ({ ok: true, text: async () => version }),
  });
  for (const tab of tabs) {
    tab.click();
    assert.equal(image.lastSrc, `/assets/app-${tab.dataset.shot}.png`);
    assert.ok(classes.has("is-ready"), "cached image load must not leave the preview hidden");
    assert.equal(tabs.filter(t => t.selected).length, 1);
  }
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(visibleVersion.textContent, `v${version}`);
});
