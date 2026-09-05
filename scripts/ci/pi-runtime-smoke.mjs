import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { access, mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const LIMIT = 256 * 1024;

export async function verifyRuntime(directory, { brokenExtension = false } = {}) {
  const runtime = resolve(directory);
  const manifest = JSON.parse(await readFile(join(runtime, "runtime.json"), "utf8"));
  const root = await mkdtemp(join(tmpdir(), "cypher-runtime-smoke-"));
  const agent = join(root, "agent");
  const key = "cypher-ci-fixture-not-a-real-api-key";
  const redact = text => String(text).replaceAll(key, "[redacted]");
  // A whitelist prevents inherited provider keys, system Pi settings, proxies
  // or NODE_OPTIONS from turning a smoke test into a real account operation.
  const env = {
    PATH: [join(runtime, "bin"), "/usr/bin", "/bin", "/usr/sbin", "/sbin"].join(delimiter),
    HOME: root, TMPDIR: tmpdir(), LANG: "en_US.UTF-8",
    PI_CODING_AGENT_DIR: agent, PI_PACKAGE_DIR: join(runtime, "pi"), PI_OFFLINE: "1",
  };
  const server = createServer((req, res) => {
    if (req.url !== "/v1/models" || req.headers.authorization !== `Bearer ${key}`) {
      res.writeHead(403).end();
      return;
    }
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify({ data: [{ id: "gpt-4o" }] }));
  });
  let child;
  try {
    await mkdir(join(agent, "extension-settings"), { recursive: true });
    await mkdir(join(agent, "npm"), { recursive: true });
    await writeFile(join(agent, "npm/package.json"), '{"name":"cypher-ci-agent","private":true}');
    await writeFile(join(agent, "extension-settings/provider-newapi.json"), JSON.stringify({
      version: 1, providers: {}, settings: { onboardingWarnCountdown: 0 },
    }));
    const packages = [];
    for (const name of Object.keys(manifest.plugins).sort()) {
      const source = join(runtime, "npm/node_modules", name);
      await access(join(source, "package.json"));
      packages.push(name === "pi-permission-control" ? { source, extensions: ["-index.ts"] } : source);
    }
    assert.ok(packages.length > 0, "Runtime must register its curated packages");
    const extensions = [join(runtime, "extensions/cypher-provider-auth.ts")];
    if (brokenExtension) {
      const path = join(root, "broken.ts");
      await writeFile(path, 'throw new Error("CYPHER_EXPECTED_EXTENSION_FAILURE");\n');
      extensions.push(path);
    }
    await writeFile(join(agent, "settings.json"), JSON.stringify({ packages, extensions }));
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    // Bootstrap a local model via the real provider service, using stdin only.
    const helper = spawn(join(runtime, "bin/node"), [join(runtime, "provider-service.mjs")],
      { cwd: agent, env, stdio: ["pipe", "pipe", "pipe"] });
    let helperOutput = "";
    let helperError = "";
    helper.stdout.setEncoding("utf8").on("data", text => {
      helperOutput += text;
      if (helperOutput.length > LIMIT) helper.kill("SIGKILL");
    });
    helper.stderr.setEncoding("utf8").on("data", text => { helperError = (helperError + text).slice(-LIMIT); });
    helper.stdin.on("error", () => {});
    const helperTimer = setTimeout(() => helper.kill("SIGKILL"), 25000);
    const helperExit = once(helper, "close");
    helper.stdin.end(JSON.stringify({
      action: "save", id: "cypher-ci", baseUrl: `http://127.0.0.1:${server.address().port}`, apiKey: key,
    }));
    try {
      const [code] = await helperExit;
      assert.equal(code, 0, redact(helperOutput + helperError));
      assert.equal(JSON.parse(helperOutput).ok, true);
    } finally {
      clearTimeout(helperTimer);
      if (helper.exitCode === null && helper.signalCode === null) helper.kill("SIGKILL");
    }
    child = spawn(join(runtime, "bin/pi"),
      ["--mode", "rpc", "--offline", "--no-session", "--model", "cypher-ci/gpt-4o"],
      { cwd: agent, env, stdio: ["pipe", "pipe", "pipe"] });
    let diagnostics = "";
    let buffer = "";
    const responses = new Map();
    child.stdin.on("error", () => {});
    await new Promise((accept, reject) => {
      const finish = error => {
        clearTimeout(timer);
        error ? reject(error) : accept();
      };
      const fail = message => finish(new Error(redact(message + "\n" + diagnostics)));
      const timer = setTimeout(() => fail("Pi RPC smoke test timed out"), 30000);
      child.on("error", error => fail(error.message));
      child.on("exit", (code, signal) => fail(`Pi exited before smoke completion (${code ?? signal})`));
      child.stderr.setEncoding("utf8").on("data", text => {
        diagnostics = (diagnostics + text).slice(-LIMIT);
      });
      child.stdout.setEncoding("utf8").on("data", text => {
        buffer += text;
        if (buffer.length > LIMIT) { fail("Oversized Pi RPC output"); return; }
        let index;
        while ((index = buffer.indexOf("\n")) !== -1) {
          const line = buffer.slice(0, index); buffer = buffer.slice(index + 1);
          let value;
          try { value = JSON.parse(line); } catch {
            diagnostics = (diagnostics + line + "\n").slice(-LIMIT);
            continue;
          }
          if (value.type === "extension_error" ||
              (value.type === "extension_ui_request" && value.notifyType === "error")) {
            fail("Extension startup error: " + line); return;
          }
          if (value.type !== "response") continue;
          if (!value.success) { fail("Pi RPC command failed: " + line); return; }
          responses.set(value.id, value.data);
          if (responses.has("state") && responses.has("commands") && responses.has("models")) {
            try {
              assert.equal(responses.get("state").model?.provider, "cypher-ci");
              assert.equal(responses.get("state").isStreaming, false);
              assert.ok(responses.get("models").models.some(model =>
                model.provider === "cypher-ci" && model.id === "gpt-4o"));
              const names = new Set(responses.get("commands").commands.map(command => command.name));
              for (const name of ["provider", "login", "logout", "newapi-provider-add"]) {
                assert.ok(names.has(name), `Required command missing: ${name}`);
              }
              finish();
            } catch (error) { fail(error.message); }
          }
        }
      });
      for (const [id, type] of [["state", "get_state"], ["commands", "get_commands"], ["models", "get_available_models"]]) {
        child.stdin.write(JSON.stringify({ id, type }) + "\n");
      }
    });
  } finally {
    if (child?.pid && child.exitCode === null && child.signalCode === null) {
      const closed = once(child, "close");
      child.kill("SIGKILL");
      await closed;
    }
    server.closeAllConnections();
    if (server.listening) await new Promise(done => server.close(done));
    await rm(root, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    assert.ok(process.argv[2], "Usage: node pi-runtime-smoke.mjs <extracted-runtime>");
    await verifyRuntime(process.argv[2]);
    // Regression guard: a deliberately broken extension MUST make the same
    // startup check fail. `pi --help` used to pass this exact failure.
    await assert.rejects(verifyRuntime(process.argv[2], { brokenExtension: true }),
      /CYPHER_EXPECTED_EXTENSION_FAILURE/);
    console.log("Pi Runtime RPC smoke and broken-extension regression passed");
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
