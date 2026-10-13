/**
 * Landstrip: OS-level sandboxing for AI bash commands and the ripgrep behind
 * `grep` (Seatbelt on macOS, Landlock + seccomp on Linux).
 *
 * Every launch gets a policy compiled from the level, the zones and the
 * grants that apply to that one command. Reads stay open (`allowRead: ["/"]`)
 * and sensitive files are denied by exact path: a `denyRead` without the `/`
 * allow makes Landstrip enumerate every sibling at launch, so a file the
 * command creates afterwards would be unreadable to it.
 *
 * A denied path inside a writable root also gets each of its ancestors below
 * that root denied as a single entry (a one-character class turns the path
 * into an anchored pattern that matches the directory entry, not its
 * contents). Without that, `mv sub sub2 && cat sub2/.env` or renaming `.git`
 * would move a protected file out from under its rule.
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { globToRegExp } from "./glob.ts";
import { grantCovers } from "./grants.ts";
import type { Grant } from "./grants.ts";
import type { LevelName, PermissionsConfig } from "./config.ts";
import { expandHome, isUnder } from "./paths.ts";

export interface LandstripStatus {
  available: boolean;
  binary?: string;
  implementation?: string;
  error?: string;
}

let cachedStatus: LandstripStatus | undefined;

function landstripModule(): { binaryPath(): string } {
  return requireLandstrip("@landstrip/landstrip") as { binaryPath(): string };
}

export interface FilterProxy {
  port: number;
  stop(): Promise<void>;
}

/** Landstrip's domain-filtering HTTP(S) proxy (refuses private and loopback destinations). */
export function startFilterProxy(options: { isDomainAllowed(domain: string): boolean | Promise<boolean>; authorization: string }): Promise<FilterProxy> {
  const proxy = requireLandstrip("@landstrip/landstrip/proxy") as {
    startFilterProxy(options: { isDomainAllowed(domain: string): boolean | Promise<boolean>; authorization?: string }): Promise<FilterProxy>;
  };
  return proxy.startFilterProxy(options);
}

function requireLandstrip(specifier: string): unknown {
  const bases: string[] = [];
  const packageDir = process.env.PI_PACKAGE_DIR;
  if (packageDir) {
    try {
      // The Runtime's `pi` is a symlink into npm/node_modules; resolve from its real location.
      bases.push(join(realpathSync(packageDir), "package.json"));
    } catch {
      // Fall through to the extension's own location.
    }
  }
  bases.push(import.meta.url);
  let lastError: unknown;
  for (const base of bases) {
    try {
      return createRequire(base)(specifier);
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError instanceof Error ? lastError : new Error("@landstrip/landstrip is not installed");
}

/** Whether this host can sandbox at all; checked once per process. */
export function landstripStatus(): LandstripStatus {
  if (cachedStatus) return cachedStatus;
  if (process.env.CYPHER_PERMISSIONS_LANDSTRIP) {
    // Tests point at a specific binary.
    return (cachedStatus = doctor(process.env.CYPHER_PERMISSIONS_LANDSTRIP));
  }
  let binary: string;
  try {
    binary = landstripModule().binaryPath();
  } catch (error) {
    return (cachedStatus = { available: false, error: error instanceof Error ? error.message : String(error) });
  }
  return (cachedStatus = doctor(binary));
}

function doctor(binary: string): LandstripStatus {
  if (!existsSync(binary)) return { available: false, error: `Landstrip binary not found at ${binary}` };
  const result = spawnSync(binary, ["doctor"], { encoding: "utf8", timeout: 10_000 });
  const output = `${result.stdout ?? ""}`.trim();
  try {
    const parsed = JSON.parse(output.split("\n").pop() ?? "{}") as { ok?: boolean; implementation?: string; message?: string };
    if (result.status === 0 && parsed.ok) return { available: true, binary, implementation: parsed.implementation };
    return { available: false, binary, error: parsed.message ?? (`${result.stderr ?? ""}`.trim() || "landstrip doctor failed") };
  } catch {
    return { available: false, binary, error: `${result.stderr ?? ""}`.trim() || result.error?.message || "landstrip doctor failed" };
  }
}

/** For tests. */
export function resetLandstripStatus(): void {
  cachedStatus = undefined;
}

export interface LandstripPolicy {
  filesystem: {
    allowRead: string[];
    denyRead: string[];
    allowWrite: string[];
    denyWrite: string[];
  };
  network: {
    allowNetwork: boolean;
    allowLocalBinding: boolean;
    allowAllUnixSockets: boolean;
    allowUnixSockets: string[];
    /** The filter proxy's local port (filtered network mode). */
    httpProxyPort?: number;
  };
}

export interface ConcretePaths {
  /** Existing sensitive files and directories: denied for reads. */
  sensitiveRead: string[];
  /** Sensitive paths for write rules, including ones that do not exist yet. */
  sensitiveWrite: string[];
  protectedPaths: string[];
  /** Roots of Cypher's own data; `splitSelf` carves the exception patterns out. */
  selfRoots: string[];
  selfExceptions: string[];
}

export interface CompileInput {
  config: PermissionsConfig;
  level: LevelName;
  roots: readonly string[];
  supportDirs: readonly string[];
  tempDirs: readonly string[];
  paths: ConcretePaths;
  /** Path grants that apply to this launch (see GrantStore.forSandbox): never sensitive. */
  grants: readonly Grant[];
  /** ripgrep for `grep`: reads only, no network. */
  readOnly?: boolean;
  /** Filtered network mode: the command reaches the network only through this proxy port. */
  proxyPort?: number;
  home?: string;
}

export interface CompiledPolicy {
  policy: LandstripPolicy;
}

/**
 * Devices every command may write. macOS needs `/dev/stdout` and friends
 * listed; Linux reaches them through the process's own descriptors, and
 * Landlock rejects a rule on what they resolve to (a pipe).
 */
const DEVICES =
  process.platform === "darwin"
    ? ["/dev/null", "/dev/tty", "/dev/zero", "/dev/random", "/dev/urandom", "/dev/stdout", "/dev/stderr", "/dev/fd", "/dev/dtracehelper"]
    : ["/dev/null", "/dev/tty", "/dev/zero", "/dev/random", "/dev/urandom", "/dev/full"];

/** An anchored single-entry pattern for `path`: matches the entry, not what is beneath it. */
export function entryPattern(path: string): string {
  const escaped = [...path].map((char) => (char === "*" || char === "?" || char === "[" ? `[${char}]` : char === "]" ? "?" : char));
  // Find the last plain character and make it a one-character class.
  for (let index = escaped.length - 1; index >= 0; index--) {
    const char = escaped[index];
    if (char.length === 1 && char !== "/" && char !== "?" && char !== "\\") {
      escaped[index] = `[${char}]`;
      return escaped.join("");
    }
    if (char.length > 1 || char === "?") return escaped.join("");
  }
  return escaped.join("");
}

/** Ancestors of `path` strictly between `root` and `path`, plus `path` itself. */
function entriesBelow(path: string, root: string): string[] {
  const out: string[] = [];
  let current = path;
  while (current !== root && isUnder(current, root)) {
    out.push(current);
    const parent = dirname(current);
    if (parent === current) break;
    current = parent;
  }
  return out;
}

/** Whether `path`'s components match the leading components of `pattern`. */
function prefixMatches(path: string, pattern: string): boolean {
  const have = path.split("/").filter(Boolean);
  const want = pattern.split("/").filter(Boolean);
  for (let index = 0; index < have.length; index++) {
    const part = want[index];
    if (part === undefined) return false;
    if (part === "**") return true;
    if (!globToRegExp(part).test(have[index])) return false;
  }
  return true;
}

/**
 * Deny roots for Cypher's own data inside a writable root, without covering
 * the excepted subtrees (`worktrees/`): a self root with an exception beneath
 * it is split into its children, down to three levels.
 */
export function splitSelf(selfRoot: string, exceptions: readonly string[], depth = 0): string[] {
  if (exceptions.some((exception) => globToRegExp(exception).test(selfRoot))) return [];
  const holdsException = exceptions.some(
    (exception) => prefixMatches(selfRoot, exception) && exception.split("/").filter(Boolean).length > selfRoot.split("/").filter(Boolean).length,
  );
  if (!holdsException || depth >= 3) return [selfRoot];
  let names: string[];
  try {
    names = readdirSync(selfRoot);
  } catch {
    return [selfRoot];
  }
  return names.flatMap((name) => {
    const child = join(selfRoot, name);
    let isDir = false;
    try {
      isDir = statSync(child).isDirectory();
    } catch {
      return [child];
    }
    if (exceptions.some((exception) => globToRegExp(exception).test(child))) return [];
    return isDir ? splitSelf(child, exceptions, depth + 1) : [child];
  });
}

function unique(paths: Iterable<string>): string[] {
  return [...new Set(paths)].sort();
}

export function compilePolicy(input: CompileInput): CompiledPolicy {
  const { config, level, grants, paths } = input;
  const home = input.home ?? homedir();
  const matrix = config.levels[level];

  // Grants on domains, tools and commands are not filesystem rules.
  const fileGrants = grants
    .map((grant) => ({ ...grant, paths: grant.paths.filter((path) => path.startsWith("/")) }))
    .filter((grant) => grant.paths.length > 0);
  const readCovered = (path: string) => fileGrants.some((grant) => grantCovers(grant, "read", path));
  const writeCovered = (path: string) => fileGrants.some((grant) => grantCovers(grant, "write", path));

  const allowWrite = new Set<string>(input.readOnly ? ["/dev/null"] : [...DEVICES, ...input.tempDirs]);
  if (!input.readOnly) {
    if (matrix.workspace.write === "allow") {
      for (const root of input.roots) allowWrite.add(root);
      for (const dir of input.supportDirs) allowWrite.add(dir);
    }
    if (matrix.external.write === "allow") allowWrite.add("/");
    for (const grant of fileGrants) {
      if (grant.capability === "write") for (const path of grant.paths) allowWrite.add(path);
    }
  }
  const writeRoots = [...allowWrite].filter((path) => !path.startsWith("/dev/"));
  const insideWritable = (path: string) => writeRoots.some((root) => isUnder(path, root));

  const denyWrite = new Set<string>();
  const denyRead = new Set<string>();
  const anchors = new Set<string>();
  const guardEntries = (path: string) => {
    for (const root of writeRoots) {
      if (!isUnder(path, root) || path === root) continue;
      for (const entry of entriesBelow(dirname(path), root)) anchors.add(entryPattern(entry));
    }
  };

  if (matrix.sensitive.read !== "allow") {
    for (const path of paths.sensitiveRead) {
      if (readCovered(path)) continue;
      denyRead.add(path);
      if (insideWritable(path)) {
        denyWrite.add(path);
        guardEntries(path);
      }
    }
  }
  if (!input.readOnly) {
    const guarded: string[] = [];
    if (matrix.sensitive.write !== "allow") guarded.push(...paths.sensitiveWrite);
    if (level !== "full-access") {
      guarded.push(...paths.protectedPaths);
      for (const selfRoot of paths.selfRoots) {
        for (const root of writeRoots) {
          if (isUnder(selfRoot, root)) guarded.push(...splitSelf(selfRoot, paths.selfExceptions));
        }
      }
    }
    for (const path of guarded) {
      if (writeCovered(path) || !insideWritable(path)) continue;
      denyWrite.add(path);
      guardEntries(path);
    }
  }

  let allowRead = ["/"];
  if (matrix.external.read !== "allow") {
    // Only the workspace, the toolchain and the system stay readable.
    allowRead = unique([...input.roots, ...input.supportDirs, ...input.tempDirs, ...fileGrants.flatMap((grant) => grant.paths)]);
    denyRead.add(dirname(home));
  }

  // No secret is readable here (a command that needs one runs outside the
  // sandbox), so the network is open, or only through the filter proxy.
  const filtered = config.network.mode === "filtered";
  const allowNetwork = !input.readOnly && !filtered;
  const viaProxy = !input.readOnly && filtered && input.proxyPort !== undefined;

  return {
    policy: {
      filesystem: {
        allowRead,
        denyRead: unique(denyRead),
        allowWrite: unique(allowWrite),
        denyWrite: unique([...denyWrite, ...anchors]),
      },
      network: {
        allowNetwork,
        allowLocalBinding: !input.readOnly && config.network.localBinding,
        allowAllUnixSockets: false,
        allowUnixSockets: config.network.unixSockets.map((socket) => expandHome(socket, home)).filter(isSocket),
        ...(viaProxy ? { httpProxyPort: input.proxyPort } : {}),
      },
    },
  };
}

function isSocket(path: string): boolean {
  try {
    return statSync(path).isSocket();
  } catch {
    return false;
  }
}

/** Remove secret-looking variables unless a grant lets one through. */
export function scrubEnv(env: NodeJS.ProcessEnv, patterns: readonly string[], allowed: readonly string[] = []): NodeJS.ProcessEnv {
  const matchers = patterns.map((pattern) => globToRegExp(pattern, false));
  const keep = new Set(allowed);
  const out: NodeJS.ProcessEnv = {};
  for (const [name, value] of Object.entries(env)) {
    if (!keep.has(name) && matchers.some((matcher) => matcher.test(name))) continue;
    out[name] = value;
  }
  return out;
}

export function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

export interface PolicyFile {
  path: string;
  dispose(): void;
}

export function writePolicyFile(policy: LandstripPolicy): PolicyFile {
  const dir = mkdtempSync(join(tmpdir(), "cypher-permissions-"));
  const path = join(dir, "policy.json");
  writeFileSync(path, JSON.stringify(policy), { mode: 0o600 });
  return {
    path,
    dispose() {
      rmSync(dir, { recursive: true, force: true });
    },
  };
}

/** The shell line Pi runs instead of the agent's command. */
export function wrapCommand(binary: string, policyPath: string, shell: string, command: string): string {
  return `exec ${shellQuote(binary)} run -p ${shellQuote(policyPath)} -- ${shellQuote(shell)} -c ${shellQuote(command)}`;
}

/** Temp directories every command may write. */
export function tempDirs(): string[] {
  const out = new Set<string>();
  for (const candidate of [tmpdir(), process.env.TMPDIR, "/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp"]) {
    if (!candidate) continue;
    try {
      out.add(realpathSync(candidate));
    } catch {
      // Missing on this platform.
    }
  }
  return [...out];
}

/** Landstrip's denial events (Linux) as `operation path` pairs, for the agent's hint. */
export interface DenialEvent {
  kind: "filesystem" | "network";
  operation: string;
  target: string;
}

export function parseDenials(output: string): { denials: DenialEvent[]; rest: string } {
  const denials: DenialEvent[] = [];
  const kept: string[] = [];
  for (const line of output.split("\n")) {
    const trimmed = line.trim();
    if (trimmed.startsWith("{\"kind\":")) {
      try {
        const event = JSON.parse(trimmed) as { kind?: string; operation?: string; path?: string; target?: string; code?: string };
        if (event.kind === "filesystem" && event.path) {
          denials.push({ kind: "filesystem", operation: event.operation ?? "access", target: event.path });
          continue;
        }
        if (event.kind === "network" && event.target) {
          denials.push({ kind: "network", operation: event.operation ?? "connect", target: event.target });
          continue;
        }
      } catch {
        // Not an event; keep the line.
      }
    }
    kept.push(line);
  }
  return { denials, rest: kept.join("\n") };
}

/**
 * Output that looks like the sandbox's doing even when it reported nothing:
 * a refused syscall, or a credential it hides failing like a wrong one.
 */
export const DENIAL_TEXT =
  /Operation not permitted|Permission denied|EPERM|EACCES|Could not resolve host|Couldn't resolve host|getaddrinfo|ENOTFOUND|Network is unreachable|Host key verification failed|Authentication failed|could not read Username|terminal prompts disabled|Unable to locate credentials|not logged in|SecKeychain|keychain/i;
