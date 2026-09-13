/// <reference types="@cloudflare/vitest-pool-workers" />

declare module "cloudflare:test" {
  interface ProvidedEnv {
    TEST_LOG: DurableObjectNamespace;
    TEST_SYNC3: DurableObjectNamespace;
    TEST_WORKSPACE3: DurableObjectNamespace;
  }
}
