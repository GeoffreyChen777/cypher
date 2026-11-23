/**
 * Canonical paths and zones. Every path is made absolute, `~` expanded, `..`
 * resolved and symlinks followed (through the nearest existing ancestor for a
 * path that does not exist yet) before it is classified:
 *
 *   sensitive  matches a sensitive pattern (checked first, always wins)
 *   workspace  under a workspace root, or in the temp directories: the
 *              scratch space sandboxed commands write too (`scratch`)
 *   external   otherwise
 *
 * On top of the zone, a path may be write-guarded: `protected` (a write runs
 * code outside the sandbox later; the judge may approve it) or `self`
 * (Cypher's and Pi's own data; only a human may approve a write).
 *
 * A chat whose project folder Cypher itself made inside its data (a
 * workspace bot's folder, `ownFolders`) works there like in any project: the
 * patterns that cover that whole folder because of where it lives (other
 * chats' data, Cypher's own data) do not apply inside it. A secret in it
 * (`.env`, a key) is still one.
 */

import { homedir } from "node:os";
import { isAbsolute, join, sep } from "node:path";
import { globBase, globToRegExp } from "./glob.ts";
import { canonicalize, escapeGlob, existingMatches, expandHome, floating, globsIntersect, sameInodeAs } from "./resolve.ts";
import type { Zone } from "./config.ts";

// One resolver for every path (resolve.ts): literal paths here, patterns in `classifyPattern`.
export { canonicalize, expandHome } from "./resolve.ts";

export function isUnder(path: string, root: string): boolean {
  if (path === root) return true;
  const prefix = root.endsWith(sep) ? root : `${root}${sep}`;
  return path.startsWith(prefix);
}

interface CompiledPattern {
  source: string;
  regex: RegExp;
  exception: boolean;
  /** The expanded glob (absolute, or `**\/…` anchored anywhere), for `classifyPattern`. */
  glob: string;
}

export type Guard = "protected" | "self";

export interface Classification {
  path: string;
  zone: Zone;
  guard?: Guard;
  /** The pattern that made the path sensitive or guarded, for dialogs and the audit. */
  match?: string;
  /** In the temp directories, which count as workspace. */
  scratch?: boolean;
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
      return { source: pattern, regex: globToRegExp(anchored), exception, glob: anchored };
    }),
  );
}

/** The first non-exception pattern that matches, unless an exception matches too. `skip` leaves patterns out. */
export function matchPatterns(path: string, compiled: readonly CompiledPattern[], skip?: (pattern: CompiledPattern) => boolean): string | undefined {
  let hit: string | undefined;
  for (const pattern of compiled) {
    if (skip?.(pattern)) continue;
    if (!pattern.regex.test(path)) continue;
    if (pattern.exception) return undefined;
    hit ??= pattern.source;
  }
  return hit;
}

/**
 * What a glob (resolve.ts `resolvePattern`) could stand for (round 5): the existing paths it matches now,
 * each classified, and the configured patterns some path it may be could match although none exists now.
 *
 * A pattern rooted in a place (`~/.ssh/**`, `<agent>/cypher-permissions/**`) counts when the glob could
 * be a path inside it; a pattern that names a file anywhere by its name (`**\/.env`, `**\/*.pem`) counts
 * only through an existing match, as bash expands a glob only to files that are there (`rm build/*` is no
 * secret because some file *could* be called `id_rsa.log`). The permission system is read more strictly:
 * any of its patterns the glob could match, and with `subtree` (a deletion, a move, a recursive change)
 * any of its places inside what the glob names.
 */
export interface PatternClassification {
  matches: Classification[];
  /** The sensitive pattern it could match. */
  sensitive?: string;
  /** The permission-system pattern it could match. */
  permissionSystem?: string;
  /** The guard (`self` before `protected`) it could fall under, with the pattern. */
  guard?: Guard;
  guardMatch?: string;
}

export interface PathContextInput {
  roots: readonly string[];
  sensitive: readonly string[];
  protectedPatterns: readonly string[];
  self: readonly string[];
  writableSupport: readonly string[];
  /** The permission system's own files (config.ts): a change there always asks. */
  permissionSystem?: readonly string[];
  /** Temp directories: scratch space, writable like the workspace. */
  scratch?: readonly string[];
  /** Project folders Cypher made for this chat inside its own data (config.ts `ownFolders`), canonical. */
  ownFolders?: readonly string[];
  home?: string;
}

export class PathContext {
  readonly roots: readonly string[];
  readonly home: string;
  readonly sensitive: CompiledPattern[];
  readonly protectedPatterns: CompiledPattern[];
  readonly self: CompiledPattern[];
  readonly writableSupport: CompiledPattern[];
  readonly permissionSystem: CompiledPattern[];
  readonly scratch: readonly string[];
  readonly ownFolders: readonly string[];

  constructor(input: PathContextInput) {
    this.scratch = input.scratch ?? [];
    this.ownFolders = input.ownFolders ?? [];
    this.home = input.home ?? homedir();
    this.roots = input.roots;
    this.sensitive = compilePatterns(input.sensitive, input.roots, this.home);
    this.protectedPatterns = compilePatterns(input.protectedPatterns, input.roots, this.home);
    this.self = compilePatterns(input.self, input.roots, this.home);
    this.writableSupport = compilePatterns(input.writableSupport, input.roots, this.home);
    this.permissionSystem = compilePatterns(input.permissionSystem ?? [], input.roots, this.home);
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

  /** A file of the permission system itself: changing it always asks the user. */
  isPermissionSystem(path: string): string | undefined {
    const matched = matchPatterns(path, this.permissionSystem);
    if (matched !== undefined) return matched;
    // Another name of one of its files (a hard link): the files there now, by the rooted patterns.
    const linked = sameInodeAs(path, this.permissionFiles());
    return linked === undefined ? undefined : `${matchPatterns(linked, this.permissionSystem) ?? "the permission system"} (a hard link of ${linked})`;
  }

  /** The permission system's files that exist now (rooted patterns only; bounded by `existingMatches`). */
  private *permissionFiles(): Generator<string> {
    for (const pattern of this.permissionSystem) {
      if (pattern.exception || floating(pattern.glob)) continue;
      yield* existingMatches(pattern.glob).paths;
    }
  }

  inScratch(path: string): boolean {
    return this.scratch.some((root) => isUnder(path, root));
  }

  /**
   * Whether `glob` could stand for a path `list` covers: it intersects a positive pattern (or, with
   * `subtree`, holds a place a rooted pattern names), unless an exception holds every path it may be.
   */
  private couldMatch(list: readonly CompiledPattern[], glob: string, subtree: boolean, anywhereNames = true): string | undefined {
    const own = this.ownFolders.find((root) => glob.startsWith(`${root}/`) || glob === root);
    for (const pattern of list) {
      if (pattern.exception) continue;
      // A pattern that names a file anywhere by its name counts when the glob's own name says that name
      // (`"$D/.pi/permissions.json"`), never because some path it may be could be called so (`/tmp/x-$$`).
      // Every fixed name of the pattern (`.pi`, `extensions`) must be in the glob's own text (round 5 step 2: `**/.pi/extensions/**`
      // is no part of `repos/x/…/logs`, though `…` could be anything).
      if (floating(pattern.glob) && (!anywhereNames || !fixedNames(pattern.glob).every((name) => glob.split("/").includes(name)) || (!pattern.glob.endsWith("/**") && /[*?[]/.test(glob.slice(glob.lastIndexOf("/") + 1))))) continue;
      if (own !== undefined && pattern.regex.test(own)) continue;
      const hit = globsIntersect(glob, pattern.glob) || (subtree && !floating(pattern.glob) && this.holds(glob, pattern.glob));
      if (!hit) continue;
      // An exception that holds the whole glob (`~/.ssh/*.pub` against `!~/.ssh/*.pub`): its regex reads the glob as text.
      if (list.some((exception) => exception.exception && exception.regex.test(glob))) continue;
      return pattern.source;
    }
    return undefined;
  }

  /**
   * Whether what `glob` names holds a place a rooted pattern covers: the pattern's literal directory lies
   * inside it (`rm -rf ~/.cypher/pi-runtime` holds `<agent>/cypher-permissions`), or a place the pattern
   * matches now does (`~/.cypher*\/*\/pi-runtime/agent/…` under a profile folder that is there).
   */
  private holds(glob: string, pattern: string): boolean {
    const inside = `${glob.replace(/\/+$/, "")}/**`;
    const place = pattern.replace(/\/\*\*$/, "");
    const base = globBase(place);
    if (globsIntersect(inside, escapeGlob(base)) || globsIntersect(glob, escapeGlob(base))) return true;
    if (base === place) return false;
    return existingMatches(place).paths.some((path) => globsIntersect(inside, escapeGlob(path)) || globsIntersect(glob, escapeGlob(path)));
  }

  /** What a glob could be (`PatternClassification`): a literal path is one too, with `subtree` for what lies below it. */
  classifyPattern(glob: string, options: { subtree?: boolean; unknownText?: boolean } = {}): PatternClassification {
    const subtree = options.subtree === true;
    // What it matches now (bash expands a glob to the files that are there); text known only when it runs
    // is no glob: it could be anything, so only the patterns tell.
    const { paths } = options.unknownText ? { paths: [] } : /[*?[\]\\]/.test(glob) ? existingMatches(glob) : { paths: [glob] };
    const matches = paths.map((path) => this.classify(path));
    const out: PatternClassification = { matches };
    const sensitive = matches.find((item) => item.zone === "sensitive")?.match ?? this.couldMatch(this.sensitive, glob, false, false);
    if (sensitive) out.sensitive = sensitive;
    const system = matches.map((item) => this.isPermissionSystem(item.path)).find(Boolean) ?? this.couldMatch(this.permissionSystem, glob, subtree);
    if (system) out.permissionSystem = system;
    const self = matches.find((item) => item.guard === "self")?.match ?? this.couldMatch(this.self, glob, false, false);
    const guarded = self ? undefined : (matches.find((item) => item.guard === "protected")?.match ?? this.couldMatch(this.protectedPatterns, glob, false, false));
    if (self) Object.assign(out, { guard: "self", guardMatch: self });
    else if (guarded) Object.assign(out, { guard: "protected", guardMatch: guarded });
    return out;
  }

  classify(path: string): Classification {
    // In the chat's own folder inside Cypher's data, a pattern that covers the whole folder says nothing.
    const own = this.ownFolders.find((root) => isUnder(path, root));
    const skip = own === undefined ? undefined : (pattern: CompiledPattern) => !pattern.exception && pattern.regex.test(own);
    const sensitive = matchPatterns(path, this.sensitive, skip);
    if (sensitive) return { path, zone: "sensitive", match: sensitive };
    const scratch = !this.inWorkspace(path) && this.inScratch(path);
    const zone: Zone = scratch || this.inWorkspace(path) ? "workspace" : "external";
    const self = matchPatterns(path, this.self, skip);
    if (self) return { path, zone, guard: "self", match: self };
    const guarded = matchPatterns(path, this.protectedPatterns);
    if (guarded) return { path, zone, guard: "protected", match: guarded };
    // The permission system is guarded wherever it is, by any name of its files (a hard link, round 5 step 2).
    const system = this.isPermissionSystem(path);
    if (system) return { path, zone, guard: "protected", match: system };
    return scratch ? { path, zone, scratch } : { path, zone };
  }
}

/** The segments of a glob with no glob character: the names it fixes. */
function fixedNames(glob: string): string[] {
  return glob.split("/").filter((segment) => segment !== "" && !/[*?[{\\]/.test(segment));
}
