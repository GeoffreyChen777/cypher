// Cypher's settings transport. Only explicit, device-scoped paths are accepted.
// Secrets arrive over stdin, never argv, stdout, logs, or chat messages.
import { mkdir, readFile, realpath, rename, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join, isAbsolute } from "node:path";
import { pathToFileURL } from "node:url";
import { createHash } from "node:crypto";

class ProviderError extends Error {}
function safeError(error) {
  return error instanceof ProviderError ? error.message
    : "Provider operation failed. Check the Runtime and provider configuration.";
}

export function normalizeUrl(value) {
  let url;
  try { url = new URL(value.trim()); } catch { throw new ProviderError("Enter a valid Base URL."); }
  if (url.username || url.password || url.search || url.hash)
    throw new ProviderError("Base URL must not contain credentials, query parameters, or a fragment.");
  if (url.protocol !== "https:" &&
      !(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)))
    throw new ProviderError("Use HTTPS (HTTP is allowed only for local development).");
  // The bundled NewAPI provider adds /v1 itself.
  url.pathname = url.pathname.replace(/\/+$/, "").replace(/\/v1$/, "");
  return url.toString().replace(/\/+$/, "");
}

export function validateId(id) {
  if (typeof id !== "string" || !/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$/.test(id))
    throw new ProviderError("Use 1–64 letters, numbers, dots, underscores, or hyphens for the provider name.");
  if (["constructor", "prototype", "__proto__"].includes(id))
    throw new ProviderError("This provider name is reserved.");
  return id;
}

async function jsonFile(path, fallback) {
  try { return JSON.parse(await readFile(path, "utf8")); }
  catch (error) {
    if (error.code === "ENOENT") return fallback;
    throw new ProviderError("Provider configuration could not be read. Existing data was not replaced.");
  }
}

async function atomicJson(path, data) {
  const temporary = `${path}.${process.pid}.tmp`;
  await writeFile(temporary, JSON.stringify(data, null, 2) + "\n", { mode: 0o600 });
  await rename(temporary, path);
}

async function probe(baseUrl, key) {
  if (!key || /[\r\n]/.test(key)) throw new ProviderError("Enter an API Key.");
  let response;
  try {
    response = await fetch(`${baseUrl}/v1/models`, {
      headers: { Authorization: `Bearer ${key}` },
      redirect: "error", // Never forward credentials to another endpoint.
      signal: AbortSignal.timeout(15000),
    });
  } catch {
    throw new ProviderError("Connection failed or timed out. Check the Base URL and network.");
  }
  if (response.status === 401 || response.status === 403)
    throw new ProviderError("Authentication rejected. Check the API Key and its permissions.");
  if (!response.ok) throw new ProviderError(`Model discovery failed (HTTP ${response.status}).`);
  let size = 0;
  const chunks = [];
  for await (const chunk of response.body) {
    size += chunk.length;
    if (size > 4 * 1024 * 1024) throw new ProviderError("Model catalog is too large.");
    chunks.push(chunk);
  }
  let data;
  try { data = JSON.parse(Buffer.concat(chunks).toString()); }
  catch { throw new ProviderError("The endpoint did not return a JSON model catalog."); }
  if (!Array.isArray(data.data) || data.data.some(m => !m || typeof m.id !== "string" || !m.id))
    throw new ProviderError("Expected an OpenAI-compatible /v1/models response.");
  if (!data.data.length) throw new ProviderError("No models are available for this API Key.");
  return data;
}

// registerProvider schedules a fire-and-forget refresh. A simultaneous explicit
// refresh can supersede it (or be superseded by it) inside pi-ai, returning an
// empty snapshot even though the catalog is already persisted. Serialize this
// helper instance's refreshes, including SDK-initiated calls, so the final
// awaited refresh observes a completed snapshot. Do not patch SDK globals.
export function serializeModelRefreshes(runtime) {
  const refresh = runtime.refresh.bind(runtime);
  let tail = Promise.resolve();
  runtime.refresh = (...args) => {
    const result = tail.then(() => refresh(...args));
    tail = result.then(() => undefined, () => undefined);
    return result;
  };
  return runtime;
}

export async function providerRequest(request) {
  const agent = process.env.PI_CODING_AGENT_DIR;
  const pkg = process.env.PI_PACKAGE_DIR;
  if (!agent || !pkg || !isAbsolute(agent) || !isAbsolute(pkg))
    throw new ProviderError("Cypher's isolated Pi Runtime is not configured.");
  await mkdir(join(agent, "extension-settings"), { recursive: true });
  const require = createRequire(join(await realpath(pkg), "package.json"));
  const { createJiti } = require("jiti");
  const jiti = createJiti(join(pkg, "package.json"), { fsCache: false });
  const pluginRoot = require.resolve("pi-provider-newapi/package.json").replace(/package\.json$/, "");
  const { buildProviderModels, parseModelsResponse } = await jiti.import(join(pluginRoot, "src/models.ts"));
  const { deserializeConfig } = await jiti.import(join(pluginRoot, "src/config.ts"));
  const { ModelRuntime } = await import(pathToFileURL(join(pkg, "dist/core/model-runtime.js")));
  const { AuthStorage } = await import(pathToFileURL(join(pkg, "dist/core/auth-storage.js")));
  const auth = AuthStorage.create(join(agent, "auth.json"));
  const runtime = serializeModelRefreshes(await ModelRuntime.create({
    credentials: auth, modelsPath: join(agent, "models.json"),
    modelsStorePath: join(agent, "models-store.json"), refreshOnCreate: false,
  }));
  const configPath = join(agent, "extension-settings/provider-newapi.json");
  const statusPath = join(agent, "cypher-provider-status.json");
  const config = await jsonFile(configPath, { version: 1, providers: {}, settings: {} });
  if (config.version !== 1 || !config.providers || Array.isArray(config.providers) ||
      typeof config.providers !== "object")
    throw new ProviderError("Unsupported provider configuration. Existing data was not replaced.");
  try { deserializeConfig(JSON.stringify(config)); }
  catch { throw new ProviderError("Unsupported provider configuration. Existing data was not replaced."); }
  const statuses = await jsonFile(statusPath, {});
  const fingerprint = (entry, credential) => createHash("sha256")
    .update(JSON.stringify([entry.baseUrl, credential])).digest("hex");
  const register = (id, entry, fresh) => runtime.registerProvider(id, {
    name: `NewAPI (${id})`, baseUrl: entry.baseUrl, api: "openai-completions", models: [],
    async refreshModels(context) {
      if (fresh) {
        await context.publish({ persist: { models: fresh, checkedAt: Date.now() } });
        return fresh;
      }
      return context.stored?.models ?? [];
    },
  });
  for (const [id, entry] of Object.entries(config.providers)) register(id, entry);
  const action = request.action;
  if (action !== "list") {
    const id = validateId(request.id);
    const existing = Object.hasOwn(config.providers, id) ? config.providers[id] : undefined;
    if (action === "save") {
      if (!request.edit && (existing || runtime.getProvider(id)))
        throw new ProviderError("That provider name is already in use.");
      if (request.edit && !existing) throw new ProviderError("Provider no longer exists. Reload the page.");
      const baseUrl = normalizeUrl(request.baseUrl ?? "");
      const stored = await auth.read(id);
      // Do not silently send an existing credential to an edited host.
      if (!request.apiKey && existing && normalizeUrl(existing.baseUrl) !== baseUrl)
        throw new ProviderError("Enter an API Key again when changing the Base URL.");
      const key = request.apiKey?.trim() || (stored?.type === "api_key" ? stored.key : undefined);
      const data = await probe(baseUrl, key);
      const entry = { ...existing, baseUrl, modelApiOverrides: existing?.modelApiOverrides ?? {} };
      const models = buildProviderModels({
        providerName: id, baseUrl, apiModels: parseModelsResponse(data),
        ratios: { modelRatios: {}, completionRatios: {}, cacheRatios: {}, createCacheRatios: {} },
        modelApiOverrides: entry.modelApiOverrides,
      });
      register(id, entry, models);
      await runtime.login(id, "api_key", {
        prompt: async () => key, notify: () => {}, signal: AbortSignal.timeout(15000),
      });
      config.providers[id] = entry;
      await atomicJson(configPath, config);
      await runtime.refresh({ allowNetwork: false });
      statuses[id] = {
        state: "connected", checkedAt: Date.now(), modelCount: models.length,
        fingerprint: fingerprint(entry, await auth.read(id)),
      };
    } else {
      if (!existing) throw new ProviderError("Provider not found.");
      if (action === "refresh") {
        const credential = await auth.read(id);
        try {
          const baseUrl = normalizeUrl(existing.baseUrl);
          const data = await probe(baseUrl, credential?.type === "api_key" ? credential.key : undefined);
          const models = buildProviderModels({
            providerName: id, baseUrl, apiModels: parseModelsResponse(data),
            ratios: { modelRatios: {}, completionRatios: {}, cacheRatios: {}, createCacheRatios: {} },
            modelApiOverrides: existing.modelApiOverrides ?? {},
          });
          register(id, existing, models);
          await runtime.refresh({ allowNetwork: false });
          statuses[id] = { state: "connected", checkedAt: Date.now(), modelCount: models.length,
            fingerprint: fingerprint(existing, credential) };
        } catch (error) {
          statuses[id] = { state: "error", checkedAt: Date.now(), message: safeError(error),
            fingerprint: fingerprint(existing, credential) };
        }
      } else if (action === "logout" || action === "remove") {
        await runtime.logout(id);
        delete statuses[id];
        if (action === "remove") {
          delete config.providers[id];
          await atomicJson(configPath, config);
          runtime.unregisterProvider(id);
        }
      } else throw new ProviderError("Unsupported provider operation.");
    }
    await atomicJson(statusPath, statuses);
  }
  await runtime.refresh({ allowNetwork: false });
  const providers = [];
  for (const [id, entry] of Object.entries(config.providers)) {
    const credential = await auth.read(id);
    const saved = statuses[id];
    const status = saved?.fingerprint === fingerprint(entry, credential) ? saved : undefined;
    // Legacy plugin commands allowed arbitrary URLs. Never echo URL credentials.
    let displayUrl = "Invalid Base URL — edit this provider";
    try {
      const url = new URL(entry.baseUrl);
      url.username = ""; url.password = ""; url.search = ""; url.hash = "";
      displayUrl = url.toString().replace(/\/+$/, "");
    } catch {}
    providers.push({
      id, baseUrl: displayUrl, providerType: "newapi",
      credentialSaved: !!credential,
      state: credential ? (status?.state ?? "unverified") : "signed_out",
      checkedAt: status?.checkedAt, message: status?.message,
      modelCount: runtime.getModels(id).length,
    });
  }
  return { providers: providers.sort((a, b) => a.id.localeCompare(b.id)) };
}

// No raw exceptions: dependencies may put request headers/credentials in errors.
if (process.argv[1] && import.meta.url === pathToFileURL(await realpath(process.argv[1])).href) {
  let request;
  try {
    let input = "";
    for await (const chunk of process.stdin) {
      input += chunk;
      if (input.length > 65536) throw new ProviderError("Provider request is too large.");
    }
    request = JSON.parse(input);
    console.log = console.warn = console.error = () => {};
    const result = await providerRequest(request);
    process.stdout.write(JSON.stringify({ ok: true, data: result }));
  } catch (error) {
    let message = safeError(error);
    if (request?.apiKey) message = message.replaceAll(request.apiKey, "[redacted]");
    process.stdout.write(JSON.stringify({ ok: false, error: message }));
    process.exitCode = 1;
  }
}
