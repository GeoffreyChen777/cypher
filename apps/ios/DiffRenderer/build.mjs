import { build } from "esbuild";
import { readFile, writeFile, mkdir, readdir, copyFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createHash } from "node:crypto";

const cwd = path.dirname(fileURLToPath(import.meta.url));
const output = path.resolve(cwd, "../Cypher/Resources/DiffRenderer.bundle");
await mkdir(output, { recursive: true });
const common = {
  absWorkingDir: cwd, bundle: true, minify: true, format: "iife",
  platform: "browser", target: "safari17", write: false, legalComments: "inline",
  define: { "process.env.NODE_ENV": '"production"' },
};
const worker = await build({
  ...common, entryPoints: ["node_modules/@pierre/diffs/dist/worker/worker.js"],
});
const renderer = await build({
  ...common, entryPoints: ["src/renderer.mjs"],
  define: { ...common.define, __WORKER_SOURCE__: JSON.stringify(worker.outputFiles[0].text) },
  plugins: [{
    name: "bounded-shiki",
    setup(build) {
      build.onResolve({ filter: /^shiki$/ }, () => ({ path: path.join(cwd, "src/shiki.mjs") }));
    },
  }],
});
await writeFile(path.join(output, "renderer.js"), renderer.outputFiles[0].contents);
for (const file of ["index.html", "renderer.css"]) {
  await copyFile(path.join(cwd, "src", file), path.join(output, file));
}
// Preserve redistribution notices for the pinned renderer and transitive deps.
const notices = [];
async function licenses(folder) {
  for (const item of (await readdir(folder, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    if (!item.isDirectory() || item.name.startsWith(".")) continue;
    const dir = path.join(folder, item.name);
    if (item.name.startsWith("@")) { await licenses(dir); continue; }
    try {
      const pkg = JSON.parse(await readFile(path.join(dir, "package.json"), "utf8"));
      const names = (await readdir(dir)).filter(name => /^(license|notice|copying)(\.|$)/i.test(name));
      for (const name of names.sort()) {
        notices.push(`\n=== ${pkg.name} ${pkg.version} / ${name} ===\n` + await readFile(path.join(dir, name), "utf8"));
      }
    } catch (error) { if (error.code !== "ENOENT") throw error; }
  }
}
await licenses(path.join(cwd, "node_modules"));
await writeFile(path.join(output, "THIRD_PARTY_NOTICES.txt"), notices.join("\n"));
const lock = await readFile(path.join(cwd, "package-lock.json"));
await writeFile(path.join(output, "build.json"), JSON.stringify({
  renderer: "@pierre/diffs", version: "1.4.1",
  lockSHA256: createHash("sha256").update(lock).digest("hex"),
  scriptSHA256: createHash("sha256").update(renderer.outputFiles[0].contents).digest("hex"),
}, null, 2) + "\n");
console.log(`Offline renderer: ${(renderer.outputFiles[0].contents.length / 1024 / 1024).toFixed(2)} MiB`);
