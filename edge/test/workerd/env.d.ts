/// <reference types="@cloudflare/vitest-pool-workers" />

declare global {
  namespace Cloudflare {
    interface Env {
      TEST_LOG: DurableObjectNamespace;
    }
  }
}

export {};
