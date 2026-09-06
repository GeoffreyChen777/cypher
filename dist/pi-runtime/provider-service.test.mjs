import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, readFile, writeFile, mkdir, rm, stat, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { normalizeUrl, validateId, serializeModelRefreshes } from "./provider-service.mjs";

test("SDK background refreshes and explicit refreshes are serialized, including failures", async () => {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  const started = [];
  let active = 0;
  const runtime = serializeModelRefreshes({
    async refresh(name) {
      assert.equal(active++, 0, "refreshes must not overlap");
      started.push(name);
      try {
        if (name === "background") await gate;
        if (name === "failure") throw new Error("fixture failure");
        return name;
      } finally { active--; }
    },
    registerProvider() { void this.refresh("background"); },
  });
  runtime.registerProvider();
  const failed = runtime.refresh("failure");
  const rejected = assert.rejects(failed, /fixture failure/);
  const final = runtime.refresh("final snapshot");
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(started, ["background"]);
  release();
  await rejected;
  assert.equal(await final, "final snapshot");
  assert.deepEqual(started, ["background", "failure", "final snapshot"]);
});

test("provider input validation", () => {
  assert.equal(normalizeUrl("https://example.com/v1/"), "https://example.com");
  assert.equal(normalizeUrl("https://example.com/gateway/v1"), "https://example.com/gateway");
  assert.equal(normalizeUrl("http://127.0.0.1:1234/"), "http://127.0.0.1:1234");
  for (const url of ["file:///tmp/x", "https://user:secret@example.com", "https://example.com/?key=x",
    "https://example.com/#key", "http://example.com", "not a URL"])
    assert.throws(() => normalizeUrl(url));
  for (const id of ["", "../x", "x/y", "__proto__", "constructor", "a b", "x".repeat(65)])
    assert.throws(() => validateId(id));
  assert.equal(validateId("mvp-lab"), "mvp-lab");
});

test("isolated Runtime provider lifecycle and credential redaction", {
  skip: !process.env.PI_PACKAGE_DIR,
}, async () => {
  const agent = await mkdtemp(join(tmpdir(), "cypher-providers-test-"));
  const helperSource = process.env.CYPHER_PROVIDER_HELPER ??
    fileURLToPath(new URL("./provider-service.mjs", import.meta.url));
  const helper = join(agent, "provider-service.mjs");
  // A build launched from Pi must not lend its session marker, credentials,
  // proxy configuration or home-directory state to the fixture subprocesses.
  const childEnv = {
    HOME: agent, PATH: process.env.PATH, TMPDIR: tmpdir(), PI_OFFLINE: "1",
    PI_CODING_AGENT_DIR: agent, PI_PACKAGE_DIR: process.env.PI_PACKAGE_DIR,
  };
  // Production invokes the script via current -> versions/<version>.
  await symlink(helperSource, helper);
  const secret = "cypher-fixture-secret-never-log";
  let mode = "ok";
  let requests = 0;
  const server = createServer((req, res) => {
    requests++;
    if (mode === "redirect") { res.writeHead(302, { location: "/elsewhere" }); res.end(); return; }
    if (req.headers.authorization !== `Bearer ${secret}` || mode === "unauthorized") {
      res.writeHead(401); res.end(secret); return;
    }
    res.setHeader("content-type", "application/json");
    if (mode === "malformed") { res.end(JSON.stringify({ detail: secret })); return; }
    res.end(JSON.stringify({ data: mode === "empty" ? [] : [{ id: "gpt-4o" }, { id: "test-model" }] }));
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}`;
  async function call(request, success = true) {
    const child = spawn(process.execPath, [helper], {
      cwd: agent, env: childEnv,
      stdio: ["pipe", "pipe", "pipe"],
    });
    let output = "", stderr = "";
    child.stdout.on("data", d => { output += d; });
    child.stderr.on("data", d => { stderr += d; });
    child.stdin.end(JSON.stringify(request));
    const exit = await new Promise(resolve => child.on("close", resolve));
    assert.equal(stderr.includes(secret), false);
    assert.equal(output.includes(secret), false);
    assert.equal(exit, success ? 0 : 1, output + stderr);
    const envelope = JSON.parse(output);
    assert.equal(envelope.ok, success, output);
    return success ? envelope.data : envelope.error;
  }
  try {
    await mkdir(join(agent, "extension-settings"), { recursive: true });
    await writeFile(join(agent, "extension-settings/provider-newapi.json"), JSON.stringify({
      version: 1, providers: {}, settings: { onboardingWarnCountdown: 0 },
    }));
    assert.deepEqual((await call({ action: "list" })).providers, []);
    await call({ action: "save", id: "one", baseUrl: url, apiKey: "wrong" }, false);
    assert.deepEqual((await call({ action: "list" })).providers, []);
    for (const failure of ["empty", "malformed", "redirect"]) {
      mode = failure;
      await call({ action: "save", id: "one", baseUrl: url, apiKey: secret }, false);
    }
    mode = "ok";
    let snapshot = await call({ action: "save", id: "one", baseUrl: `${url}/v1`, apiKey: secret });
    assert.equal(snapshot.providers[0].state, "connected");
    assert.equal(snapshot.providers[0].modelCount, 2);
    const authPath = join(agent, "auth.json");
    assert.equal(JSON.parse(await readFile(authPath, "utf8")).one.key, secret);
    assert.equal((await stat(authPath)).mode & 0o077, 0, "credential file is private");
    const before = requests;
    snapshot = await call({ action: "list" });
    assert.equal(requests, before, "listing is offline");
    assert.equal(snapshot.providers[0].modelCount, 2, "catalog persists across processes");
    // The same configuration + credential + catalog must work in Pi RPC,
    // not just in the settings helper's custom runtime.
    const packageRoot = await import("node:fs/promises").then(fs => fs.realpath(process.env.PI_PACKAGE_DIR));
    const plugin = join(packageRoot, "..", "..", "pi-provider-newapi");
    await writeFile(join(agent, "settings.json"), JSON.stringify({ packages: [plugin] }));
    const pi = spawn(process.execPath, [join(packageRoot, "dist/cli.js"), "--mode", "rpc", "--no-session"], {
      cwd: agent, env: childEnv,
      stdio: ["pipe", "pipe", "pipe"],
    });
    try {
      let buffer = "";
      let diagnostics = "";
      let lastResponse = "";
      pi.stderr.on("data", chunk => { diagnostics += chunk; });
      const catalog = await new Promise((resolve, reject) => {
        const poll = setInterval(() => {
          pi.stdin.write('{"id":"catalog","type":"get_available_models"}\n');
        }, 200);
        const stop = () => { clearTimeout(timer); clearInterval(poll); };
        const timer = setTimeout(() => {
          stop();
          reject(new Error(`Pi RPC catalog timed out: ${plugin}\n${diagnostics}\n${lastResponse}`.replaceAll(secret, "[redacted]")));
        }, 20000);
        pi.on("error", error => { stop(); reject(error); });
        pi.on("exit", () => { stop(); reject(new Error("Pi RPC exited before catalog")); });
        pi.stdout.on("data", chunk => {
          buffer += chunk;
          let at;
          while ((at = buffer.indexOf("\n")) >= 0) {
            const line = buffer.slice(0, at); buffer = buffer.slice(at + 1);
            let value; try { value = JSON.parse(line); } catch { continue; }
            if (value.id === "catalog") lastResponse = line;
            else diagnostics += line + "\n";
            if (value.id === "catalog" && value.data?.models?.some(m => m.provider === "one")) {
              stop(); resolve(value);
            }
          }
        });
        pi.stdin.write('{"id":"catalog","type":"get_available_models"}\n');
      });
      assert.equal(catalog.success, true);
      assert.equal(catalog.data.models.filter(m => m.provider === "one").length, 2);
    } finally {
      pi.kill();
    }
    await call({ action: "save", id: "one", baseUrl: url, apiKey: secret }, false);
    await call({ action: "save", id: "openai", baseUrl: url, apiKey: secret }, false);
    await call({ action: "save", id: "one", baseUrl: url + "/other", edit: true }, false);
    await call({ action: "save", id: "one", baseUrl: url, edit: true });
    await call({ action: "save", id: "two", baseUrl: url, apiKey: secret });
    mode = "unauthorized";
    snapshot = await call({ action: "refresh", id: "one" });
    assert.equal(snapshot.providers.find(p => p.id === "one").state, "error");
    assert.equal(snapshot.providers.find(p => p.id === "one").modelCount, 2, "failed refresh keeps catalog");
    mode = "ok";
    snapshot = await call({ action: "refresh", id: "one" });
    assert.equal(snapshot.providers.find(p => p.id === "one").state, "connected");
    snapshot = await call({ action: "logout", id: "one" });
    assert.equal(snapshot.providers.find(p => p.id === "one").credentialSaved, false);
    assert.equal(snapshot.providers.find(p => p.id === "two").credentialSaved, true);
    snapshot = await call({ action: "remove", id: "two" });
    assert.equal(snapshot.providers.length, 1);
    const config = JSON.parse(await readFile(join(agent, "extension-settings/provider-newapi.json"), "utf8"));
    assert.equal(config.settings.onboardingWarnCountdown, 0);
    assert.equal(JSON.parse(await readFile(authPath, "utf8")).two, undefined);
    const futureConfig = JSON.stringify({ ...config, version: 99 });
    await writeFile(join(agent, "extension-settings/provider-newapi.json"), futureConfig);
    await call({ action: "list" }, false);
    assert.equal(await readFile(join(agent, "extension-settings/provider-newapi.json"), "utf8"), futureConfig);
    await writeFile(join(agent, "extension-settings/provider-newapi.json"), "{broken");
    await call({ action: "list" }, false);
    assert.equal(await readFile(join(agent, "extension-settings/provider-newapi.json"), "utf8"), "{broken");
  } finally {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    await rm(agent, { recursive: true, force: true });
  }
});
