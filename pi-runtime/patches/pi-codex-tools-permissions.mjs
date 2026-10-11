// Build-time patch: route apply_patch's file reads and writes through
// Cypher's permission checks.
//
// Why this exists
// ---------------
// Cypher's permission extension (extensions/cypher-permissions.ts) checks
// every filesystem access of the built-in file tools at the operation itself,
// on the path actually touched. pi-codex-tools' `apply_patch` imports
// `node:fs/promises` directly and has no operations interface, so its writes
// would only see the extension's pre-check of the patch headers. The patch
// makes the four content operations (open, writeFile, mkdir, unlink) call a
// hook the extension installs on `globalThis` under a registered symbol; the
// metadata calls (lstat, readlink, realpath) stay direct. Without the
// extension the hook is absent and the functions behave exactly as before.
//
// The extension's file tools replace the built-ins under the same names, so
// pi-codex-tools no longer recognised them as native (`builtin:` source) and
// stopped hiding `edit`/`write` from grammar-capable OpenAI models, which
// should see `apply_patch` instead. The second edit treats the permission
// extension's copies as native.
//
// Lifetime
// --------
// Until pi-codex-tools offers pluggable operations. Pinned to the bundled
// version; a bump must re-check the anchors (cypher-permissions.staged.test.mjs
// applies a patch through the hook).
//
// Failure policy: hard. A missing anchor means the package changed shape.
//
// Usage: node pi-codex-tools-permissions.mjs <pi-codex-tools-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-codex-tools-permissions.mjs <pi-codex-tools-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: codex-tools-permissions";
const NAME = "pi-codex-tools";
requireVersion(packageDir, { name: NAME, expected: "0.4.0", script: "pi-codex-tools-permissions.mjs" });

const patchedOps = patchFile(packageDir, "src/apply-patch.ts", {
  name: NAME,
  marker: MARKER,
  edits: [
    {
      label: "fs imports",
      from: `import { lstat, readlink, realpath, open, mkdir, unlink, writeFile } from "node:fs/promises";`,
      to: `import * as nodeFs from "node:fs/promises";
// ${MARKER}: content operations go through Cypher's permission hook when
// the permissions extension installed one.
const { lstat, readlink, realpath } = nodeFs;
type CypherFsHook = Partial<Pick<typeof nodeFs, "open" | "mkdir" | "unlink" | "writeFile">>;
const cypherFs = (): CypherFsHook | undefined =>
  (globalThis as Record<symbol, CypherFsHook | undefined>)[Symbol.for("cypher.permissions.fs")];
const open = ((...args: Parameters<typeof nodeFs.open>) => (cypherFs()?.open ?? nodeFs.open)(...args)) as typeof nodeFs.open;
const mkdir = ((...args: Parameters<typeof nodeFs.mkdir>) => (cypherFs()?.mkdir ?? nodeFs.mkdir)(...args)) as typeof nodeFs.mkdir;
const unlink = ((...args: Parameters<typeof nodeFs.unlink>) => (cypherFs()?.unlink ?? nodeFs.unlink)(...args)) as typeof nodeFs.unlink;
const writeFile = ((...args: Parameters<typeof nodeFs.writeFile>) => (cypherFs()?.writeFile ?? nodeFs.writeFile)(...args)) as typeof nodeFs.writeFile;`,
    },
  ],
});

const patchedTools = patchFile(packageDir, "extensions/index.ts", {
  name: NAME,
  marker: MARKER,
  edits: [
    {
      label: "native file tools",
      from: "&& tools.some((tool) => tool.name === name && tool.sourceInfo?.path === `builtin:${name}`),",
      to: "&& tools.some((tool) => tool.name === name && (tool.sourceInfo?.path === `builtin:${name}`\n          // " + MARKER + ": Cypher's permission-checked copies of the built-ins.\n          || /cypher-permissions\\.ts$/.test(tool.sourceInfo?.path ?? \"\"))),",
    },
  ],
});
if (patchedOps || patchedTools) {
  console.log(`${NAME} patch: apply_patch file access goes through the permission hook (apply-patch.ts, index.ts)`);
}
