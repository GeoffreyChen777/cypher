import { build } from "esbuild";
import { readFile, writeFile, mkdir, readdir, copyFile, rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createHash } from "node:crypto";

const cwd = path.dirname(fileURLToPath(import.meta.url));
const output = path.resolve(cwd, "../Cypher/Resources/MermaidRenderer.bundle");
const version = JSON.parse(await readFile(path.join(cwd, "node_modules/mermaid/package.json"), "utf8")).version;

// ELK is EPL-2.0 (desktop builds Merman without it too). Drop Mermaid's ELK
// layout loaders, so diagrams that ask for ELK take Mermaid's own fallback to
// dagre, and stub elkjs so no ELK code can be bundled. Each anchor must match
// exactly once: a Mermaid update that moves it fails the build here.
const ANCHORS = [{ from: "...elkLayoutLoaders()", to: "" }];
const patched = new Map(ANCHORS.map(anchor => [anchor.from, 0]));
const withoutElk = {
  name: "without-elk",
  setup(build) {
    build.onResolve({ filter: /^elkjs(\/|$)/ }, () => ({ path: "elkjs", namespace: "no-elk" }));
    build.onLoad({ filter: /.*/, namespace: "no-elk" }, () => ({
      contents: "export default class { constructor() { throw new Error('ELK layout is not included'); } }",
    }));
    build.onLoad({ filter: /node_modules\/mermaid\/dist\/.*\.mjs$/ }, async args => {
      let contents = await readFile(args.path, "utf8");
      for (const { from, to } of ANCHORS) {
        const count = contents.split(from).length - 1;
        if (count === 0) continue;
        patched.set(from, patched.get(from) + count);
        contents = contents.replaceAll(from, to);
      }
      return { contents, loader: "js" };
    });
  },
};

const result = await build({
  absWorkingDir: cwd, entryPoints: ["src/renderer.mjs"], bundle: true, minify: true,
  format: "iife", platform: "browser", target: "safari17", write: false,
  legalComments: "inline", define: { "process.env.NODE_ENV": '"production"' },
  plugins: [withoutElk],
});
for (const [anchor, count] of patched) {
  if (count !== 1) throw new Error(`ELK anchor ${JSON.stringify(anchor)} matched ${count} times`);
}
const script = result.outputFiles[0].contents;
const text = new TextDecoder().decode(script);
for (const marker of ["elk.layered.", "org.eclipse.elk"]) {
  if (text.includes(marker)) throw new Error(`ELK code reached the bundle (${marker})`);
}

await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
await writeFile(path.join(output, "renderer.js"), script);
await copyFile(path.join(cwd, "src/index.html"), path.join(output, "index.html"));

// Redistribution notices for the bundled packages (elkjs is installed but
// never bundled, so it is left out).
const notices = [];
async function licenses(folder) {
  for (const item of (await readdir(folder, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    if (!item.isDirectory() || item.name.startsWith(".")) continue;
    const dir = path.join(folder, item.name);
    if (item.name.startsWith("@")) { await licenses(dir); continue; }
    if (item.name === "elkjs" || item.name === "esbuild") continue;
    try {
      const pkg = JSON.parse(await readFile(path.join(dir, "package.json"), "utf8"));
      const names = (await readdir(dir)).filter(name => /^(license|licence|notice|copying)(\.|$)/i.test(name));
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
  renderer: "mermaid", version,
  lockSHA256: createHash("sha256").update(lock).digest("hex"),
  scriptSHA256: createHash("sha256").update(script).digest("hex"),
}, null, 2) + "\n");
console.log(`Offline renderer: ${(script.length / 1024 / 1024).toFixed(2)} MiB (mermaid ${version}, no ELK)`);
