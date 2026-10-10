// Shared mechanics of the build-time runtime patches: the package-dir
// argument, the version pin, marker idempotency and unique-anchor
// replacement. Every failure prints one message and exits 1; the packaging
// script runs the patches under `set -e`.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

const fail = (message) => {
  console.error(message);
  process.exit(1);
};

/** The package directory argument; exits with `usage: <usage>` without one. */
export function packageDirArg(usage) {
  const [, , packageDir] = process.argv;
  if (!packageDir) fail(`usage: ${usage}`);
  return packageDir;
}

/** Exits unless `packageDir` holds exactly `expected` of `name`. */
export function requireVersion(packageDir, { name, expected, script }) {
  const version = JSON.parse(readFileSync(join(packageDir, "package.json"), "utf-8")).version;
  if (version !== expected) {
    fail(
      `${name} patch: expected ${expected}, found ${version}.\n` +
        `Re-check pi-runtime/patches/${script} against the new version.`,
    );
  }
}

/** Applies `edits` ({label, from, to}, in order) to `packageDir/file`. Each
 * `from` must occur exactly once at its turn. Returns false, writing nothing,
 * when `marker` shows the file is already patched. */
export function patchFile(packageDir, file, { name, marker, edits }) {
  const target = join(packageDir, file);
  if (!existsSync(target)) fail(`${name} patch: ${target} not found`);
  const source = readFileSync(target, "utf-8");
  if (source.includes(marker)) return false;
  let out = source;
  for (const { label, from, to } of edits) {
    const at = out.indexOf(from);
    if (at < 0 || out.indexOf(from, at + 1) >= 0) {
      fail(
        `${name} patch: anchor ${at < 0 ? "not found" : "not unique"} (${basename(file)}: ${label}).\n` +
          `${name} changed shape — update pi-runtime/patches/ before packaging.`,
      );
    }
    out = out.replace(from, to);
  }
  writeFileSync(target, out);
  return true;
}
