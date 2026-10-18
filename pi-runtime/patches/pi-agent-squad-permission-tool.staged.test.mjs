// Tests for the permission-tool patch: every `--tools` allowlist the staged
// pi-agent-squad builds for a subagent carries `request_permission` beside a
// file tool, and only there.
//
// Run against a staged runtime whose pi-agent-squad is already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const squad = join(stage, "npm", "node_modules", "pi-agent-squad");

/** Each allowlist pi-agent-squad builds: `const tools = …` through its `--tools` push. */
function allowlists(file) {
  const source = readFileSync(join(squad, file), "utf-8");
  const blocks = [];
  for (const end of source.matchAll(/^\t\tif \(tools\.length > 0\) args\.push\("--tools", tools\.join\(","\)\);$/gm)) {
    const start = source.lastIndexOf("\t\tconst tools = ", end.index);
    assert.ok(start >= 0, `${file}: an allowlist without its \`const tools\``);
    blocks.push(source.slice(start, end.index + end[0].length));
  }
  return blocks;
}

/** The `--tools` value one allowlist block builds for a profile's tools. */
function run(block, tools) {
  const args = [];
  new Function("agent", "args", block)({ tools }, args);
  return args[args.indexOf("--tools") + 1];
}

test("every subagent allowlist adds request_permission beside file tools", () => {
  const blocks = [...allowlists("spawn.ts"), ...allowlists("pool.ts")];
  assert.equal(blocks.length, 3, "spawnInteractiveSubagent, spawnSubagent and the pool");
  for (const block of blocks) {
    assert.equal(run(block, ["read", "bash"]), "read,bash,send_message,read_inbox,reply_message,request_permission");
    assert.equal(run(block, ["edit", "request_permission"]), "edit,request_permission,send_message,read_inbox,reply_message");
    assert.equal(run(block, ["bash"]), "bash,send_message,read_inbox,reply_message", "bash escalates instead");
    assert.equal(run(block, undefined), "send_message,read_inbox,reply_message");
  }
});
