// Contract test for Settings → MCP sign-in, which the engine drives through
// Pi's built-in MCP (crates/engine/src/mcp/login.rs): with every curated
// package loaded, `/mcp login <server>` over RPC announces the authorization
// link in a notify, takes the pasted callback URL from an input dialog, and
// stores the tokens in `mcp-auth.json` under the key the engine reads
// (`credential_keys` in crates/engine/src/mcp.rs). The OAuth-protected MCP
// server is a local fixture; no browser or network is used.
//
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { test } from "node:test";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const runtime = resolve(stage);
const LIMIT = 256 * 1024;

/** An MCP server behind OAuth: protected resource and authorization server
 * metadata, dynamic client registration, an authorization endpoint that
 * redirects at once, and a token endpoint. */
function fixtureServer() {
  const issued = { code: "fixture-code", access: "fixture-access-token" };
  const server = createServer(async (req, res) => {
    const origin = `http://127.0.0.1:${server.address().port}`;
    const url = new URL(req.url, origin);
    let body = "";
    for await (const chunk of req) body += chunk;
    const json = (status, value, headers = {}) => {
      res.writeHead(status, { "content-type": "application/json", ...headers }).end(JSON.stringify(value));
    };
    if (url.pathname.startsWith("/.well-known/oauth-protected-resource")) {
      return json(200, { resource: `${origin}/mcp`, authorization_servers: [origin] });
    }
    if (url.pathname.startsWith("/.well-known/oauth-authorization-server")) {
      return json(200, {
        issuer: origin,
        authorization_endpoint: `${origin}/authorize`,
        token_endpoint: `${origin}/token`,
        registration_endpoint: `${origin}/register`,
        response_types_supported: ["code"],
        grant_types_supported: ["authorization_code", "refresh_token"],
        code_challenge_methods_supported: ["S256"],
        token_endpoint_auth_methods_supported: ["none"],
      });
    }
    if (url.pathname === "/register") {
      return json(201, { ...JSON.parse(body), client_id: "fixture-client" });
    }
    if (url.pathname === "/authorize") {
      const callback = new URL(url.searchParams.get("redirect_uri"));
      callback.searchParams.set("code", issued.code);
      callback.searchParams.set("state", url.searchParams.get("state"));
      res.writeHead(302, { location: callback.href }).end();
      return;
    }
    if (url.pathname === "/token") {
      const form = new URLSearchParams(body);
      if (form.get("code") !== issued.code || !form.get("code_verifier")) return json(400, { error: "invalid_grant" });
      return json(200, { access_token: issued.access, token_type: "Bearer", expires_in: 3600, refresh_token: "fixture-refresh" });
    }
    if (url.pathname === "/mcp") {
      if (req.headers.authorization !== `Bearer ${issued.access}`) {
        res.writeHead(401, {
          "www-authenticate": `Bearer resource_metadata="${origin}/.well-known/oauth-protected-resource/mcp"`,
        }).end();
        return;
      }
      if (req.method !== "POST") return void res.writeHead(405).end();
      const message = JSON.parse(body);
      if (message.id === undefined) return void res.writeHead(202).end();
      const result = message.method === "initialize"
        ? { protocolVersion: message.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "fixture", version: "1" } }
        : message.method === "tools/list"
          ? { tools: [{ name: "echo", description: "Echo a message", inputSchema: { type: "object", properties: {} } }] }
          : {};
      return json(200, { jsonrpc: "2.0", id: message.id, result });
    }
    res.writeHead(404).end();
  });
  return server;
}

test("/mcp login over RPC takes a pasted callback and stores tokens where the engine reads them", async (t) => {
  const manifest = JSON.parse(await readFile(join(runtime, "runtime.json"), "utf8"));
  const root = await mkdtemp(join(tmpdir(), "cypher-mcp-login-"));
  const agent = join(root, "agent");
  const bin = join(root, "bin");
  const server = fixtureServer();
  let child;
  t.after(async () => {
    if (child?.exitCode === null && child.signalCode === null) {
      const closed = once(child, "close");
      child.kill("SIGKILL");
      await closed;
    }
    server.closeAllConnections();
    if (server.listening) await new Promise((done) => server.close(done));
    await rm(root, { recursive: true, force: true });
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const origin = `http://127.0.0.1:${server.address().port}`;

  await mkdir(join(agent, "npm"), { recursive: true });
  await writeFile(join(agent, "npm/package.json"), '{"name":"cypher-ci-agent","private":true}');
  // Every curated package, as activation registers them: none may replace /mcp.
  const packages = Object.keys(manifest.plugins).sort().map((name) => {
    const source = join(runtime, "npm/node_modules", name);
    return name === "pi-permission-control" ? { source, extensions: ["-index.ts"] } : source;
  });
  await writeFile(join(agent, "settings.json"), JSON.stringify({ packages }));
  await writeFile(join(agent, "mcp.json"), JSON.stringify({ mcpServers: { "fixture-docs": { url: `${origin}/mcp` } } }));
  // Pi opens the authorization page itself; keep it from reaching a browser.
  await mkdir(bin);
  for (const name of ["open", "xdg-open"]) {
    await writeFile(join(bin, name), "#!/bin/sh\nexit 0\n");
    await chmod(join(bin, name), 0o755);
  }
  const env = {
    PATH: [bin, join(runtime, "bin"), "/usr/bin", "/bin", "/usr/sbin", "/sbin"].join(delimiter),
    HOME: root, TMPDIR: tmpdir(), LANG: "en_US.UTF-8",
    PI_CODING_AGENT_DIR: agent, PI_PACKAGE_DIR: join(runtime, "pi"), PI_OFFLINE: "1",
  };
  child = spawn(join(runtime, "bin/pi"), ["--mode", "rpc", "--no-session"],
    { cwd: agent, env, stdio: ["pipe", "pipe", "pipe"] });
  let diagnostics = "";
  child.stderr.setEncoding("utf8").on("data", (text) => { diagnostics = (diagnostics + text).slice(-LIMIT); });
  child.stdin.on("error", () => {});
  const send = (value) => child.stdin.write(JSON.stringify(value) + "\n");

  const notices = [];
  let link;
  await new Promise((accept, reject) => {
    const fail = (message) => { clearTimeout(timer); reject(new Error(`${message}\n${diagnostics}`)); };
    const timer = setTimeout(() => fail("MCP login timed out"), 60000);
    child.on("exit", (code, signal) => fail(`Pi exited during MCP login (${code ?? signal})`));
    let buffer = "";
    child.stdout.setEncoding("utf8").on("data", async (text) => {
      buffer += text;
      let index;
      while ((index = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, index);
        buffer = buffer.slice(index + 1);
        let value;
        try { value = JSON.parse(line); } catch { continue; }
        if (value.type === "extension_ui_request" && value.method === "notify") {
          notices.push(value);
          if (value.notifyType === "error") return fail(`MCP login failed: ${value.message}`);
          // What login.rs extracts: one line that is the HTTPS-or-loopback link.
          link ??= value.message.split("\n").map((part) => part.trim())
            .find((part) => part.startsWith(`${origin}/authorize?`));
        } else if (value.type === "extension_ui_request" && value.method === "input") {
          try {
            assert.ok(link, "the authorization link must precede the input dialog");
            const authorization = new URL(link);
            assert.ok(authorization.searchParams.get("state"));
            assert.ok(authorization.searchParams.get("redirect_uri"));
            // The browser runs elsewhere: follow the redirect, paste its target.
            const redirect = await fetch(link, { redirect: "manual" });
            const callback = redirect.headers.get("location");
            assert.equal(new URL(callback).searchParams.get("state"), authorization.searchParams.get("state"));
            send({ type: "extension_ui_response", id: value.id, value: callback });
          } catch (error) { fail(error.message); }
        } else if (value.type === "response" && value.id === "login") {
          if (!value.success) return fail(`/mcp login was rejected: ${line}`);
          clearTimeout(timer);
          accept();
        }
      }
    });
    send({ id: "login", type: "prompt", message: "/mcp login fixture-docs" });
  });
  assert.ok(notices.some((notice) => notice.notifyType === "info" &&
    notice.message.includes('Signed in to MCP server "fixture-docs"')), JSON.stringify(notices));
  const stored = JSON.parse(await readFile(join(agent, "mcp-auth.json"), "utf8"));
  // Namespace (`-` → `_`) plus the URL as `new URL()` serializes it.
  const state = stored[`mcp__fixture_docs|${origin}/mcp`];
  assert.ok(state, `unexpected credential keys: ${Object.keys(stored)}`);
  assert.equal(state.tokens.access_token, "fixture-access-token");
  assert.equal(state.tokens.refresh_token, "fixture-refresh");
  assert.ok(state.tokensExpireAt > Date.now());
});
