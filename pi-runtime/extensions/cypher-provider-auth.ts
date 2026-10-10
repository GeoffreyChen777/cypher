import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { AuthEvent, AuthPrompt } from "@earendil-works/pi-ai";

type RuntimeAuthBridge = {
  login(
    providerId: string,
    type: "api_key" | "oauth",
    interaction: {
      signal?: AbortSignal;
      prompt(prompt: AuthPrompt): Promise<string>;
      notify(event: AuthEvent): void;
    },
  ): Promise<unknown>;
  logout(providerId: string): Promise<void>;
  refresh(): Promise<unknown>;
};

function runtime(ctx: ExtensionContext): RuntimeAuthBridge {
  // Pi intentionally exposes ModelRegistry rather than ModelRuntime to
  // extensions. Cypher pins Pi with this runtime bundle and uses the registry's
  // backing runtime so RPC mode can provide the authentication flow that Pi's
  // built-in, TUI-only /login command normally owns.
  const value = (ctx.modelRegistry as unknown as { runtime?: RuntimeAuthBridge }).runtime;
  if (!value) throw new Error("Pi's provider authentication runtime is unavailable.");
  return value;
}

async function chooseProvider(args: string, ctx: ExtensionContext): Promise<string | undefined> {
  const explicit = args.trim();
  if (explicit) return explicit;
  const providers = [...ctx.modelRegistry.getRegisteredProviderIds()].sort();
  if (providers.length === 0) {
    ctx.ui.notify("No extension providers are configured.", "warning");
    return undefined;
  }
  return ctx.ui.select("Provider", providers);
}

async function answerPrompt(prompt: AuthPrompt, ctx: ExtensionContext): Promise<string> {
  if (prompt.type === "select") {
    const labels = prompt.options.map((option) => option.label);
    const picked = await ctx.ui.select(prompt.message, labels);
    const option = prompt.options.find((candidate) => candidate.label === picked);
    if (!option) throw new Error("Login cancelled.");
    return option.id;
  }
  const value = await ctx.ui.input(
    prompt.type === "secret" ? `${prompt.message} (kept out of chat)` : prompt.message,
    prompt.placeholder,
  );
  if (!value) throw new Error("Login cancelled.");
  return value;
}

function showAuthEvent(event: AuthEvent, ctx: ExtensionContext): void {
  if (event.type === "auth_url") {
    ctx.ui.notify(`${event.instructions ?? "Open this URL to authenticate"}: ${event.url}`, "info");
  } else if (event.type === "device_code") {
    ctx.ui.notify(
      `Open ${event.verificationUri} and enter code ${event.userCode}.`,
      "info",
    );
  } else {
    ctx.ui.notify(event.message, "info");
  }
}

export default function cypherProviderAuth(pi: ExtensionAPI): void {
  pi.registerCommand("provider", {
    description: "Open Settings → Providers (/provider add to add a service)",
    handler: async (_args, ctx) => {
      ctx.ui.notify("Manage model services in Cypher: Settings → Providers.", "info");
    },
  });
  pi.registerCommand("login", {
    description: "Authenticate a provider in Cypher's isolated Pi runtime",
    handler: async (args, ctx) => {
      if (ctx.mode === "rpc") {
        ctx.ui.notify("Authenticate in Cypher: Settings → Providers → Edit / authenticate.", "info");
        return;
      }
      const provider = await chooseProvider(args, ctx);
      if (!provider) return;
      if (!ctx.modelRegistry.getProvider(provider)) {
        ctx.ui.notify(`Provider "${provider}" is not registered.`, "error");
        return;
      }
      try {
        const auth = runtime(ctx);
        await auth.login(provider, "api_key", {
          signal: ctx.signal,
          prompt: (prompt) => answerPrompt(prompt, ctx),
          notify: (event) => showAuthEvent(event, ctx),
        });
        await auth.refresh();
        ctx.ui.notify(`Provider "${provider}" authenticated.`, "info");
      } catch (error) {
        ctx.ui.notify(error instanceof Error ? error.message : String(error), "error");
      }
    },
  });

  pi.registerCommand("logout", {
    description: "Remove a provider credential from Cypher's isolated Pi runtime",
    handler: async (args, ctx) => {
      if (ctx.mode === "rpc") {
        ctx.ui.notify("Log out in Cypher: Settings → Providers → Log out.", "info");
        return;
      }
      const provider = await chooseProvider(args, ctx);
      if (!provider) return;
      try {
        const auth = runtime(ctx);
        await auth.logout(provider);
        await auth.refresh();
        ctx.ui.notify(`Provider "${provider}" signed out.`, "info");
      } catch (error) {
        ctx.ui.notify(error instanceof Error ? error.message : String(error), "error");
      }
    },
  });
}
