// End-to-end tests of the permission extension in a real Pi: the staged
// Runtime's `bin/pi` in RPC mode (what the engine spawns), a scripted model
// that issues tool calls, the real Landstrip, and the patched grep and
// apply_patch. Approval dialogs are answered over RPC like Cypher's bridge.
//
// Run against a staged Runtime:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { after, test } from "node:test";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");
const landstrip = createRequire(join(modules, "@landstrip", "landstrip", "package.json"))("@landstrip/landstrip").binaryPath();
const doctor = spawnSync(landstrip, ["doctor"], { encoding: "utf8" });
const sandboxed = doctor.status === 0 && doctor.stdout.includes('"ok":true');
const skip = sandboxed ? false : `Landstrip cannot sandbox on this host: ${doctor.stdout || doctor.stderr}`;

// Not under the temp directory: every sandboxed command may write /tmp, and
// the fake home must be "outside the workspace" for real.
const scratch = realpathSync(mkdtempSync(join(homedir(), ".cypher-permissions-e2e-")));
after(() => rmSync(scratch, { recursive: true, force: true }));

const home = join(scratch, "home");
const workspace = join(home, "Developer", "app");
const other = join(home, "Developer", "other");
const agentDir = join(home, ".cypher", "pi-runtime", "agent");

function write(path, content) {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, content);
}

write(join(home, ".ssh", "id_ed25519"), "PRIVATE KEY");
write(join(workspace, "src", "main.ts"), "export const main = 1;\n");
write(join(workspace, ".env"), "API_TOKEN=hunter2\n");
write(join(workspace, "notes.md"), "API_TOKEN is read from .env\n");
write(join(other, "readme.md"), "other\n");
mkdirSync(agentDir, { recursive: true });
// Permission control is off unless the user's file turns it on.
const enabledConfig = JSON.stringify({ enabled: true });
writeFileSync(join(agentDir, "permissions.json"), enabledConfig);
spawnSync("git", ["init", "-q", workspace]);
symlinkSync(join(home, ".ssh", "id_ed25519"), join(workspace, "key-link"));

// The scripted model: tool calls from SCRIPT_FILE in order, then "done". A
// request carrying the judge's system prompt gets JUDGE_ANSWER instead.
const provider = join(scratch, "script-provider.ts");
writeFileSync(
  provider,
  `import { readFileSync } from "node:fs";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai";
export default function (pi) {
  const steps = JSON.parse(readFileSync(process.env.SCRIPT_FILE, "utf8"));
  let next = 0;
  // apply_patch runs only for grammar-capable OpenAI Responses models.
  const api = process.env.SCRIPT_API || "script-api";
  const core = createFauxCore({ api, provider: "script", models: [{ id: "m" }] });
  const respond = (context) => {
    // The system prompt travels as a system message in the transcript.
    if (JSON.stringify(context.messages ?? []).includes("You review one permission request")) {
      return fauxAssistantMessage(process.env.JUDGE_ANSWER ?? "{}");
    }
    const step = steps[next++];
    if (!step) return fauxAssistantMessage("done");
    return fauxAssistantMessage(fauxToolCall(step.tool, step.args), { stopReason: "toolUse" });
  };
  core.setResponses(Array.from({ length: 64 }, () => respond));
  pi.registerProvider("script", {
    baseUrl: "http://127.0.0.1:9", apiKey: "x", api,
    models: [{ id: "m", name: "m", reasoning: false, input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 100000, maxTokens: 1000, compat: { supportsOpenAIGrammarTools: true } }],
    streamSimple: core.streamSimple,
  });
}
`,
);

/**
 * Run one turn. `answers` are picked, in order, for the approval dialogs.
 * Returns the tool results by call order and the dialogs that were shown.
 */
async function turn(steps, { answers = [], level = "workspace-write", judge, extraExtensions = [], api } = {}) {
  const scriptFile = join(scratch, `script-${Math.random().toString(36).slice(2)}.json`);
  writeFileSync(scriptFile, JSON.stringify(steps));
  const child = spawn(
    join(stage, "bin", "pi"),
    [
      "--mode", "rpc", "--no-session", "--no-extensions",
      "-e", provider,
      "-e", join(stage, "extensions", "cypher-permissions.ts"),
      ...extraExtensions.flatMap((path) => ["-e", path]),
      "--model", "script/m",
    ],
    {
      cwd: workspace,
      env: {
        PATH: process.env.PATH,
        HOME: home,
        PI_CODING_AGENT_DIR: agentDir,
        PI_PACKAGE_DIR: join(stage, "pi"),
        SCRIPT_FILE: scriptFile,
        JUDGE_ANSWER: judge ?? "",
        SCRIPT_API: api ?? "",
        CYPHER_PERMISSION_LEVEL: level,
        CYPHER_CHAT_ID: "chat-e2e",
        GITHUB_TOKEN: "ghp_secret",
      },
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  let stderr = "";
  child.stderr.on("data", (chunk) => (stderr += chunk));
  const results = [];
  const dialogs = [];
  const pending = [...answers];
  const done = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`turn timed out\n${stderr}`)), 60_000);
    createInterface({ input: child.stdout }).on("line", (line) => {
      let event;
      try {
        event = JSON.parse(line);
      } catch {
        return;
      }
      if (event.type === "extension_ui_request" && event.method === "select") {
        dialogs.push({ title: event.title, options: event.options });
        const answer = pending.shift() ?? "Deny";
        child.stdin.write(`${JSON.stringify({ type: "extension_ui_response", id: event.id, value: answer })}\n`);
      } else if (event.type === "tool_execution_end") {
        const text = (event.result?.content ?? []).map((part) => part.text ?? "").join("\n");
        results.push({ tool: event.toolName, isError: event.isError, text, record: event.result?.details?.cypherPermissions });
      } else if (event.type === "agent_end") {
        clearTimeout(timer);
        resolve();
      }
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`pi exited (${code}) before the turn ended\n${stderr}`));
    });
  });
  child.stdin.write(`${JSON.stringify({ type: "prompt", message: "please do the task" })}\n`);
  try {
    await done;
  } finally {
    child.removeAllListeners("exit");
    child.kill();
  }
  return { results, dialogs };
}

test("bash runs sandboxed: the workspace is writable, secrets and the outside are not", { skip }, async () => {
  const { results } = await turn([
    {
      tool: "bash",
      args: {
        command:
          "echo built > out.txt && cat out.txt; cat .env; cat ~/.ssh/id_ed25519; echo x > ../other/new.txt; git config core.fsmonitor 'touch /tmp/pwned'; mv .git .git-moved; echo \"token=${GITHUB_TOKEN:-scrubbed}\"",
      },
    },
  ]);
  const [bash] = results;
  assert.match(bash.text, /^built$/m);
  assert.doesNotMatch(bash.text, /hunter2|PRIVATE KEY/);
  assert.match(bash.text, /token=scrubbed/);
  assert.ok(!existsSync(join(other, "new.txt")));
  assert.ok(existsSync(join(workspace, ".git")), ".git cannot be renamed away");
  assert.doesNotMatch(readFileSync(join(workspace, ".git", "config"), "utf8"), /fsmonitor/);
  assert.equal(readFileSync(join(workspace, "out.txt"), "utf8"), "built\n");
});

test("a command that needs more fails in the sandbox; escalated and approved, it runs outside", { skip }, async () => {
  const command = "echo outside > ../other/outside.txt && cat ~/.ssh/id_ed25519 | head -c 11";
  const { results, dialogs } = await turn(
    [
      { tool: "bash", args: { command } },
      { tool: "bash", args: { command, escalate: { label: "Update the other repo", reason: "the user asked", patterns: [["head", "**"], ["cat", "**"]] } } },
    ],
    { answers: ["Run once"] },
  );
  assert.equal(results[0].isError, true);
  assert.match(results[0].text, /run it again with `escalate`/);
  assert.equal(dialogs.length, 1);
  assert.match(dialogs[0].title, /^Run this command outside the sandbox\?\n\nContext:\nPurpose: Update the other repo\n/);
  assert.equal(results[1].isError, false, results[1].text);
  assert.match(results[1].text, /PRIVATE KEY/);
  assert.match(results[1].text, /Ran outside the sandbox \(approved once; it could not be allowed for the chat, because it writes ~\/Developer\/other\/outside\.txt, which commands in the sandbox can't\)\./);
  assert.equal(readFileSync(join(other, "outside.txt"), "utf8"), "outside\n");
});

test("allowed for the chat, matching commands run outside without asking; others still ask", { skip }, async () => {
  const { results, dialogs } = await turn(
    [
      { tool: "bash", args: { command: "cat ~/.ssh/id_ed25519", escalate: { label: "Read the key", reason: "x", patterns: [["cat", "**/.ssh/*"]] } } },
      { tool: "bash", args: { command: "cat ~/.ssh/id_ed25519" } },
      { tool: "bash", args: { command: "cat .env", escalate: { label: "Read the env", reason: "x" } } },
    ],
    { answers: ["Allow for this chat", "Deny"] },
  );
  assert.equal(dialogs.length, 2, "the second command matched the pattern");
  assert.match(results[0].text, /PRIVATE KEY/);
  assert.match(results[1].text, /PRIVATE KEY/);
  assert.match(results[1].text, /allowed by `cat \*\*\/\.ssh\/\*`/);
  assert.equal(results[2].isError, true);
  assert.doesNotMatch(results[2].text, /hunter2/);
});

test("a deny rule blocks a command before it runs, escalated or not", { skip }, async () => {
  const config = join(agentDir, "permissions.json");
  writeFileSync(config, JSON.stringify({ enabled: true, commands: [{ match: ["touch", "**"], decision: "deny" }] }));
  try {
    const { results, dialogs } = await turn([
      { tool: "bash", args: { command: "touch never.txt" } },
      { tool: "bash", args: { command: "touch never.txt", escalate: { label: "x", reason: "x" } } },
    ]);
    assert.equal(dialogs.length, 0);
    for (const result of results) {
      assert.match(result.text, /set to never run/);
      assert.equal(result.record.outcome, "denied");
    }
    assert.ok(!existsSync(join(workspace, "never.txt")));
  } finally {
    writeFileSync(config, enabledConfig);
  }
});

test("file tools: a sensitive read asks a human; symlinks resolve to their target", async () => {
  const { results, dialogs } = await turn(
    [
      { tool: "read", args: { path: ".env" } },
      { tool: "read", args: { path: ".env" } },
      { tool: "read", args: { path: "key-link" } },
      { tool: "read", args: { path: "src/main.ts" } },
    ],
    { answers: ["Deny", "Allow once", "Deny"] },
  );
  assert.equal(results[0].isError, true);
  assert.match(results[1].text, /hunter2/);
  assert.equal(results[2].isError, true);
  assert.doesNotMatch(results[2].text, /PRIVATE KEY/);
  assert.match(results[3].text, /main = 1/);
  assert.equal(dialogs.length, 3);
  assert.match(dialogs[2].title, /\.ssh\/id_ed25519/);
});

test("assisted: the judge approves an external write without a dialog; protected writes it escalates go to the human", async () => {
  const allowed = await turn(
    [{ tool: "write", args: { path: join(other, "from-judge.md"), content: "ok\n" } }],
    { judge: '{"decision":"approve","reason":"the user asked for it","confidence":0.95}' },
  );
  assert.equal(allowed.dialogs.length, 0);
  assert.equal(allowed.results[0].isError, false);
  assert.equal(readFileSync(join(other, "from-judge.md"), "utf8"), "ok\n");

  const escalated = await turn(
    [{ tool: "edit", args: { path: ".git/config", edits: [{ oldText: "[core]", newText: "[core]\n\tfsmonitor = touch /tmp/pwned" }] } }],
    { judge: '{"decision":"ask-user","reason":"fsmonitor runs a program","confidence":0.9}', answers: ["Deny"] },
  );
  assert.equal(escalated.dialogs.length, 1);
  assert.match(escalated.dialogs[0].title, /fsmonitor/);
  assert.equal(escalated.results[0].isError, true);
  assert.doesNotMatch(readFileSync(join(workspace, ".git", "config"), "utf8"), /fsmonitor/);
});

test("grep never shows a sensitive file's lines", { skip }, async () => {
  const { results } = await turn([{ tool: "grep", args: { pattern: "API_TOKEN" } }]);
  assert.match(results[0].text, /notes\.md/);
  assert.doesNotMatch(results[0].text, /hunter2/);
  assert.doesNotMatch(results[0].text, /^\.env:/m);
});

test("apply_patch goes through the same checks", async () => {
  const codex = join(modules, "pi-codex-tools", "extensions", "index.ts");
  const patch = (path) => `*** Begin Patch\n*** Add File: ${path}\n+patched\n*** End Patch`;
  const { results, dialogs } = await turn(
    [
      { tool: "apply_patch", args: { patch: patch("src/added.ts") } },
      { tool: "apply_patch", args: { patch: patch(join(other, "patched.md")) } },
    ],
    { extraExtensions: [codex], answers: ["Deny"], judge: '{"decision":"ask-user","reason":"unclear","confidence":0}', api: "openai-responses" },
  );
  assert.equal(results[0].isError, false, results[0].text);
  assert.equal(readFileSync(join(workspace, "src", "added.ts"), "utf8"), "patched\n");
  assert.equal(dialogs.length, 1);
  assert.equal(results[1].isError, true);
  assert.ok(!existsSync(join(other, "patched.md")));
});

test("filtered network: a new domain asks; a denied one never connects", { skip }, async () => {
  const config = join(agentDir, "permissions.json");
  writeFileSync(config, JSON.stringify({ enabled: true, network: { mode: "filtered" } }));
  try {
    const { results, dialogs } = await turn(
      [
        {
          tool: "bash",
          args: {
            command:
              "echo \"proxy=${HTTPS_PROXY%%@*}\"; curl -sS -m 10 -o /dev/null -w 'code=%{http_code}\\n' https://example.com 2>&1 | tail -1; curl -sS -m 5 --noproxy '*' -o /dev/null https://example.com 2>&1 | tail -1",
          },
        },
      ],
      { answers: ["Deny"], judge: '{"decision":"ask-user","reason":"not part of the task","confidence":0.9}' },
    );
    assert.equal(dialogs.length, 1);
    assert.match(dialogs[0].title, /^Allow the agent to reach example\.com\?/);
    assert.match(dialogs[0].title, /couldn't approve this automatically\. not part of the task/);
    assert.match(results[0].text, /proxy=http:\/\/cypher:/);
    assert.doesNotMatch(results[0].text, /code=200/);
  } finally {
    writeFileSync(config, enabledConfig);
  }
});

test("the patched grep and apply_patch carry their hooks", () => {
  assert.match(readFileSync(join(modules, "@earendil-works", "pi-coding-agent", "dist", "core", "tools", "grep.js"), "utf8"), /CYPHER-RUNTIME-PATCH: grep-spawn-hook/);
  assert.match(readFileSync(join(modules, "pi-codex-tools", "src", "apply-patch.ts"), "utf8"), /CYPHER-RUNTIME-PATCH: codex-tools-permissions/);
  // OpenAI grammar models still see apply_patch instead of the (replaced) edit/write.
  assert.match(readFileSync(join(modules, "pi-codex-tools", "extensions", "index.ts"), "utf8"), /cypher-permissions\\\.ts\$/);
});

test("full access: no checks and no sandbox", { skip }, async () => {
  const { results, dialogs } = await turn([{ tool: "bash", args: { command: "cat .env" } }, { tool: "read", args: { path: ".env" } }], { level: "danger-full-access" });
  assert.equal(dialogs.length, 0);
  assert.match(results[0].text, /hunter2/);
  assert.match(results[1].text, /hunter2/);
});

test("off by default: without the switch the extension registers nothing", async () => {
  const config = join(agentDir, "permissions.json");
  rmSync(config, { force: true });
  try {
    const { results, dialogs } = await turn([
      { tool: "read", args: { path: ".env" } },
      { tool: "request_permission", args: { capability: "read", paths: [".env"], reason: "test" } },
    ]);
    assert.equal(dialogs.length, 0);
    assert.match(results[0].text, /hunter2/);
    assert.equal(results[1].isError, true, "no request_permission tool");
  } finally {
    writeFileSync(config, enabledConfig);
  }
});

// A throwaway sshd on localhost that accepts one generated key, so a real ssh
// signs in without touching real servers or known_hosts.
function startSshd() {
  const dir = mkdtempSync(join(scratch, "sshd-"));
  const keygen = (file) => spawnSync("ssh-keygen", ["-q", "-t", "ed25519", "-N", "", "-f", file]).status === 0;
  if (!keygen(join(dir, "host_key")) || !keygen(join(home, ".ssh", "id_test"))) return undefined;
  writeFileSync(join(dir, "authorized_keys"), readFileSync(join(home, ".ssh", "id_test.pub")), { mode: 0o600 });
  const port = 20000 + Math.floor(Math.random() * 20000);
  writeFileSync(
    join(dir, "sshd_config"),
    [`Port ${port}`, "ListenAddress 127.0.0.1", `HostKey ${join(dir, "host_key")}`, `AuthorizedKeysFile ${join(dir, "authorized_keys")}`, `PidFile ${join(dir, "pid")}`, "PasswordAuthentication no", "KbdInteractiveAuthentication no", "UsePAM no", "StrictModes no"].join("\n"),
  );
  const sshd = ["/usr/sbin/sshd", "/usr/bin/sshd"].find((path) => existsSync(path));
  if (!sshd || spawnSync(sshd, ["-f", join(dir, "sshd_config")]).status !== 0) return undefined;
  return {
    port,
    stop: () => {
      spawnSync("sh", ["-c", `kill $(cat ${join(dir, "pid")}) 2>/dev/null`]);
      for (const name of ["id_test", "id_test.pub"]) rmSync(join(home, ".ssh", name), { force: true });
    },
  };
}

test("ssh: fails in the sandbox; approving the key files doesn't change that; escalated it signs in", { skip }, async (t) => {
  const sshd = startSshd();
  if (!sshd) return t.skip("no sshd to sign in to");
  try {
    const ssh = `ssh -p ${sshd.port} -i ~/.ssh/id_test -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR 127.0.0.1 echo signed-in`;
    const { results, dialogs } = await turn(
      [
        { tool: "bash", args: { command: ssh } },
        { tool: "request_permission", args: { capability: "read", paths: ["~/.ssh"], reason: "ssh needs the key" } },
        { tool: "bash", args: { command: ssh } },
        { tool: "bash", args: { command: ssh, escalate: { label: "Sign in to the test server", reason: "the task", patterns: [["ssh", "**"]] } } },
        { tool: "bash", args: { command: ssh.replace("echo signed-in", "echo again") } },
      ],
      { answers: ["Allow for this chat", "Allow for this chat"] },
    );
    // In the sandbox the key is hidden: ssh fails, and the hint says how to go on.
    assert.equal(results[0].isError, true);
    assert.match(results[0].text, /Permission denied \(publickey\)/);
    assert.match(results[0].text, /run it again with `escalate`/);
    // Approving the key files serves the file tools; both sides are told so.
    assert.match(dialogs[0].title, /Note: For the agent's file tools only; commands such as ssh never get it\./);
    assert.match(results[1].text, /Commands never get this: a command that needs it .* runs again with `escalate`/);
    assert.equal(results[2].isError, true, "still sandboxed");
    // Escalated, ssh signs in; allowed for the chat, the next one needs no dialog.
    assert.match(dialogs[1].title, /^Run this command outside the sandbox\?/);
    assert.equal(results[3].isError, false, results[3].text);
    assert.match(results[3].text, /^signed-in$/m);
    assert.match(results[4].text, /^again$/m);
    assert.match(results[4].text, /allowed by `ssh …`/);
    assert.equal(dialogs.length, 2);
    // What Cypher shows on each row, and which call each dialog belongs to.
    assert.deepEqual(results.map((result) => result.record), [
      { outcome: "blocked" },
      { outcome: "allowed", scope: "chat" },
      { outcome: "blocked" },
      { outcome: "outside", scope: "chat", purpose: "Sign in to the test server" },
      { outcome: "outside", scope: "rule", rule: "ssh …", purpose: "Sign in to the test server" },
    ]);
    for (const dialog of dialogs) assert.match(dialog.title.split("\n").at(-1), /^cypher-permission: \{"kind":"(access|escalate)","toolCallId":"[^"]+"\}$/);
  } finally {
    sshd.stop();
  }
});
