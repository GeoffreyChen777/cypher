/**
 * Minimal WorkOS User Management REST client — the fetch-based port of the
 * old apps/server `WorkOsAuth` service (which used @workos-inc/node; the
 * Worker keeps it SDK-free). This is the one place that holds the WorkOS
 * **API key** (a Worker secret). Device backends build the public authorize
 * URL themselves and delegate the secret-bearing steps here, so the key never
 * lands on a device.
 *
 * Without WORKOS_API_KEY configured the routes answer 501; in dev mode
 * backends use their userId as the bearer and never call these.
 */
import type { Env } from "./env";

const API = "https://api.workos.com";

/** Stable machine-readable codes carried by [`WorkOsAuthError`]. Devices key
 * session-revocation off `invalid_grant` ALONE — every other code is
 * retryable, so a transient WorkOS/network hiccup can never clear a session. */
export type WorkOsErrorCode = "invalid_grant" | "rate_limited" | "network" | "upstream";

/** Typed WorkOS failure. Replaces the old blanket `WorkOsAuthFailed` → 401
 * mapping: carries the HTTP status to surface, a stable machine-readable
 * `code`, whether the attempt is retryable (false = permanent credential
 * rejection — the ONLY case a device may sign out), and a human-safe message
 * that never embeds upstream error bodies. */
export class WorkOsAuthError extends Error {
  readonly status: number;
  readonly code: WorkOsErrorCode;
  readonly retryable: boolean;

  constructor(status: number, code: WorkOsErrorCode, retryable: boolean, message: string) {
    super(message);
    this.name = "WorkOsAuthError";
    this.status = status;
    this.code = code;
    this.retryable = retryable;
  }
}

export interface ExchangeResult {
  readonly user: {
    readonly id: string;
    readonly email: string;
    readonly firstName: string | null;
    readonly lastName: string | null;
  };
  readonly accessToken: string;
  readonly refreshToken: string;
}

export interface RefreshResult {
  readonly accessToken: string;
  readonly refreshToken: string;
}

export interface OrgMembership {
  readonly id: string;
  readonly organizationId: string;
  readonly name: string;
}

interface WireUser {
  id: string;
  email: string;
  first_name: string | null;
  last_name: string | null;
}

interface WireAuthResponse {
  user: WireUser;
  access_token: string;
  refresh_token: string;
}

interface WireMembership {
  id: string;
  organization_id: string;
  organization_name?: string | null;
}

/** Parse a rejected WorkOS body for its OAuth `error` code without trusting
 * the rest; a non-JSON body is simply an unknown error. */
const readWireError = async (res: Response): Promise<{
  error?: string;
  error_description?: string;
  message?: string;
}> => {
  try {
    return (await res.json()) as { error?: string; error_description?: string; message?: string };
  } catch {
    return {};
  }
};

/** Map a rejected WorkOS response to a typed, status-preserving error.
 *
 *  - `invalid_grant` (expired/invalid auth code or refresh token — WorkOS's
 *    explicit credential rejection) → 401, permanent.
 *  - 429 rate limit → 429, retryable.
 *  - 5xx → 503, retryable.
 *  - anything else (unexpected 4xx, unknown error code, non-JSON body) → 502,
 *    retryable, WITHOUT the upstream body: only an explicit `invalid_grant`
 *    may clear a session, so every ambiguous failure is conservative. */
const failed = async (res: Response): Promise<never> => {
  const { error, error_description, message } = await readWireError(res);
  const status = res.status;
  if (error === "invalid_grant") {
    throw new WorkOsAuthError(
      401,
      "invalid_grant",
      false,
      error_description ?? message ?? "authentication rejected"
    );
  }
  if (status === 429) {
    throw new WorkOsAuthError(429, "rate_limited", true, "rate limited — try again later");
  }
  if (status >= 500) {
    throw new WorkOsAuthError(503, "upstream", true, "authentication service unavailable");
  }
  throw new WorkOsAuthError(
    502,
    "upstream",
    true,
    "authentication failed — please try again"
  );
};

/** fetch that turns transport failures (DNS, connection reset, timeouts) into
 * a typed, retryable 502 — never an untyped throw the routes can't classify. */
const fetchOrError = async (url: string, init: RequestInit): Promise<Response> => {
  try {
    return await fetch(url, init);
  } catch {
    throw new WorkOsAuthError(502, "network", true, "could not reach the authentication service");
  }
};

const post = async (apiKey: string, path: string, body: unknown): Promise<Response> =>
  fetchOrError(`${API}${path}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json"
    },
    body: JSON.stringify(body)
  });

/** `authenticateWithCode`: WorkOS code + PKCE verifier → tokens + user. The
 * verifier (`code_verifier`) is what proves this exchange belongs to the
 * authorize URL that published its S256 `code_challenge`; it must match the
 * challenge WorkOS issued. Neither the code nor the verifier is ever logged
 * here or by the caller. */
export const exchange = async (
  env: Env,
  apiKey: string,
  code: string,
  codeVerifier: string
): Promise<ExchangeResult> => {
  const res = await fetchOrError(`${API}/user_management/authenticate`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_id: env.WORKOS_CLIENT_ID,
      client_secret: apiKey,
      grant_type: "authorization_code",
      code,
      code_verifier: codeVerifier
    })
  });
  if (!res.ok) return failed(res);
  const r = (await res.json()) as WireAuthResponse;
  return {
    user: {
      id: r.user.id,
      email: r.user.email,
      firstName: r.user.first_name,
      lastName: r.user.last_name
    },
    accessToken: r.access_token,
    refreshToken: r.refresh_token
  };
};

/** `authenticateWithRefreshToken`; passing `organizationId` scopes the session
 * to that org (the next access token carries `org_id`). */
export const refresh = async (
  env: Env,
  apiKey: string,
  refreshToken: string,
  organizationId?: string
): Promise<RefreshResult> => {
  const res = await fetchOrError(`${API}/user_management/authenticate`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      client_id: env.WORKOS_CLIENT_ID,
      client_secret: apiKey,
      grant_type: "refresh_token",
      refresh_token: refreshToken,
      ...(organizationId ? { organization_id: organizationId } : {})
    })
  });
  if (!res.ok) return failed(res);
  const r = (await res.json()) as WireAuthResponse;
  return { accessToken: r.access_token, refreshToken: r.refresh_token };
};

/** The user's active organization memberships. */
export const listOrgs = async (apiKey: string, userId: string): Promise<OrgMembership[]> => {
  const params = new URLSearchParams({ user_id: userId, statuses: "active", limit: "100" });
  const res = await fetchOrError(
    `${API}/user_management/organization_memberships?${params}`,
    { headers: { authorization: `Bearer ${apiKey}` } }
  );
  if (!res.ok) return failed(res);
  const r = (await res.json()) as { data: WireMembership[] };
  return r.data.map((m) => ({
    id: m.id,
    organizationId: m.organization_id,
    name: m.organization_name ?? m.organization_id
  }));
};

/** Create an organization and make the user its first (admin) member. */
export const createOrg = async (
  apiKey: string,
  userId: string,
  name: string
): Promise<{ organizationId: string }> => {
  const orgRes = await post(apiKey, "/organizations", { name });
  if (!orgRes.ok) return failed(orgRes);
  const org = (await orgRes.json()) as { id: string };
  // The creator administers their workspace. Role slugs are per-environment
  // config, so fall back to the default role if "admin" doesn't exist rather
  // than failing the whole onboarding.
  const withRole = await post(apiKey, "/user_management/organization_memberships", {
    user_id: userId,
    organization_id: org.id,
    role_slug: "admin"
  });
  if (!withRole.ok) {
    const fallback = await post(apiKey, "/user_management/organization_memberships", {
      user_id: userId,
      organization_id: org.id
    });
    if (!fallback.ok) return failed(fallback);
  }
  return { organizationId: org.id };
};
