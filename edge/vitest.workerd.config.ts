import { defineConfig } from "vitest/config";
import { cloudflareTest } from "@cloudflare/vitest-pool-workers";

// Runtime-real test tier: runs inside actual workerd via
// @cloudflare/vitest-pool-workers, against a real SQLite-backed Durable
// Object, so platform limits like the ~2MB SQLITE_TOOBIG row cap are the
// runtime's own, not FakeSql constants. `npm run test:workerd`.
export default defineConfig({
  plugins: [
    cloudflareTest({
      main: "./test/workerd/fixture.ts",
      miniflare: {
        compatibilityDate: "2026-07-01",
        durableObjects: {
          TEST_LOG: { className: "TestLogRoom", useSQLite: true }
        }
      }
    })
  ],
  test: {
    include: ["test/workerd/**/*.test.ts"]
  }
});
