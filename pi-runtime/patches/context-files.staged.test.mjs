// Tests for the context-file patches: both copies of Pi's loader read the
// cwd's AGENTS.md / CLAUDE.md and the agent dir's, never an ancestor's, and the
// patched pi-claude-bridge still loads. No network, provider or session.
//
// Run against a staged runtime whose Pi and bridge are already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");
const pi = join(modules, "@earendil-works", "pi-coding-agent");

// Keep the bridge off the real agent directory.
const scratch = realpathSync(mkdtempSync(join(tmpdir(), "pi-context-files-")));
process.env.PI_CODING_AGENT_DIR = join(scratch, "pi-agent");
after(() => rmSync(scratch, { recursive: true, force: true }));

// scratch/AGENTS.md, scratch/parent/CLAUDE.md, scratch/parent/project/AGENTS.md,
// scratch/parent/empty/ and scratch/agent/AGENTS.md.
function write(path, content) {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, content);
}
const agentDir = join(scratch, "agent");
const parent = join(scratch, "parent");
const project = join(parent, "project");
const empty = join(parent, "empty");
write(join(scratch, "AGENTS.md"), "grandparent");
write(join(parent, "CLAUDE.md"), "parent");
write(join(project, "AGENTS.md"), "project");
write(join(agentDir, "AGENTS.md"), "global");
mkdirSync(empty, { recursive: true });

const loaders = {
  "dist/index.js": (await import(pathToFileURL(join(pi, "dist", "index.js")).href)).loadProjectContextFiles,
  "dist/bundle/index.js": (await import(pathToFileURL(join(pi, "dist", "bundle", "index.js")).href)).loadProjectContextFiles,
};

for (const [entry, loadProjectContextFiles] of Object.entries(loaders)) {
  test(`${entry}: the cwd's context file follows the agent dir's, ancestors are skipped`, () => {
    const files = loadProjectContextFiles({ cwd: project, agentDir });
    assert.deepEqual(files.map((file) => file.content), ["global", "project"]);
  });

  test(`${entry}: a cwd without a context file inherits nothing from its ancestors`, () => {
    const files = loadProjectContextFiles({ cwd: empty, agentDir });
    assert.deepEqual(files.map((file) => file.content), ["global"]);
  });

  test(`${entry}: CLAUDE.md in the cwd still loads`, () => {
    const files = loadProjectContextFiles({ cwd: parent, agentDir: join(scratch, "no-agent") });
    assert.deepEqual(files.map((file) => file.content), ["parent"]);
  });
}

test("the patched bridge excludes CLAUDE.local.md and still loads", async () => {
  const entry = join(modules, "pi-claude-bridge", "src", "index.ts");
  const source = readFileSync(entry, "utf-8");
  const excludes = source.match(/^const CLAUDE_MD_EXCLUDES = .*$/m)?.[0];
  assert.equal(
    excludes,
    `const CLAUDE_MD_EXCLUDES = ["**/CLAUDE.md", "**/CLAUDE.local.md", "**/AGENTS.md", "**/.claude/rules/**"];`,
  );
  const piRequire = createRequire(join(pi, "package.json"));
  const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
  const bridge = await createJiti(import.meta.url).import(entry);
  assert.equal(typeof bridge.default, "function");
});
