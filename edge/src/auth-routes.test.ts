import { describe, expect, it, vi } from "vitest";
import { handleAuthRoute } from "./auth-routes";
import type { Env } from "./env";

/** The iOS callback touches no bindings; any env shape works. */
const env = {} as unknown as Env;

/** Drive `handleAuthRoute` directly with a real Request/URL pair. */
const call = (path: string, init?: RequestInit): Promise<Response | undefined> => {
  const url = new URL(`https://edge.letscypher.app${path}`);
  return handleAuthRoute(new Request(url, init), env, url);
};

/** A valid RFC 7636 verifier (43 chars of the unreserved alphabet). */
const VALID_VERIFIER = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-";

describe("POST /auth/exchange", () => {
  const workosEnv = {
    WORKOS_API_KEY: "sk_test",
    WORKOS_CLIENT_ID: "client_test",
    AUTH_MODE: "workos"
  } as unknown as Env;
  const exchange = (body: unknown): Promise<Response | undefined> =>
    handleAuthRoute(
      new Request("https://edge.letscypher.app/auth/exchange", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body)
      }),
      workosEnv,
      new URL("https://edge.letscypher.app/auth/exchange")
    );

  it("rejects a missing codeVerifier with 400", async () => {
    const res = await exchange({ code: "some-code" });
    expect(res?.status).toBe(400);
    expect(await res?.json()).toEqual({ error: "missing or invalid codeVerifier" });
  });

  it("rejects a missing code with 400", async () => {
    const res = await exchange({ codeVerifier: VALID_VERIFIER });
    expect(res?.status).toBe(400);
    expect(await res?.json()).toEqual({ error: "missing code" });
  });

  it("rejects an empty code with 400", async () => {
    const res = await exchange({ code: "", codeVerifier: VALID_VERIFIER });
    expect(res?.status).toBe(400);
  });

  it("rejects verifiers outside RFC 7636 length 43-128", async () => {
    for (const short of ["", "a".repeat(42), "a".repeat(129)]) {
      const res = await exchange({ code: "c", codeVerifier: short });
      expect(res?.status).toBe(400);
    }
  });

  it("rejects verifiers with non-unreserved characters", async () => {
    for (const bad of [
      `${VALID_VERIFIER.slice(0, -1)}+`,
      `${VALID_VERIFIER.slice(0, -1)}/`,
      `${VALID_VERIFIER.slice(0, -1)}=`,
      `${VALID_VERIFIER.slice(0, -1)} `,
      `${VALID_VERIFIER.slice(0, -1)}%`
    ]) {
      const res = await exchange({ code: "c", codeVerifier: bad });
      expect(res?.status).toBe(400);
    }
  });

  it("answers 501 when WorkOS is not configured", async () => {
    const res = await handleAuthRoute(
      new Request("https://edge.letscypher.app/auth/exchange", {
        method: "POST",
        body: JSON.stringify({ code: "c", codeVerifier: VALID_VERIFIER })
      }),
      env,
      new URL("https://edge.letscypher.app/auth/exchange")
    );
    expect(res?.status).toBe(501);
  });

  it("forwards code + code_verifier to the WorkOS authenticate call", async () => {
    const calls: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      async (url: string, init: { body: string }) => {
        calls.push({ url, body: JSON.parse(init.body) });
        return new Response(
          JSON.stringify({
            user: {
              id: "u1",
              email: "a@b.c",
              first_name: "Ann",
              last_name: "X",
              profile_picture_url: "https://avatars.example.com/a.png"
            },
            access_token: "at",
            refresh_token: "rt"
          }),
          { status: 200 }
        );
      }
    );
    try {
      const res = await exchange({ code: "some-code", codeVerifier: VALID_VERIFIER });
      expect(res?.status).toBe(200);
      const payload = (await res?.json()) as Record<string, unknown>;
      expect(payload.accessToken).toBe("at");
      expect(payload.refreshToken).toBe("rt");
      expect((payload.user as Record<string, unknown>).email).toBe("a@b.c");
      // The GitHub/WorkOS profile picture rides the exchange to the device.
      expect((payload.user as Record<string, unknown>).profilePictureUrl).toBe(
        "https://avatars.example.com/a.png"
      );
      // The single WorkOS call carries the PKCE verifier (snake_case) plus
      // the client credentials the edge alone holds.
      expect(calls).toHaveLength(1);
      expect(calls[0].url).toBe("https://api.workos.com/user_management/authenticate");
      const sent = calls[0].body as Record<string, unknown>;
      expect(sent.code).toBe("some-code");
      expect(sent.code_verifier).toBe(VALID_VERIFIER);
      expect(sent.grant_type).toBe("authorization_code");
      expect(sent.client_id).toBe("client_test");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("never logs the code or verifier, even on a WorkOS failure", async () => {
    // WorkOS rejects: the route must not echo the secrets into any sink.
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(JSON.stringify({ error: "invalid_grant", error_description: "expired" }), {
          status: 400
        })
    );
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const res = await exchange({ code: "super-secret-code", codeVerifier: VALID_VERIFIER });
      expect(res?.status).toBe(401);
      for (const call_ of warn.mock.calls) {
        const text = call_.join(" ");
        expect(text).not.toContain("super-secret-code");
        expect(text).not.toContain(VALID_VERIFIER);
      }
    } finally {
      warn.mockRestore();
      vi.unstubAllGlobals();
    }
  });

  it("sanitizes the profile picture to null when it is not a safe HTTPS URL", async () => {
    const bad = [
      // Non-HTTPS.
      "http://avatars.example.com/a.png",
      // Malformed / not a URL at all.
      "not a url",
      "https://",
      "../../etc/passwd",
      // Oversized (2048 chars is the cap).
      `https://avatars.example.com/${`a`.repeat(2048)}`,
      // Malformed ports / hosts and embedded credentials (userinfo).
      "https://avatars.example.com:99999/a.png",
      "https://user:pass@avatars.example.com/a.png"
    ];
    for (const profile_picture_url of bad) {
      vi.stubGlobal(
        "fetch",
        async () =>
          new Response(
            JSON.stringify({
              user: { id: "u1", email: "a@b.c", first_name: "Ann", last_name: "X", profile_picture_url },
              access_token: "at",
              refresh_token: "rt"
            }),
            { status: 200 }
          )
      );
      try {
        const res = await exchange({ code: "c", codeVerifier: VALID_VERIFIER });
        expect(res?.status).toBe(200);
        const payload = (await res?.json()) as Record<string, unknown>;
        const user = payload.user as Record<string, unknown>;
        expect(user.profilePictureUrl).toBeNull();
      } finally {
        vi.unstubAllGlobals();
      }
    }
  });

  it("maps invalid_grant to a permanent 401 with machine code", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(
          JSON.stringify({ error: "invalid_grant", error_description: "code expired" }),
          { status: 400 } // WorkOS answers 400 for a dead auth code too
        )
    );
    try {
      const res = await exchange({ code: "dead-code", codeVerifier: VALID_VERIFIER });
      expect(res?.status).toBe(401);
      expect(await res?.json()).toEqual({
        error: "code expired",
        code: "invalid_grant",
        retryable: false
      });
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("maps a WorkOS 429 to a retryable 429", async () => {
    vi.stubGlobal(
      "fetch",
      async () => new Response(JSON.stringify({ error: "rate_limit_exceeded" }), { status: 429 })
    );
    try {
      const res = await exchange({ code: "c", codeVerifier: VALID_VERIFIER });
      expect(res?.status).toBe(429);
      expect(await res?.json()).toEqual({
        error: "rate limited — try again later",
        code: "rate_limited",
        retryable: true
      });
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("maps a WorkOS 5xx to a retryable 503", async () => {
    for (const status of [500, 502, 503]) {
      vi.stubGlobal(
        "fetch",
        async () => new Response(JSON.stringify({ error: "boom" }), { status })
      );
      try {
        const res = await exchange({ code: "c", codeVerifier: VALID_VERIFIER });
        expect(res?.status).toBe(503);
        expect(await res?.json()).toEqual({
          error: "authentication service unavailable",
          code: "upstream",
          retryable: true
        });
      } finally {
        vi.unstubAllGlobals();
      }
    }
  });

  it("maps a network failure to a retryable 502", async () => {
    vi.stubGlobal("fetch", async () => {
      throw new TypeError("fetch failed");
    });
    try {
      const res = await exchange({ code: "c", codeVerifier: VALID_VERIFIER });
      expect(res?.status).toBe(502);
      expect(await res?.json()).toEqual({
        error: "could not reach the authentication service",
        code: "network",
        retryable: true
      });
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("maps an unexpected upstream rejection to a conservative 502 without leaking the body", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(JSON.stringify({ error: "some_internal_detail", message: "secret-ish" }), {
          status: 403
        })
    );
    try {
      const res = await exchange({ code: "c", codeVerifier: VALID_VERIFIER });
      expect(res?.status).toBe(502);
      const body = (await res?.json()) as Record<string, unknown>;
      expect(body.code).toBe("upstream");
      expect(body.retryable).toBe(true);
      // The upstream body is never echoed back to the client.
      expect(JSON.stringify(body)).not.toContain("some_internal_detail");
      expect(JSON.stringify(body)).not.toContain("secret-ish");
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

// ---------------------------------------------------------------------------
// POST /auth/refresh — the device-session semantics live here
// ---------------------------------------------------------------------------

describe("POST /auth/refresh", () => {
  const workosEnv = {
    WORKOS_API_KEY: "sk_test",
    WORKOS_CLIENT_ID: "client_test",
    AUTH_MODE: "workos"
  } as unknown as Env;
  const refresh = (body: unknown): Promise<Response | undefined> =>
    handleAuthRoute(
      new Request("https://edge.letscypher.app/auth/refresh", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body)
      }),
      workosEnv,
      new URL("https://edge.letscypher.app/auth/refresh")
    );

  it("maps invalid_grant to a permanent 401 that may clear a session", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(
          JSON.stringify({
            error: "invalid_grant",
            error_description: "refresh token expired or revoked"
          }),
          { status: 401 }
        )
    );
    try {
      const res = await refresh({ refreshToken: "rt-1" });
      expect(res?.status).toBe(401);
      expect(await res?.json()).toEqual({
        error: "refresh token expired or revoked",
        code: "invalid_grant",
        retryable: false
      });
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("maps a WorkOS 429 to a retryable 429 that must NOT clear a session", async () => {
    vi.stubGlobal(
      "fetch",
      async () => new Response(JSON.stringify({ error: "rate_limit_exceeded" }), { status: 429 })
    );
    try {
      const res = await refresh({ refreshToken: "rt-1" });
      expect(res?.status).toBe(429);
      expect(await res?.json()).toEqual({
        error: "rate limited — try again later",
        code: "rate_limited",
        retryable: true
      });
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("never logs the raw refresh token — only a short SHA-256 fingerprint", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(JSON.stringify({ error: "invalid_grant" }), { status: 401 })
    );
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const secret = "super-secret-refresh-token-0123456789abcdef";
      const res = await refresh({ refreshToken: secret });
      expect(res?.status).toBe(401);
      expect(warn).toHaveBeenCalled();
      for (const call_ of warn.mock.calls) {
        const text = call_.join(" ");
        expect(text).not.toContain(secret);
        // The old prefix (`token:super…`) must be gone; only a digest remains.
        expect(text).not.toContain("token:super");
        expect(text).not.toContain("len");
        expect(text).toMatch(/token:sha256:[0-9a-f]{16}/);
      }
    } finally {
      warn.mockRestore();
      vi.unstubAllGlobals();
    }
  });

  it("rejects a malformed organizationId locally with 400", async () => {
    const res = await refresh({ refreshToken: "rt-1", organizationId: 7 });
    expect(res?.status).toBe(400);
    expect(await res?.json()).toEqual({ error: "organizationId must be a string" });
  });

  it("surfaces the refresh-grant user so devices can refresh the avatar", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(
          JSON.stringify({
            user: {
              id: "u1",
              email: "a@b.c",
              first_name: "Ann",
              last_name: "X",
              profile_picture_url: "https://avatars.example.com/new.png"
            },
            access_token: "at",
            refresh_token: "rt"
          }),
          { status: 200 }
        )
    );
    try {
      const res = await refresh({ refreshToken: "rt-1" });
      expect(res?.status).toBe(200);
      const payload = (await res?.json()) as Record<string, unknown>;
      expect(payload.accessToken).toBe("at");
      expect(payload.refreshToken).toBe("rt");
      const user = payload.user as Record<string, unknown>;
      expect(user.id).toBe("u1");
      expect(user.profilePictureUrl).toBe("https://avatars.example.com/new.png");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("omits the nested user when WorkOS returns none (devices preserve the avatar)", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(
          JSON.stringify({ access_token: "at", refresh_token: "rt" }),
          { status: 200 }
        )
    );
    try {
      const res = await refresh({ refreshToken: "rt-1" });
      expect(res?.status).toBe(200);
      const payload = (await res?.json()) as Record<string, unknown>;
      expect(payload.accessToken).toBe("at");
      expect(payload.refreshToken).toBe("rt");
      // Absent (not null): an old edge / omitted user must never clear it.
      expect("user" in payload).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("sanitizes an unsafe refresh profile picture to null (explicit clear)", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(
          JSON.stringify({
            user: { id: "u1", email: "a@b.c", first_name: "Ann", last_name: "X", profile_picture_url: "http://avatars.example.com/a.png" },
            access_token: "at",
            refresh_token: "rt"
          }),
          { status: 200 }
        )
    );
    try {
      const res = await refresh({ refreshToken: "rt-1" });
      const payload = (await res?.json()) as Record<string, unknown>;
      const user = payload.user as Record<string, unknown>;
      expect(user.profilePictureUrl).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

// ---------------------------------------------------------------------------
// iOS + CLI callback bridges (the pre-existing bridge security surface)
// ---------------------------------------------------------------------------

describe("GET /auth/ios/callback", () => {
  it("302s to cypher://callback preserving the full query (code + state)", async () => {
    const res = await call("/auth/ios/callback?state=abc&code=xyz");
    expect(res?.status).toBe(302);
    expect(res?.headers.get("location")).toBe("cypher://callback?state=abc&code=xyz");
  });

  it("forwards OAuth error fields verbatim (error_description etc)", async () => {
    const res = await call(
      "/auth/ios/callback?state=abc&error=access_denied&error_description=User%20declined&error_code=1"
    );
    expect(res?.status).toBe(302);
    expect(res?.headers.get("location")).toBe(
      "cypher://callback?state=abc&error=access_denied&error_description=User%20declined&error_code=1"
    );
  });

  it("requires non-empty state", async () => {
    expect((await call("/auth/ios/callback?code=xyz"))?.status).toBe(400);
    expect((await call("/auth/ios/callback?state=&code=xyz"))?.status).toBe(400);
  });

  it("requires non-empty code or error", async () => {
    expect((await call("/auth/ios/callback?state=abc"))?.status).toBe(400);
    expect((await call("/auth/ios/callback?state=abc&code="))?.status).toBe(400);
  });

  it("answers malformed input with a plain-text 400", async () => {
    const res = await call("/auth/ios/callback");
    expect(res?.status).toBe(400);
    expect(res?.headers.get("content-type")).toContain("text/plain");
  });

  it("carries no-store/no-referrer/nosniff on the malformed 400 too", async () => {
    const res = await call("/auth/ios/callback");
    expect(res?.status).toBe(400);
    expect(res?.headers.get("cache-control")).toBe("no-store, max-age=0");
    expect(res?.headers.get("referrer-policy")).toBe("no-referrer");
    expect(res?.headers.get("x-content-type-options")).toBe("nosniff");
  });

  it("sends no-store cache-control, no-referrer, and nosniff headers", async () => {
    const res = await call("/auth/ios/callback?state=abc&code=xyz");
    expect(res?.headers.get("cache-control")).toBe("no-store, max-age=0");
    expect(res?.headers.get("referrer-policy")).toBe("no-referrer");
    expect(res?.headers.get("x-content-type-options")).toBe("nosniff");
  });

  it("is not an open redirect: `next` is never the target", async () => {
    const res = await call(
      "/auth/ios/callback?state=abc&code=xyz&next=https://evil.example/steal"
    );
    expect(res?.status).toBe(302);
    // The target is fixed: whatever else the query carries, the redirect
    // host is exactly the app's callback scheme, never the attacker URL.
    const target = new URL(res!.headers.get("location")!);
    expect(target.protocol).toBe("cypher:");
    expect(target.host).toBe("callback");
  });

  it("is not handled for non-GET methods (router falls through to 404)", async () => {
    expect(await call("/auth/ios/callback?state=abc&code=xyz", { method: "POST" })).toBeUndefined();
    expect(await call("/auth/ios/callback?state=abc&code=xyz", { method: "PUT" })).toBeUndefined();
  });

  it("is not handled for extra path segments", async () => {
    expect(await call("/auth/ios/callback/extra?state=abc&code=xyz")).toBeUndefined();
  });

  it("leaves non-auth paths alone", async () => {
    expect(await call("/health")).toBeUndefined();
    expect(await call("/auth/unknown-route")).toBeUndefined();
  });
});

describe("GET /auth/cli/callback", () => {
  it("carries no-store/no-referrer/nosniff on the paste-code page", async () => {
    const res = await call("/auth/cli/callback?code=abc&state=xyz");
    expect(res?.status).toBe(200);
    expect(res?.headers.get("cache-control")).toBe("no-store, max-age=0");
    expect(res?.headers.get("referrer-policy")).toBe("no-referrer");
    expect(res?.headers.get("x-content-type-options")).toBe("nosniff");
  });

  it("locks the page down with a CSP that still allows the copy button", async () => {
    const res = await call("/auth/cli/callback?code=abc&state=xyz");
    expect(res?.headers.get("content-security-policy")).toBe(
      "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'"
    );
    const page = await res?.text();
    // The inline <style> block and the inline onclick copy button survive.
    expect(page).toContain("<style>");
    expect(page).toContain("onclick=\"navigator.clipboard.writeText");
  });

  it("carries the same header set on the malformed 400 page", async () => {
    const res = await call("/auth/cli/callback");
    expect(res?.status).toBe(400);
    expect(res?.headers.get("cache-control")).toBe("no-store, max-age=0");
    expect(res?.headers.get("referrer-policy")).toBe("no-referrer");
    expect(res?.headers.get("x-content-type-options")).toBe("nosniff");
    expect(res?.headers.get("content-security-policy")).toBe(
      "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'"
    );
  });

  it("is not handled for extra path segments", async () => {
    expect(await call("/auth/cli/callback/extra?code=abc&state=xyz")).toBeUndefined();
  });
});
