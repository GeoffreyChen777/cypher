/**
 * The one path resolver (round 5, design §7.3.1, row 2). Every protected-path,
 * sensitive-path and permission-system check reads a path through here,
 * before it is classified (paths.ts `PathContext.classify` /
 * `classifyPattern`):
 *
 * - a literal path (a file tool's argument, a word the shell reader knows):
 *   `~`, `~user`, `~+`, `$HOME`/`${HOME}` at its start, the directory it is
 *   relative to, `..`, and symlinks (realpath through the nearest existing
 *   ancestor) — `canonicalize`;
 * - a word known only when it runs, as the shell reader writes it down
 *   (shell.ts `patterns`): globs (`*`, `?`, `[…]`, `**`), braces (`{a,b}`,
 *   `{1..3}`), environment variables the line doesn't set (`\u0002NAME\u0003`:
 *   the value the environment has, or nothing, as bash expands an unset one),
 *   and text known only when it runs (`\u0001`: anything, `/` included) —
 *   `resolvePattern` turns it into absolute globs whose fixed directory is
 *   canonical (symlinks followed);
 * - whether such a glob could stand for a path a pattern list covers
 *   (`globsIntersect`: some path matches both), and which existing files it
 *   matches now (`existingMatches`, each one canonical).
 *
 * The checks then ask about every path the word could be. A glob whose fixed
 * directory is an ancestor of a protected one, or a word whose start is
 * known only when it runs, *could* match it: the caller decides what that
 * means (the permission system: the floor asks).
 */

import { lstatSync, readdirSync, readlinkSync, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";

/** Text known only when the line runs: anything, `/` included. */
export const ANY = "\u0001";
/** An environment variable the line doesn't set: `ENV_OPEN NAME ENV_CLOSE`. */
export const ENV_OPEN = "\u0002";
export const ENV_CLOSE = "\u0003";

const CASE_INSENSITIVE = process.platform === "darwin";

/** The home folder of `user` (`~user`), by where this machine keeps homes; root's own place. */
export function userHome(user: string, home = homedir()): string {
  if (user === "root") return process.platform === "darwin" ? "/var/root" : "/root";
  return join(dirname(home), user);
}

/** `~` and `~/x` at the start of a literal path (a file tool's argument, a word the shell reader expanded). */
export function expandHome(path: string, home = homedir()): string {
  if (path === "~") return home;
  if (path.startsWith("~/")) return join(home, path.slice(2));
  return path;
}

/**
 * Text inside a script, an option's value or an unparsed line that names a path from home: `~`, `~/x`,
 * `~user`, `~user/x`, `~+` (the directory it runs in) and `$HOME`/`${HOME}` at its start, as bash expands
 * them. Anything else stays as written.
 */
export function resolveText(text: string, home = homedir(), cwd?: string): string {
  const variable = /^(?:\$HOME|\$\{HOME\})(?=\/|$)/.exec(text);
  if (variable) return home + text.slice(variable[0].length);
  if ((text === "~+" || text.startsWith("~+/")) && cwd !== undefined) return cwd + text.slice(2);
  const user = /^~([A-Za-z_][\w.-]*)(?=\/|$)/.exec(text);
  if (user) return userHome(user[1], home) + text.slice(user[0].length);
  return expandHome(text, home);
}

/** The most symlinks one path is followed through (as the kernel's ELOOP bound). */
const MAX_LINKS = 40;

/**
 * Follow symlinks through the nearest existing ancestor; keep the missing tail as written. A link
 * whose target is missing (dangling: `ln -s ~/.cypher/…/permissions.json x`) is followed by its text
 * (`readlink`), as a write through it would create the target (round 5 step 2): the path is where the
 * link points, not the link.
 */
/**
 * macOS's data volume spells every user path a second way (`/System/Volumes/Data/Users/…`, a firmlink realpath keeps):
 * the same file as `/Users/…` (iteration 2). Folded when the path without the prefix is one of the system's own roots.
 */
const DATA_VOLUME = "/System/Volumes/Data";
function unfirmlinked(path: string): string {
  if (!path.startsWith(`${DATA_VOLUME}/`)) return path;
  const rest = path.slice(DATA_VOLUME.length);
  const root = `/${rest.split("/")[1] ?? ""}`;
  try {
    return root !== "/" && lstatSync(root) ? rest : path;
  } catch {
    return path;
  }
}

export function canonicalize(path: string, cwd: string, home = homedir(), links = 0): string {
  const absolute = unfirmlinked(resolve(cwd, expandHome(path.startsWith("@") ? path.slice(1) : path, home)));
  const tail: string[] = [];
  let current = absolute;
  for (;;) {
    try {
      const real = realpathSync.native(current);
      return tail.length === 0 ? real : join(real, ...tail.reverse());
    } catch {
      if (links < MAX_LINKS) {
        let target: string | undefined;
        try {
          if (lstatSync(current).isSymbolicLink()) target = readlinkSync(current);
        } catch {
          target = undefined;
        }
        if (target !== undefined) {
          const through = resolve(dirname(current), target);
          return canonicalize(tail.length === 0 ? through : join(through, ...[...tail].reverse()), "/", home, links + 1);
        }
      }
      const parent = dirname(current);
      if (parent === current) return absolute;
      tail.push(basename(current));
      current = parent;
    }
  }
}

/**
 * Whether a file has another name among `files` (a hard link: the same device and inode). A write
 * through any name of an inode writes them all, and no path says so (round 5 step 2): a file with more
 * than one link is compared with the given ones (the permission system's files that exist).
 */
export function sameInodeAs(path: string, files: Iterable<string>): string | undefined {
  let stat;
  try {
    stat = lstatSync(path);
  } catch {
    return undefined;
  }
  if (!stat.isFile() || stat.nlink < 2) return undefined;
  for (const file of files) {
    try {
      const other = statSync(file);
      if (other.ino === stat.ino && other.dev === stat.dev) return file;
    } catch {
      // Not there.
    }
  }
  return undefined;
}

// --- Patterns ----------------------------------------------------------------------

/** The most globs one word's text known only when it runs is read as (each such segment doubles them). */
const MAX_ALTERNATIVES = 16;

/** The most words a brace expansion yields; past it, the braces stand for anything. */
const MAX_BRACES = 64;

/** Escape glob metacharacters in literal text. */
export function escapeGlob(text: string): string {
  return text.replace(/[\\*?[\]{},]/g, "\\$&");
}

/** Whether a pattern has a glob or a marker outside escapes. */
export function hasPattern(pattern: string): boolean {
  for (let i = 0; i < pattern.length; i++) {
    const char = pattern[i];
    if (char === "\\") {
      i++;
      continue;
    }
    if ("*?[{".includes(char) || char === ANY || char === ENV_OPEN) return true;
  }
  return false;
}

/** The index of the brace closing the one at `open` (unescaped), or -1. */
function closingBrace(pattern: string, open: number): number {
  let depth = 0;
  for (let i = open; i < pattern.length; i++) {
    const char = pattern[i];
    if (char === "\\") {
      i++;
      continue;
    }
    if (char === "{") depth++;
    else if (char === "}" && --depth === 0) return i;
  }
  return -1;
}

/** The top-level comma-separated parts of a brace body. */
function braceParts(body: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < body.length; i++) {
    const char = body[i];
    if (char === "\\") {
      i++;
      continue;
    }
    if (char === "{") depth++;
    else if (char === "}") depth--;
    else if (char === "," && depth === 0) {
      parts.push(body.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(body.slice(start));
  return parts;
}

/** A `{a..b}` sequence's words (numbers or single letters), or undefined. */
function sequence(body: string): string[] | undefined {
  const numbers = /^(-?\d+)\.\.(-?\d+)(?:\.\.(-?\d+))?$/.exec(body);
  if (numbers) {
    const [from, to] = [Number(numbers[1]), Number(numbers[2])];
    const step = Math.abs(Number(numbers[3] ?? 1)) || 1;
    const out: string[] = [];
    for (let n = from; from <= to ? n <= to : n >= to; n += from <= to ? step : -step) {
      out.push(String(n));
      if (out.length > MAX_BRACES) return undefined;
    }
    return out;
  }
  const letters = /^([A-Za-z])\.\.([A-Za-z])$/.exec(body);
  if (letters) {
    const [from, to] = [letters[1].charCodeAt(0), letters[2].charCodeAt(0)];
    const out: string[] = [];
    for (let c = from; from <= to ? c <= to : c >= to; c += from <= to ? 1 : -1) out.push(String.fromCharCode(c));
    return out.length > MAX_BRACES ? undefined : out;
  }
  return undefined;
}

/** Bash brace expansion of a pattern: every word it stands for (bounded; past the bound, braces become `*`). */
export function expandBraces(pattern: string): string[] {
  for (let i = 0; i < pattern.length; i++) {
    if (pattern[i] === "\\") {
      i++;
      continue;
    }
    if (pattern[i] !== "{") continue;
    const close = closingBrace(pattern, i);
    if (close < 0) return [pattern];
    const body = pattern.slice(i + 1, close);
    const parts = sequence(body) ?? braceParts(body);
    // `{x}` alone is no expansion: the braces are literal.
    if (parts.length < 2) continue;
    const head = pattern.slice(0, i);
    const tail = pattern.slice(close + 1);
    const out: string[] = [];
    for (const part of parts) {
      for (const word of expandBraces(`${head}${part}${tail}`)) {
        out.push(word);
        if (out.length > MAX_BRACES) return [`${head}*${tail}`];
      }
    }
    return out;
  }
  return [pattern];
}

export interface ResolveContext {
  home: string;
  /** The directory the command runs in; undefined when known only when it runs. */
  cwd: string | undefined;
  /** The environment the line runs with (process.env by default). */
  env?: Readonly<Record<string, string | undefined>>;
}

/** A word as absolute path globs. */
export interface ResolvedPattern {
  /** Absolute globs (`**` crosses directories), their fixed directory canonical. */
  globs: string[];
  /** Its start is known only when it runs (or relative to a directory that is): it could be any path. */
  anywhere: boolean;
  /** Some of it is text known only when it runs (not a glob bash expands to files that are there). */
  unknownText: boolean;
}

/** Split a pattern into `/` segments, keeping escapes. */
function segmentsOf(glob: string): string[] {
  const out: string[] = [];
  let current = "";
  for (let i = 0; i < glob.length; i++) {
    const char = glob[i];
    if (char === "\\" && i + 1 < glob.length) {
      current += char + glob[i + 1];
      i++;
      continue;
    }
    if (char === "/") {
      out.push(current);
      current = "";
      continue;
    }
    current += char;
  }
  out.push(current);
  return out;
}

/** Undo `escapeGlob` in a segment with no glob in it. */
function literalOf(segment: string): string {
  return segment.replace(/\\(.)/g, "$1");
}

/**
 * A pattern from the shell reader (shell.ts `patterns`) as absolute globs: braces expanded, environment
 * variables substituted, `~` and the directory it runs in applied, `..` folded, text known only when it runs
 * as `**`, and the fixed directory canonical (symlinks followed).
 */
export function resolvePattern(pattern: string, context: ResolveContext): ResolvedPattern {
  const env = context.env ?? process.env;
  const globs = new Set<string>();
  let anywhere = false;
  const unknownText = pattern.includes(ANY);
  for (const word of expandBraces(pattern)) {
    // An environment variable the line doesn't set: its value, or nothing.
    // `NAME:-default`: the default when the environment has no value for it.
    let text = word.replace(new RegExp(`${ENV_OPEN}([A-Za-z_][A-Za-z0-9_]*)(?::-([^${ENV_CLOSE}]*))?${ENV_CLOSE}`, "g"), (_, name: string, fallback: string | undefined) => {
      if (name === "HOME") return escapeGlob(context.home);
      const value = env[name];
      return value !== undefined && value !== "" ? escapeGlob(value) : (fallback ?? "");
    });
    // `~` at the start (when the reader left it as written).
    if (text === "~" || text.startsWith("~/")) text = escapeGlob(context.home) + text.slice(1);
    const user = /^~([A-Za-z_][\w.-]*)(?=\/|$)/.exec(text);
    if (user) text = escapeGlob(userHome(user[1], context.home)) + text.slice(user[0].length);
    // A word that expands to nothing names no path (`rm -rf "$UNSET"`).
    if (text === "") continue;
    if (text.startsWith(ANY)) {
      anywhere = true;
      text = `/${text}`;
    } else if (!text.startsWith("/")) {
      // Relative to a directory known only when it runs: anywhere.
      if (context.cwd === undefined) {
        anywhere = true;
        text = `/${ANY}/${text}`;
      } else text = `${escapeGlob(context.cwd)}/${text}`;
    }
    // Segments: `.` and `..` fold; text known only when it runs is, within its segment, `*` — or, as it may
    // hold `/`, it spans directories (`pre$X.log`: `pre*.log`, or `pre*` … `**` … `*.log`).
    let outs: string[][] = [[]];
    for (const segment of segmentsOf(text)) {
      if (segment === "" || segment === ".") continue;
      if (segment === "..") {
        for (const out of outs) if (out.length && out.at(-1) !== "**") out.pop();
        continue;
      }
      if (!segment.includes(ANY)) {
        for (const out of outs) out.push(segment);
        continue;
      }
      const parts = segment.split(ANY);
      const within = [parts.join("*")];
      const across = [...(parts[0] ? [`${parts[0]}*`] : []), "**", ...(parts.at(-1) ? [`*${parts.at(-1)}`] : [])];
      const next: string[][] = [];
      for (const out of outs) {
        const spanning = out.at(-1) === "**" && across[0] === "**" ? [...out, ...across.slice(1)] : [...out, ...across];
        if (outs.length * 2 <= MAX_ALTERNATIVES) next.push([...out, ...within]);
        next.push(spanning);
      }
      outs = next;
    }
    for (const out of outs) {
      // The fixed directory: canonical, as every literal path is.
      let fixed = 0;
      while (fixed < out.length && !hasPattern(out[fixed])) fixed++;
      const literal = `/${out.slice(0, fixed).map(literalOf).join("/")}`;
      const real = canonicalize(literal, "/", context.home);
      const rest = out.slice(fixed);
      globs.add([escapeGlob(real === "/" ? "" : real), ...rest].join("/") || "/");
    }
  }
  return { globs: [...globs], anywhere, unknownText };
}

// --- Intersection ----------------------------------------------------------------------

type Atom = { lit: string } | { star: true } | { one: true } | { set: string; negated: boolean };

function atomsOf(segment: string): Atom[] {
  const atoms: Atom[] = [];
  for (let i = 0; i < segment.length; i++) {
    const char = segment[i];
    if (char === "\\" && i + 1 < segment.length) {
      atoms.push({ lit: segment[++i] });
      continue;
    }
    if (char === "*") {
      if (!(atoms.at(-1) && "star" in atoms.at(-1)!)) atoms.push({ star: true });
      continue;
    }
    if (char === "?") {
      atoms.push({ one: true });
      continue;
    }
    if (char === "[") {
      const end = segment.indexOf("]", i + 2);
      if (end > i) {
        let body = segment.slice(i + 1, end);
        const negated = body.startsWith("!") || body.startsWith("^");
        if (negated) body = body.slice(1);
        atoms.push({ set: expandSet(body), negated });
        i = end;
        continue;
      }
    }
    atoms.push({ lit: char });
  }
  return atoms;
}

/** A character class body (`a-z0-9_`) as its characters. */
function expandSet(body: string): string {
  let out = "";
  for (let i = 0; i < body.length; i++) {
    if (body[i + 1] === "-" && i + 2 < body.length) {
      const [from, to] = [body.charCodeAt(i), body.charCodeAt(i + 2)];
      for (let c = Math.min(from, to); c <= Math.max(from, to) && out.length < 512; c++) out += String.fromCharCode(c);
      i += 2;
    } else out += body[i];
  }
  return out;
}

const fold = (char: string) => (CASE_INSENSITIVE ? char.toLowerCase() : char);

function charIn(char: string, atom: Atom): boolean {
  if ("lit" in atom) return fold(atom.lit) === fold(char);
  if ("one" in atom || "star" in atom) return true;
  const inSet = [...atom.set].some((c) => fold(c) === fold(char));
  return atom.negated ? !inSet : inSet;
}

/** Whether two single-character atoms can match one character. */
function compatible(a: Atom, b: Atom): boolean {
  if ("lit" in a) return charIn(a.lit, b);
  if ("lit" in b) return charIn(b.lit, a);
  if ("one" in a || "one" in b) return true;
  const x = a as { set: string; negated: boolean };
  const y = b as { set: string; negated: boolean };
  if (x.negated && y.negated) return true;
  if (!x.negated && !y.negated) return [...x.set].some((c) => charIn(c, y));
  const positive = x.negated ? y : x;
  const negative = x.negated ? x : y;
  return [...positive.set].some((c) => charIn(c, negative));
}

/** Whether some single path segment matches both segment globs. */
export function segmentsIntersect(a: string, b: string): boolean {
  const x = atomsOf(a);
  const y = atomsOf(b);
  const memo = new Map<number, boolean>();
  const go = (i: number, j: number): boolean => {
    const key = i * 4096 + j;
    const known = memo.get(key);
    if (known !== undefined) return known;
    let result: boolean;
    if (i === x.length && j === y.length) result = true;
    else if (i < x.length && "star" in x[i]) result = go(i + 1, j) || (j < y.length && go(i, j + 1)) || (j < y.length && "star" in y[j] && go(i + 1, j + 1));
    else if (j < y.length && "star" in y[j]) result = go(i, j + 1) || (i < x.length && go(i + 1, j));
    else if (i === x.length || j === y.length) result = false;
    else result = compatible(x[i], y[j]) && go(i + 1, j + 1);
    memo.set(key, result);
    return result;
  };
  return go(0, 0);
}

/**
 * Whether some path matches both globs (`*`, `?`, `[…]` within a segment, `**` across segments, a
 * trailing `/**` the directory itself too). Both absolute, or both anchored at any directory (`**` first).
 */
export function globsIntersect(a: string, b: string): boolean {
  const x = segmentsOf(a.replace(/^\/+/, "")).filter((segment, index, all) => segment !== "" || index === all.length - 1);
  const y = segmentsOf(b.replace(/^\/+/, "")).filter((segment, index, all) => segment !== "" || index === all.length - 1);
  const memo = new Map<number, boolean>();
  const go = (i: number, j: number): boolean => {
    const key = i * 4096 + j;
    const known = memo.get(key);
    if (known !== undefined) return known;
    let result: boolean;
    if (i === x.length && j === y.length) result = true;
    else if (i < x.length && x[i] === "**") result = go(i + 1, j) || (j < y.length && go(i, j + 1));
    else if (j < y.length && y[j] === "**") result = go(i, j + 1) || (i < x.length && go(i + 1, j));
    else if (i === x.length || j === y.length) result = false;
    else result = segmentsIntersect(x[i], y[j]) && go(i + 1, j + 1);
    memo.set(key, result);
    return result;
  };
  return go(0, 0);
}

/** A glob as a regex over a whole absolute path (for existing files). */
function segmentRegex(segment: string): RegExp {
  let source = "^";
  for (const atom of atomsOf(segment)) {
    if ("lit" in atom) source += atom.lit.replace(/[\\^$.*+?()[\]{}|]/g, "\\$&");
    else if ("star" in atom) source += ".*";
    else if ("one" in atom) source += ".";
    else source += `[${atom.negated ? "^" : ""}${atom.set.replace(/[\\\]^-]/g, "\\$&")}]`;
  }
  return new RegExp(`${source}$`, CASE_INSENSITIVE ? "is" : "s");
}

/** How many directory entries one search may look at, and how deep `**` goes. */
const MAX_VISITED = 4000;
const MAX_DEPTH = 8;

/**
 * The existing paths a glob matches now (bash's own expansion, dotfiles included), each canonical
 * (symlinks followed). `complete` is false when the search stopped at its bound.
 */
export function existingMatches(glob: string): { paths: string[]; complete: boolean } {
  const segments = segmentsOf(glob.replace(/^\/+/, ""));
  const found = new Set<string>();
  let visited = 0;
  let complete = true;
  const list = (dir: string): string[] => {
    if (visited > MAX_VISITED) {
      complete = false;
      return [];
    }
    try {
      const names = readdirSync(dir);
      visited += names.length;
      return names;
    } catch {
      return [];
    }
  };
  const isDir = (path: string) => {
    try {
      return lstatSync(path).isDirectory() || lstatSync(realpathSync.native(path)).isDirectory();
    } catch {
      return false;
    }
  };
  const add = (path: string) => {
    try {
      found.add(realpathSync.native(path));
    } catch {
      // Gone, or a dangling link: nothing there to act on.
    }
  };
  const walk = (dir: string, at: number, depth: number) => {
    if (at === segments.length) {
      add(dir);
      return;
    }
    const segment = segments[at];
    if (segment === "") return walk(dir, at + 1, depth);
    if (segment === "**") {
      walk(dir, at + 1, depth);
      if (depth >= MAX_DEPTH) {
        complete = false;
        return;
      }
      for (const name of list(dir)) {
        const path = join(dir, name);
        if (isDir(path)) walk(path, at, depth + 1);
      }
      return;
    }
    if (!hasPattern(segment)) {
      const path = join(dir, literalOf(segment));
      try {
        lstatSync(path);
      } catch {
        return;
      }
      walk(path, at + 1, depth);
      return;
    }
    const regex = segmentRegex(segment);
    for (const name of list(dir)) if (regex.test(name)) walk(join(dir, name), at + 1, depth);
  };
  walk("/", 0, 0);
  return { paths: [...found], complete };
}

/** Whether a glob is anchored at any directory (a configured `**\/x` pattern), not at the root. */
export function floating(glob: string): boolean {
  return glob.startsWith("/**/") || glob.startsWith("**/");
}

/** Whether `path` is a literal absolute path (no glob, no marker). */
export function literalPath(path: string): boolean {
  return isAbsolute(path) && !hasPattern(path);
}
