/**
 * Shared pieces of the Worker's handler chain (index.ts). Every handler
 * answers `Response | undefined`; `undefined` passes the request on to the
 * next handler.
 */
import type { Verified } from "./auth/auth";
import { AUTH_USER_HEADER, json, type Env } from "./env";
import { ID_RE } from "./identifiers";

export interface RouteContext {
  request: Request;
  env: Env;
  url: URL;
  /** Non-empty path segments, still percent-encoded. */
  parts: string[];
}

/** A request whose bearer has been verified. */
export interface AuthedContext extends RouteContext {
  auth: Verified;
}

export const routeContext = (request: Request, env: Env): RouteContext => {
  const url = new URL(request.url);
  return { request, env, url, parts: url.pathname.split("/").filter(Boolean) };
};

/** Forward into a DO with the verified user stamped on the request. */
export const forward = (
  ns: DurableObjectNamespace,
  name: string,
  request: Request,
  userId: string,
  path: string,
  search?: string
): Promise<Response> => {
  const stub = ns.get(ns.idFromName(name));
  const url = new URL(request.url);
  url.pathname = path;
  if (search !== undefined) url.search = search;
  const headers = new Headers(request.headers);
  headers.set(AUTH_USER_HEADER, userId);
  return stub.fetch(new Request(url.toString(), { method: request.method, body: request.body, headers }));
};

/** 426 unless the request is a WebSocket upgrade. */
export const requireWebSocket = (request: Request): Response | undefined =>
  request.headers.get("upgrade")?.toLowerCase() === "websocket"
    ? undefined
    : json({ error: "expected websocket" }, 426);

/** `decodeURIComponent` that answers `undefined` for malformed %-escapes. */
export const safeDecode = (segment: string): string | undefined => {
  try {
    return decodeURIComponent(segment);
  } catch {
    return undefined;
  }
};

/** Carry the dialing engine's `&device=` through to the DO so sockets are
 * attributable in logs. Validated so a hand-crafted value can't inject into
 * log lines or the DO's query. */
export const deviceParam = (url: URL): string => {
  const device = url.searchParams.get("device") ?? "";
  return ID_RE.test(device) ? `&device=${device}` : "";
};
