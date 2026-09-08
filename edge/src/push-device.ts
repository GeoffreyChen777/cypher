/** Global token ownership: stale per-user rooms cannot deliver after logout
 * or account switching. Registration/send are internal; public revocation
 * requires the exact opaque lease capability. */
import type { Env } from "./env";
import { sendAPNs, type PushMessage } from "./apns";
import { notificationJSON as json, object, identifier, readNotificationJSON } from "./notifications-model";

interface Registration {
  scope: string; installationId: string; epoch: number; lease: string; active: boolean;
  token: string; environment: "development" | "production"; retired: string[];
}
export class PushDevice implements DurableObject {
  constructor(private ctx: DurableObjectState, private env: Env) {}
  fetch(request: Request): Promise<Response> {
    // Do not hold blockConcurrencyWhile across Apple's external fetch:
    // Cloudflare can suspend outbound I/O while a DO input gate is held,
    // turning every APNs request into the exact 10s timeout. Delivery state
    // and the lease check below still make concurrent sends safe.
    if (new URL(request.url).pathname === "/send") return this.handle(request);
    return this.ctx.blockConcurrencyWhile(() => this.handle(request));
  }
  private async handle(request: Request): Promise<Response> {
    let body: Record<string, unknown>;
    try { body = object(await readNotificationJSON(request)); } catch { return json({ error: "invalid" }, 400); }
    if (request.method !== "POST" || typeof body.scope !== "string" || !/^[a-f0-9]{64}$/.test(body.scope)) {
      return json({ error: "invalid" }, 400);
    }
    const path = new URL(request.url).pathname;
    const current = await this.ctx.storage.get<Registration>("registration");
    if (path === "/register") {
      try {
        const installationId = identifier(body.installationId);
        const epoch = body.epoch;
        if (typeof epoch !== "number" || !Number.isSafeInteger(epoch) || epoch < 1 ||
            typeof body.token !== "string" || !/^(?:[a-f0-9]{2}){16,128}$/.test(body.token) ||
            typeof body.environment !== "string" || !["development", "production"].includes(body.environment)) throw new Error();
        if (current?.retired.includes(installationId) ||
            await this.ctx.storage.get<boolean>(`retired:${installationId}`)) return json({ error: "stale" }, 409);
        if (current?.installationId === installationId) {
          if (epoch < current.epoch || (epoch === current.epoch &&
              (current.scope !== body.scope || !current.active))) return json({ error: "stale" }, 409);
          if (epoch === current.epoch) return json({ lease: current.lease });
        }
        const registration: Registration = {
          scope: body.scope, installationId, epoch, token: String(body.token),
          environment: body.environment as Registration["environment"], active: true,
          lease: crypto.randomUUID(), retired: current?.retired ?? []
        };
        // Security watermarks must not age out: otherwise an old installation
        // could reclaim a token after enough subsequent reinstallations.
        await this.ctx.storage.put({
          registration,
          ...(current && current.installationId !== installationId
            ? { [`retired:${current.installationId}`]: true } : {})
        });
        return json({ lease: registration.lease });
      } catch { return json({ error: "invalid" }, 400); }
    }
    if (!current || current.scope !== body.scope || current.lease !== body.lease || !current.active) {
      return json({ permanent: true });
    }
    if (path === "/unregister") {
      if (typeof body.epoch !== "number" || !Number.isSafeInteger(body.epoch) || body.epoch <= current.epoch) {
        return json({ error: "stale" }, 409);
      }
      await this.ctx.storage.put("registration", { ...current, token: "", active: false, epoch: body.epoch });
      return json({ ok: true });
    }
    if (path !== "/send") return json({ error: "not found" }, 404);
    const message = body.message as PushMessage | undefined;
    if (!message || message.scope !== current.scope || !/^[a-f0-9-]{36}$/.test(message.id) ||
        !["completed", "failed", "input"].includes(message.kind)) return json({ error: "invalid" }, 400);
    const key = `delivery:${message.id}`;
    const previous = await this.ctx.storage.get<{ state: string; at: number }>(key);
    if (previous?.state === "sent") return json({ sent: true });
    if (previous?.state === "sending" && Date.now() - previous.at < 20_000) return json({ retry: true });
    if (message.expires <= Date.now()) return json({ permanent: true });
    await this.ctx.storage.put(key, { state: "sending", at: Date.now() });
    const outcome = await sendAPNs(this.env, current.token, current.environment, message);
    await this.ctx.storage.put(key, { state: outcome, at: Date.now() });
    if (outcome === "invalid") {
      // A re-registration may have happened while APNs was responding.
      const latest = await this.ctx.storage.get<Registration>("registration");
      if (latest?.lease === current.lease) await this.ctx.storage.put("registration", { ...latest, token: "", active: false });
    }
    const deliveries = await this.ctx.storage.list<{ at: number }>({ prefix: "delivery:" });
    const expired = [...deliveries].filter(([, v]) => v.at < Date.now() - 86_400_000).map(([k]) => k);
    if (deliveries.size > 512) expired.push(...[...deliveries].slice(0, deliveries.size - 512).map(([k]) => k));
    if (expired.length) await this.ctx.storage.delete([...new Set(expired)]);
    return json(outcome === "sent" ? { sent: true } : outcome === "invalid" ? { permanent: true } : { retry: true });
  }
}
