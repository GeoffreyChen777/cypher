/**
 * The /auth/* HTTP surface absorbed from zeron's apps/server:
 *
 *  - POST /auth/exchange     — WorkOS code → tokens (see `workos.ts`).
 *  - POST /auth/refresh      — WorkOS refresh → fresh tokens (org-scopable).
 *  - GET  /auth/orgs         — the caller's active org memberships.
 *  - POST /auth/orgs         — create an org + first (admin) membership.
 *  - GET  /auth/cli/callback — headless sign-in: shows a paste-able code.
 *  - GET  /auth/ios/callback — iOS bridge: 302 → cypher://callback (query intact).
 *
 * Exchange/refresh/callback run BEFORE the bearer gate (the caller has no
 * access token yet); the org routes verify the bearer themselves — the user
 * id is ALWAYS the token's `sub`, never request input: users manage their own
 * memberships and no one else's. Error mapping: bad body 400, missing bearer
 * 401, WorkOS-off 501, and WorkOS failures map through [`WorkOsAuthError`] —
 * an explicit `invalid_grant` is a permanent 401, everything else (429, 5xx,
 * network) is a retryable 429/502/503 so a transient blip can never revoke a
 * device session.
 */
import { bearerFromRequest, verifyToken } from "./auth";
import type { Env } from "./env";
import { WorkOsAuthError, createOrg, exchange, listOrgs, refresh } from "./workos";

const json = (value: unknown, status = 200): Response =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" }
  });

const notConfigured = (): Response => json({ error: "workos not configured" }, 501);

/** Map a WorkOS failure to the typed JSON envelope `{error, code, retryable}`
 * that devices parse. The machine `code` is what distinguishes a permanent
 * credential rejection (`invalid_grant`) from a retryable blip; the `error`
 * string is always a safe message. An unclassified failure answers a
 * conservative retryable 502 without leaking internals. */
const authFailed = (e: unknown): Response =>
  e instanceof WorkOsAuthError
    ? json({ error: e.message, code: e.code, retryable: e.retryable }, e.status)
    : json(
        { error: "authentication failed — please try again", code: "upstream", retryable: true },
        502
      );

/** Short SHA-256 fingerprint for log attribution of a credential WITHOUT
 * ever exposing it (a refresh token is single-use and would rotate anyway). */
const fingerprint = async (value: string): Promise<string> => {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .slice(0, 16)
    .join("");
};

const bodyJson = async <T>(request: Request): Promise<T | undefined> => {
  try {
    return (await request.json()) as T;
  } catch {
    return undefined;
  }
};

/** RFC 7636 §4.1 verifier: 43–128 characters of the unreserved URL-safe
 * alphabet. Rejected here (400) before the code ever reaches WorkOS. */
const PKCE_VERIFIER_RE = /^[A-Za-z0-9\-._~]{43,128}$/;

/** Handle an /auth/* route; undefined means "not an auth route". */
export const handleAuthRoute = async (
  request: Request,
  env: Env,
  url: URL
): Promise<Response | undefined> => {
  const parts = url.pathname.split("/").filter(Boolean);
  if (parts[0] !== "auth") return undefined;
  const apiKey = env.WORKOS_API_KEY;

  if (parts[1] === "exchange" && parts.length === 2 && request.method === "POST") {
    if (!apiKey) return notConfigured();
    const body = await bodyJson<{ code?: string; codeVerifier?: string }>(request);
    if (typeof body?.code !== "string" || body.code.length === 0)
      return json({ error: "missing code" }, 400);
    if (typeof body?.codeVerifier !== "string" || !PKCE_VERIFIER_RE.test(body.codeVerifier))
      return json({ error: "missing or invalid codeVerifier" }, 400);
    // The code + verifier go straight to WorkOS; neither is ever logged.
    try {
      return json(await exchange(env, apiKey, body.code, body.codeVerifier));
    } catch (e) {
      return authFailed(e);
    }
  }

  if (parts[1] === "refresh" && parts.length === 2 && request.method === "POST") {
    if (!apiKey) return notConfigured();
    const body = await bodyJson<{ refreshToken?: string; organizationId?: string }>(request);
    if (typeof body?.refreshToken !== "string") return json({ error: "missing refreshToken" }, 400);
    if (body.organizationId !== undefined && typeof body.organizationId !== "string") {
      return json({ error: "organizationId must be a string" }, 400);
    }
    try {
      return json(await refresh(env, apiKey, body.refreshToken, body.organizationId));
    } catch (e) {
      // Identify repeat offenders: a client with a revoked session retries
      // every 30s forever and is otherwise anonymous in the tail. The raw
      // refresh token is NEVER logged — only a short SHA-256 fingerprint.
      console.warn(
        "auth/refresh failed",
        request.headers.get("cf-connecting-ip") ?? "unknown-ip",
        `token:sha256:${await fingerprint(body.refreshToken)}`,
        e instanceof WorkOsAuthError ? e.code : "unknown"
      );
      return authFailed(e);
    }
  }

  if (parts[1] === "orgs" && parts.length === 2) {
    if (!apiKey) return notConfigured();
    const token = bearerFromRequest(request);
    const caller = token ? await verifyToken(env, token) : undefined;
    if (!caller) return json({ error: "invalid or missing bearer token" }, 401);
    if (request.method === "GET") {
      try {
        return json({ orgs: await listOrgs(apiKey, caller.userId) });
      } catch (e) {
        return authFailed(e);
      }
    }
    if (request.method === "POST") {
      const body = await bodyJson<{ name?: string }>(request);
      if (typeof body?.name !== "string") return json({ error: "missing name" }, 400);
      const trimmed = body.name.trim();
      if (trimmed.length === 0 || trimmed.length > 80) {
        return json({ error: "name must be 1-80 characters" }, 400);
      }
      try {
        return json(await createOrg(apiKey, caller.userId, trimmed));
      } catch (e) {
        return authFailed(e);
      }
    }
  }

  if (parts[1] === "cli" && parts[2] === "callback" && parts.length === 3 && request.method === "GET") {
    return cliCallback(url);
  }

  if (parts[1] === "ios" && parts[2] === "callback" && parts.length === 3 && request.method === "GET") {
    return iosCallback(url);
  }

  return undefined;
};

// ---------------------------------------------------------------------------
// iOS OAuth callback bridge
// ---------------------------------------------------------------------------

/** The ONLY redirect target this route can produce — the iOS app's
 * ASWebAuthenticationSession callback scheme. WorkOS redirects here on the
 * https edge; the app's session picks the flow back up at `cypher://callback`
 * with the full query intact. No `next`-style parameter is ever consulted, so
 * an attacker-supplied URL can never turn this into an open redirect. */
const IOS_CALLBACK = "cypher://callback";

const badIosCallback = (): Response =>
  new Response("missing state or code/error", {
    status: 400,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": "no-store, max-age=0",
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff"
    }
  });

/**
 * WorkOS AuthKit → iOS bridge. Registered as a WorkOS redirect URI; it does
 * NOT exchange the code (the app does that via POST /auth/exchange, so the
 * tokens land on the device) and never consumes or alters the query — the
 * original string (code, state, and any OAuth error fields) passes through
 * verbatim, so the app's state check still holds on the other side.
 */
const iosCallback = (url: URL): Response => {
  const state = url.searchParams.get("state");
  const code = url.searchParams.get("code");
  const error = url.searchParams.get("error");
  if (!state || (!code && !error)) return badIosCallback();
  return new Response(null, {
    status: 302,
    headers: {
      location: `${IOS_CALLBACK}${url.search}`,
      "cache-control": "no-store, max-age=0",
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff"
    }
  });
};

// ---------------------------------------------------------------------------
// Headless sign-in callback
// ---------------------------------------------------------------------------

/** Query params land verbatim in the page — escape them. (WorkOS codes/states
 * are URL-safe tokens, but this URL accepts anything.) */
const escapeHtml = (s: string): string =>
  s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");

const cliPage = (body: string): string => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<meta name="robots" content="noindex" />
<title>Cypher — sign in</title>
<style>
  body { margin: 0; min-height: 100vh; display: grid; place-items: center;
         background: #0a0a0a; color: #ededed;
         font: 15px/1.6 ui-sans-serif, system-ui, sans-serif; }
  main { max-width: 34rem; padding: 2rem; text-align: center; }
  h1 { font-size: 1.05rem; font-weight: 600; margin: 0 0 0.75rem; }
  p { color: #a1a1a1; margin: 0.25rem 0; }
  code#paste { display: block; margin: 1.25rem 0 0.75rem; padding: 0.9rem 1rem;
         background: #171717; border: 1px solid #2e2e2e; border-radius: 8px;
         font: 13px/1.5 ui-monospace, monospace; word-break: break-all;
         user-select: all; cursor: pointer; }
  button { margin-top: 0.25rem; padding: 0.45rem 1rem; border-radius: 8px;
         border: 1px solid #2e2e2e; background: #ededed; color: #0a0a0a;
         font: 500 13px ui-sans-serif, system-ui, sans-serif; cursor: pointer; }
</style>
</head>
<body><main>${body}</main></body>
</html>`;

const html = (body: string, status = 200): Response =>
  new Response(body, {
    status,
    headers: {
      "content-type": "text/html; charset=utf-8",
      // Lock the page down: no external fetch targets at all, inline style
      // (the page's <style> block) and inline script (the copy button's
      // onclick) stay allowed. The paste-code page carries a sign-in
      // code/state in its URL and is one-shot: never cache it, and don't leak
      // the query string via Referer.
      "content-security-policy":
        "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'",
      "cache-control": "no-store, max-age=0",
      "referrer-policy": "no-referrer",
      "x-content-type-options": "nosniff"
    }
  });

/**
 * The hosted OAuth callback for headless (paste-code) sign-in. Registered as a
 * WorkOS redirect URI; it does NOT exchange the code — it renders `state.code`
 * for the user to paste into the device that started the flow (`cypher login`),
 * where the exchange runs so the tokens land on that machine. The state half
 * must match the pending sign-in there, so the paste is CSRF-checked at the
 * same point the loopback flow is.
 */
const cliCallback = (url: URL): Response => {
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  const denied = url.searchParams.get("error");
  if (denied || !code || !state) {
    const detail = denied
      ? `Sign-in was not completed (${escapeHtml(denied)}).`
      : "This link is missing its sign-in code.";
    return html(
      cliPage(`<h1>Sign-in failed</h1><p>${detail}</p><p>Start again from your terminal.</p>`),
      400
    );
  }
  const paste = `${escapeHtml(state)}.${escapeHtml(code)}`;
  return html(
    cliPage(
      `<h1>Almost there</h1>
<p>Paste this code into the terminal that asked for it:</p>
<code id="paste">${paste}</code>
<button onclick="navigator.clipboard.writeText(document.getElementById('paste').textContent).then(()=>{this.textContent='Copied'})">Copy code</button>
<p style="margin-top:1rem;font-size:13px">This code expires in a few minutes and only works on the device that started sign-in.</p>`
    )
  );
};
