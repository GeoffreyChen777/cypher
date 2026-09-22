// Build-time patch: serve claude-opus-5-5 on pi-claude-bridge at 1M context.
//
// Why this exists
// ---------------
// pi-claude-bridge owns no model list: its picker is pi-ai's anthropic catalog,
// which carries claude-opus-5-5 from 0.87.1 on. What the bridge does own is
// the set of models "measured" to serve the `[1m]` id (src/models.ts,
// MEASURED_ONE_M). Anything outside it is sent as the bare id and registered at
// 200K, so Opus 5.5 would lose its 1M window and pi would compact it at a
// fifth of what the model holds. We verified 1M against the host CLI, so add
// the row until upstream measures it.
//
// Lifetime
// --------
// Temporary. Once upstream lists claude-opus-5-5 the patch reports itself inert
// at build time, which is the signal to delete this file and its call in
// scripts/package-pi-runtime.sh.
//
// Failure policy: hard. A missing anchor means pi-claude-bridge changed shape,
// and silently shipping an unpatched bundle would drop Opus 5.5 to 200K with
// no signal. The packaging script runs under `set -e`.
//
// Usage: node pi-claude-bridge-opus-5-5.mjs <bridge-package-dir>

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const [, , bridgeDir] = process.argv;
if (!bridgeDir) {
  console.error("usage: pi-claude-bridge-opus-5-5.mjs <bridge-package-dir>");
  process.exit(1);
}

const target = join(bridgeDir, "src/models.ts");
if (!existsSync(target)) {
  console.error(`pi-claude-bridge patch: ${target} not found`);
  process.exit(1);
}

const MARKER = "CYPHER-RUNTIME-PATCH: opus-5-5";

// The registered contextWindow must match the window the bridge actually
// requests or pi's status bar and compaction threshold misreport; both derive
// from this one set, so the 1M id and the 1M registration move together.
const FROM = `const MEASURED_ONE_M = new Set([\n`;
const TO = `const MEASURED_ONE_M = new Set([\n\t"claude-opus-5-5", // ${MARKER} — verified 1M against the host CLI.\n`;

const source = readFileSync(target, "utf-8");
if (source.includes(MARKER)) {
  console.log("pi-claude-bridge patch: already applied");
  process.exit(0);
}
if (!source.includes(FROM)) {
  console.error(
    "pi-claude-bridge patch: anchor not found (MEASURED_ONE_M).\n" +
      "pi-claude-bridge changed shape — update dist/pi-runtime/patches/ before packaging.",
  );
  process.exit(1);
}
const set = source.slice(source.indexOf(FROM), source.indexOf("]);", source.indexOf(FROM)));
if (set.includes('"claude-opus-5-5"')) {
  console.log(
    "pi-claude-bridge patch: NOTE — pi-claude-bridge now measures claude-opus-5-5 at 1M. " +
      "Nothing to patch; delete dist/pi-runtime/patches/pi-claude-bridge-opus-5-5.mjs " +
      "and its call in scripts/package-pi-runtime.sh.",
  );
  process.exit(0);
}

writeFileSync(target, source.replace(FROM, TO));
console.log("pi-claude-bridge patch: claude-opus-5-5 served at 1M");
