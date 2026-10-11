/**
 * Concrete paths for the OS sandbox. Landstrip takes exact paths (a glob is
 * expanded once per launch by walking the tree, seconds on a large repo), so
 * the patterns are resolved here instead:
 *
 * - `~/x` and `<root>/x`: the path itself; `~/x/**` and `<root>/x/**` its
 *   directory. Missing paths are kept for write rules (creating the file must
 *   be denied too) and dropped for read rules.
 * - a `*` inside one component: expanded by reading that directory.
 * - patterns that match anywhere: ripgrep's parallel walker over the workspace roots on
 *   every command (about 0.1 s for 100k files), and over the home directory
 *   through a cached index refreshed in the background.
 */

import { spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync, mkdirSync, renameSync } from "node:fs";
import { delimiter, dirname, join, relative, sep } from "node:path";
import { globBase, globToRegExp, hasGlob } from "./glob.ts";
import { expandPattern, isUnder, matchPatterns, compilePatterns } from "./paths.ts";

export interface SplitPatterns {
  /** Anywhere patterns without the leading `**` and `/`. */
  anywhere: string[];
  absolute: string[];
  exceptions: string[];
}

export function splitPatterns(patterns: readonly string[], roots: readonly string[], home: string): SplitPatterns {
  const out: SplitPatterns = { anywhere: [], absolute: [], exceptions: [] };
  for (const pattern of patterns) {
    for (const expanded of expandPattern(pattern, roots, home)) {
      if (expanded.startsWith("!")) out.exceptions.push(expanded.slice(1));
      else if (expanded.startsWith("**/")) out.anywhere.push(expanded.slice(3));
      else out.absolute.push(expanded);
    }
  }
  return out;
}

function exists(path: string): boolean {
  try {
    statSync(path);
    return true;
  } catch {
    return false;
  }
}

/** Expand globs inside single path components (`~/.cypher*` and the like) by reading directories. */
function expandComponents(pattern: string): string[] {
  const parts = pattern.split(sep).filter(Boolean);
  let current: string[] = [sep];
  for (const part of parts) {
    const next: string[] = [];
    for (const base of current) {
      if (!hasGlob(part)) {
        next.push(join(base, part));
        continue;
      }
      const regex = globToRegExp(part);
      let names: string[] = [];
      try {
        names = readdirSync(base);
      } catch {
        continue;
      }
      for (const name of names) if (regex.test(name)) next.push(join(base, name));
    }
    current = next;
    if (current.length === 0) break;
  }
  return current;
}

/**
 * Resolve one absolute pattern. `/**` at the end yields its directory; a `**`
 * elsewhere is left to ripgrep (returned as `deferred`).
 */
export function expandAbsolute(pattern: string, includeMissing: boolean): { paths: string[]; deferred?: { base: string; glob: string } } {
  let body = pattern;
  if (body.endsWith("/**")) body = body.slice(0, -3);
  if (body.includes("**")) {
    const base = globBase(body);
    return { paths: [], deferred: { base, glob: relative(base, pattern) } };
  }
  const candidates = hasGlob(body) ? expandComponents(body) : [body];
  return { paths: includeMissing ? candidates : candidates.filter(exists) };
}

let cachedRg: string | null | undefined;

/** Pi's managed ripgrep in the agent dir, else one on PATH. */
export function findRipgrep(agentDir: string | undefined): string | undefined {
  if (cachedRg !== undefined) return cachedRg ?? undefined;
  const candidates = [
    ...(agentDir ? [join(agentDir, "bin", "rg")] : []),
    ...(process.env.PATH ?? "").split(delimiter).filter(Boolean).map((dir) => join(dir, "rg")),
  ];
  cachedRg = candidates.find((candidate) => existsSync(candidate)) ?? null;
  return cachedRg ?? undefined;
}

/** For testing. */
export function resetRipgrepCache(): void {
  cachedRg = undefined;
}

const SKIP_IN_HOME = ["Library/Caches", ".Trash", "node_modules", ".npm", ".cache", ".cargo/registry", ".rustup", "go/pkg/mod", ".gradle/caches", ".pnpm-store"];

export interface ListOptions {
  rg?: string;
  /** Extra exclusion globs, e.g. caches when indexing the home directory. */
  exclude?: readonly string[];
  signal?: AbortSignal;
  timeoutMs?: number;
}

/** Files under `roots` matching any of `globs` (gitignore-style, relative to each root). */
export async function listMatching(globs: readonly string[], roots: readonly string[], options: ListOptions): Promise<string[]> {
  const existing = roots.filter(exists);
  if (globs.length === 0 || existing.length === 0) return [];
  if (!options.rg) return walkMatching(globs, existing, options.exclude ?? []);
  const args = ["--files", "--hidden", "--no-ignore", "--no-messages", "--no-config", "--null"];
  for (const glob of globs) args.push("--glob", glob);
  for (const glob of options.exclude ?? []) args.push("--glob", `!${glob}`);
  args.push("--", ...existing);
  return new Promise((resolvePaths) => {
    const child = spawn(options.rg!, args, { stdio: ["ignore", "pipe", "ignore"] });
    const chunks: Buffer[] = [];
    const timer = options.timeoutMs ? setTimeout(() => child.kill(), options.timeoutMs) : undefined;
    const onAbort = () => child.kill();
    options.signal?.addEventListener("abort", onAbort, { once: true });
    child.stdout.on("data", (chunk: Buffer) => chunks.push(chunk));
    const finish = () => {
      if (timer) clearTimeout(timer);
      options.signal?.removeEventListener("abort", onAbort);
      resolvePaths(Buffer.concat(chunks).toString("utf8").split("\0").filter(Boolean));
    };
    child.on("error", finish);
    child.on("close", finish);
  });
}

/** Fallback without ripgrep: a bounded recursive walk. */
function walkMatching(globs: readonly string[], roots: readonly string[], exclude: readonly string[]): string[] {
  const matchers = globs.map((glob) => globToRegExp(glob.includes("/") ? `/${glob}` : `/**/${glob}`));
  const skips = exclude.map((glob) => globToRegExp(`/${glob}`));
  const found: string[] = [];
  let budget = 200_000;
  for (const root of roots) {
    const stack = [root];
    while (stack.length > 0 && budget > 0) {
      const dir = stack.pop()!;
      let entries;
      try {
        entries = readdirSync(dir, { withFileTypes: true });
      } catch {
        continue;
      }
      for (const entry of entries) {
        if (--budget <= 0) break;
        const path = join(dir, entry.name);
        const rel = `/${relative(root, path)}`;
        if (skips.some((skip) => skip.test(rel))) continue;
        if (entry.isDirectory()) stack.push(path);
        else if (matchers.some((matcher) => matcher.test(rel))) found.push(path);
      }
    }
  }
  return found;
}

/**
 * For an anywhere pattern ending in `/**` (`.git/hooks/**`), the denied root is
 * the matching directory, not each file in it (a new file there is denied too).
 */
export function anchorDirectories(files: readonly string[], anywhere: readonly string[]): string[] {
  const dirPatterns = anywhere.filter((glob) => glob.endsWith("/**")).map((glob) => globToRegExp(`/**/${glob.slice(0, -3)}`));
  const filePatterns = anywhere.filter((glob) => !glob.endsWith("/**")).map((glob) => globToRegExp(`/**/${glob}`));
  const out = new Set<string>();
  for (const file of files) {
    if (filePatterns.some((pattern) => pattern.test(file))) out.add(file);
    if (dirPatterns.length === 0) continue;
    let ancestor = dirname(file);
    const chain: string[] = [];
    while (ancestor !== dirname(ancestor)) {
      chain.unshift(ancestor);
      ancestor = dirname(ancestor);
    }
    const dir = chain.find((candidate) => dirPatterns.some((pattern) => pattern.test(candidate)));
    if (dir) out.add(dir);
  }
  return [...out];
}

/** Turn anywhere patterns into ripgrep globs: a `/**` suffix lists the files beneath. */
export function ripgrepGlobs(anywhere: readonly string[]): string[] {
  return anywhere.map((glob) => (glob.includes("/") ? `**/${glob}` : glob));
}

export interface HomeIndex {
  createdAt: number;
  key: string;
  files: string[];
}

const HOME_INDEX_MAX_AGE_MS = 10 * 60 * 1000;

/**
 * Sensitive files matched anywhere in the home directory (outside the
 * workspace), cached across Pi processes. A stale index is used as is and
 * refreshed in the background; secrets created elsewhere since then are
 * denied to file tools at once and to bash after the next refresh.
 */
export class HomeIndexCache {
  private readonly file: string;
  private readonly home: string;
  private refreshing: Promise<void> | undefined;

  constructor(file: string, home: string) {
    this.file = file;
    this.home = home;
  }

  read(key: string): HomeIndex | undefined {
    try {
      const index = JSON.parse(readFileSync(this.file, "utf8")) as HomeIndex;
      if (index.key !== key || !Array.isArray(index.files)) return undefined;
      return index;
    } catch {
      return undefined;
    }
  }

  isFresh(index: HomeIndex | undefined): boolean {
    return index !== undefined && Date.now() - index.createdAt < HOME_INDEX_MAX_AGE_MS;
  }

  refresh(key: string, globs: readonly string[], rg: string | undefined): Promise<void> {
    this.refreshing ??= (async () => {
      const files = await listMatching(globs, [this.home], { rg, exclude: SKIP_IN_HOME, timeoutMs: 60_000 });
      const index: HomeIndex = { createdAt: Date.now(), key, files };
      try {
        mkdirSync(dirname(this.file), { recursive: true });
        const temporary = `${this.file}.tmp-${process.pid}`;
        writeFileSync(temporary, JSON.stringify(index), { mode: 0o600 });
        renameSync(temporary, this.file);
      } catch {
        // A read-only agent dir only costs the cache.
      }
    })().finally(() => {
      this.refreshing = undefined;
    });
    return this.refreshing;
  }
}

/** Filter discovered paths through a list's `!` exceptions. */
export function dropExceptions(paths: readonly string[], exceptions: readonly string[], roots: readonly string[], home: string): string[] {
  if (exceptions.length === 0) return [...paths];
  const compiled = compilePatterns(exceptions, roots, home);
  return paths.filter((path) => matchPatterns(path, compiled) === undefined);
}

/** Paths in `paths` that fall under one of `roots`. */
export function within(paths: readonly string[], roots: readonly string[]): string[] {
  return paths.filter((path) => roots.some((root) => isUnder(path, root)));
}
