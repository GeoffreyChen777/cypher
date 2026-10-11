/**
 * Canonical paths and zones. Every path is made absolute, `~` expanded, `..`
 * resolved and symlinks followed (through the nearest existing ancestor for a
 * path that does not exist yet) before it is classified:
 *
 *   sensitive  matches a sensitive pattern (checked first, always wins)
 *   workspace  under a workspace root
 *   external   otherwise
 *
 * On top of the zone, a path may be write-guarded: `protected` (a write runs
 * code outside the sandbox later; the judge may approve it) or `self`
 * (Cypher's and Pi's own data; only a human may approve a write).
 */

import { realpathSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve, sep } from "node:path";
import { globToRegExp } from "./glob.ts";
import type { Zone } from "./config.ts";

export function expandHome(path: string, home = homedir()): string {
  if (path === "~") return home;
  if (path.startsWith("~/")) return join(home, path.slice(2));
  return path;
}

/** Follow symlinks through the nearest existing ancestor; keep the missing tail as written. */
export function canonicalize(path: string, cwd: string, home = homedir()): string {
  const absolute = resolve(cwd, expandHome(path.startsWith("@") ? path.slice(1) : path, home));
  const tail: string[] = [];
  let current = absolute;
  for (;;) {
    try {
      const real = realpathSync.native(current);
      return tail.length === 0 ? real : join(real, ...tail.reverse());
    } catch {
      const parent = dirname(current);
      if (parent === current) return absolute;
      tail.push(basename(current));
      current = parent;
    }
  }
}

export function isUnder(path: string, root: string): boolean {
  if (path === root) return true;
  const prefix = root.endsWith(sep) ? root : `${root}${sep}`;
  return path.startsWith(prefix);
}

interface CompiledPattern {
  source: string;
  regex: RegExp;
  exception: boolean;
}

export type Guard = "protected" | "self";

export interface Classification {
  path: string;
  zone: Zone;
  guard?: Guard;
  /** The pattern that made the path sensitive or guarded, for dialogs and the audit. */
  match?: string;
}

/**
 * Expand a configured pattern against the home directory and workspace roots.
 * A pattern starting with `**` and `/` stays global, `<root>/x` becomes one
 * pattern per root, and a bare relative pattern matches anywhere.
 */
export function expandPattern(pattern: string, roots: readonly string[], home = homedir()): string[] {
  const exception = pattern.startsWith("!");
  const body = exception ? pattern.slice(1) : pattern;
  const prefix = exception ? "!" : "";
  if (body.startsWith("<root>/")) {
    return roots.map((root) => `${prefix}${join(root, body.slice("<root>/".length))}`);
  }
  if (body.startsWith("~/") || body === "~") return [`${prefix}${expandHome(body, home)}`];
  if (isAbsolute(body) || body.startsWith("**/")) return [`${prefix}${body}`];
  return [`${prefix}**/${body}`];
}

export function compilePatterns(patterns: readonly string[], roots: readonly string[], home = homedir()): CompiledPattern[] {
  return patterns.flatMap((pattern) =>
    expandPattern(pattern, roots, home).map((expanded) => {
      const exception = expanded.startsWith("!");
      const source = exception ? expanded.slice(1) : expanded;
      // `**/x` matches anywhere: anchor it on any directory boundary.
      const anchored = source.startsWith("**/") ? `/${source}` : source;
      return { source: pattern, regex: globToRegExp(anchored), exception };
    }),
  );
}

/** The first non-exception pattern that matches, unless an exception matches too. */
export function matchPatterns(path: string, compiled: readonly CompiledPattern[]): string | undefined {
  let hit: string | undefined;
  for (const pattern of compiled) {
    if (!pattern.regex.test(path)) continue;
    if (pattern.exception) return undefined;
    hit ??= pattern.source;
  }
  return hit;
}

export interface PathContextInput {
  roots: readonly string[];
  sensitive: readonly string[];
  protectedPatterns: readonly string[];
  self: readonly string[];
  writableSupport: readonly string[];
  home?: string;
}

export class PathContext {
  readonly roots: readonly string[];
  readonly home: string;
  readonly sensitive: CompiledPattern[];
  readonly protectedPatterns: CompiledPattern[];
  readonly self: CompiledPattern[];
  readonly writableSupport: CompiledPattern[];

  constructor(input: PathContextInput) {
    this.home = input.home ?? homedir();
    this.roots = input.roots;
    this.sensitive = compilePatterns(input.sensitive, input.roots, this.home);
    this.protectedPatterns = compilePatterns(input.protectedPatterns, input.roots, this.home);
    this.self = compilePatterns(input.self, input.roots, this.home);
    this.writableSupport = compilePatterns(input.writableSupport, input.roots, this.home);
  }

  canonical(path: string, cwd: string): string {
    return canonicalize(path, cwd, this.home);
  }

  inWorkspace(path: string): boolean {
    return this.roots.some((root) => isUnder(path, root));
  }

  isWritableSupport(path: string): boolean {
    return matchPatterns(path, this.writableSupport) !== undefined;
  }

  classify(path: string): Classification {
    const sensitive = matchPatterns(path, this.sensitive);
    if (sensitive) return { path, zone: "sensitive", match: sensitive };
    const zone: Zone = this.inWorkspace(path) ? "workspace" : "external";
    const self = matchPatterns(path, this.self);
    if (self) return { path, zone, guard: "self", match: self };
    const guarded = matchPatterns(path, this.protectedPatterns);
    if (guarded) return { path, zone, guard: "protected", match: guarded };
    return { path, zone };
  }
}
