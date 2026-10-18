/// <reference types="@cloudflare/vitest-pool-workers" />

declare global {
  namespace Cloudflare {
    interface Env {
      TEST_LOG: DurableObjectNamespace;
      BOT_ROOMS: DurableObjectNamespace;
      DEVICE_ROOMS: DurableObjectNamespace;
      AUTH_MODE: string;
    }
  }
}

export {};
