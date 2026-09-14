import { readFileSync } from "node:fs";
import base from "./vitest.workerd.config";
import { defineConfig } from "vitest/config";

const path = process.env.CYPHER_ROWS_FIXTURE;
if (!path) throw new Error("Run scripts/rows-written-baseline.sh to generate the Loro fixture");
export default defineConfig({
  ...base,
  define: { __ROWS_FIXTURE__: readFileSync(path, "utf8") },
  test: { include: ["test/workerd/rows-written.baseline.ts"], reporters: ["default", "./test/rows-reporter.mjs"] }
});
