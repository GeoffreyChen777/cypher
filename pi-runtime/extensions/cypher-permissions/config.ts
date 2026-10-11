/**
 * Configuration: built-in defaults, the user's `permissions.json` in the Pi
 * agent directory, and a project's `.pi/permissions.json`, which may only make
 * things stricter (the agent can write the project, and the project itself may
 * be hostile).
 */

import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export type Decision = "allow" | "elevate" | "deny";
export type ApprovalMode = "deny" | "ask" | "assisted";
export type Zone = "workspace" | "external" | "sensitive";
export type Capability = "read" | "write";
export type LevelName = "read-only" | "standard" | "full-access";

export interface CellDecisions {
  read: Decision;
  write: Decision;
}

export type LevelMatrix = Record<Zone, CellDecisions>;

/** A persistent ("always") grant, written by a human approval. */
export interface Rule {
  capability: "read" | "write";
  path: string;
  createdAt: string;
}

export interface PermissionsConfig {
  version: 1;
  defaultLevel: LevelName;
  levels: Record<LevelName, LevelMatrix>;
  /** Keyed `<zone>.<capability>`, `protected.write`, `network`, `tools`. */
  approval: Record<string, ApprovalMode>;
  judge: { model: string | null; timeoutMs: number; minConfidence: number };
  ask: { timeoutMs: number | null };
  sensitive: string[];
  protected: string[];
  self: string[];
  writableSupport: string[];
  envScrub: string[];
  network: {
    localBinding: boolean;
    unixSockets: string[];
    /**
     * `open`: commands reach the network directly. `filtered`: only through a
     * proxy that lets the allowed domains through and asks for the rest.
     */
    mode: "open" | "filtered";
    allowDomains: string[];
  };
  tools: { mcp: Decision; overrides: Record<string, Decision> };
  rules: Rule[];
}

export const CONFIG_FILE = "permissions.json";
export const PROJECT_CONFIG_FILE = join(".pi", "permissions.json");

/**
 * Pattern syntax: `~/` is the home directory, `<root>/` each workspace root,
 * a leading `**` plus `/` matches anywhere, a leading `!` makes an exception.
 */
export const DEFAULT_SENSITIVE: readonly string[] = [
  // Credentials in the home directory.
  "~/.ssh/**",
  "~/.gnupg/**",
  "~/.aws/**",
  "~/.azure/**",
  "~/.config/gcloud/**",
  "~/.kube/**",
  "~/.docker/config.json",
  "~/.netrc",
  "~/.npmrc",
  "~/.pypirc",
  "~/.config/gh/**",
  "~/.git-credentials",
  "~/.config/git/credentials",
  "~/Library/Keychains/**",
  "~/Library/Cookies/**",
  "~/Library/Application Support/Google/Chrome/**",
  "~/Library/Application Support/BraveSoftware/**",
  "~/Library/Application Support/Microsoft Edge/**",
  "~/Library/Application Support/Arc/**",
  "~/Library/Application Support/Firefox/**",
  "~/Library/Safari/**",
  "~/.mozilla/**",
  "~/.config/google-chrome/**",
  "~/.config/chromium/**",
  // Secrets in any project.
  "**/.env",
  "**/.env.*",
  "!**/.env.example",
  "!**/.env.sample",
  "!**/.env.template",
  "**/*.pem",
  "**/*.key",
  "**/id_rsa*",
  "**/id_ed25519*",
  "**/id_ecdsa*",
  "**/*.p12",
  "**/*.pfx",
  "**/*.keystore",
  "**/*.jks",
  // Cypher's and Pi's own credentials and other chats' transcripts.
  "~/.cypher*/session.json",
  "~/.cypher*/*/session.json",
  "~/.cypher*/profiles/**",
  "~/.cypher*/*/profiles/**",
  "~/.cypher*/orgs/**",
  "~/.cypher*/*/orgs/**",
  "~/.cypher*/pi-runtime/agent/auth.json",
  "~/.cypher*/*/pi-runtime/agent/auth.json",
  "~/.cypher*/pi-runtime/agent/mcp-auth.json",
  "~/.cypher*/*/pi-runtime/agent/mcp-auth.json",
  "~/.cypher*/pi-runtime/agent/sessions/**",
  "~/.cypher*/*/pi-runtime/agent/sessions/**",
  "~/.pi/agent/auth.json",
  "~/.pi/agent/sessions/**",
];

/** Readable, but a write runs code outside the sandbox later. */
export const DEFAULT_PROTECTED: readonly string[] = [
  "**/.git/config",
  "**/.git/config.worktree",
  "**/.git/hooks/**",
  "<root>/.gitmodules",
  "<root>/.envrc",
  "<root>/.vscode/tasks.json",
  "<root>/.husky/**",
  "~/.gitconfig",
  "~/.config/git/**",
  "~/.zshrc",
  "~/.zprofile",
  "~/.zshenv",
  "~/.zlogin",
  "~/.bashrc",
  "~/.bash_profile",
  "~/.bash_login",
  "~/.profile",
  "~/.config/fish/**",
  "~/Library/LaunchAgents/**",
  "~/.config/systemd/user/**",
  "~/.config/autostart/**",
];

/**
 * Cypher's and Pi's own data: policy, settings, extensions and the Runtime.
 * Readable (the shell runs the Runtime's binaries), but only a human may
 * approve a write: the judge must not approve edits to its own policy.
 */
export const DEFAULT_SELF: readonly string[] = [
  "~/.cypher*/**",
  "!~/.cypher*/worktrees/**",
  "!~/.cypher*/*/worktrees/**",
  "~/.pi/**",
];

export const DEFAULT_WRITABLE_SUPPORT: readonly string[] = [
  "~/.npm/**",
  "~/.cache/**",
  "~/Library/Caches/**",
  "~/.cargo/registry/**",
  "~/.cargo/git/**",
  "~/go/pkg/mod/**",
  "~/.gradle/caches/**",
  "~/.pnpm-store/**",
  "~/.bun/install/cache/**",
  "~/.yarn/berry/cache/**",
];

/** Domains a filtered network allows without asking (`*.x` covers subdomains). */
export const DEFAULT_ALLOW_DOMAINS: readonly string[] = [
  "github.com",
  "*.github.com",
  "*.githubusercontent.com",
  "registry.npmjs.org",
  "*.npmjs.org",
  "registry.yarnpkg.com",
  "pypi.org",
  "files.pythonhosted.org",
  "crates.io",
  "*.crates.io",
  "static.rust-lang.org",
  "proxy.golang.org",
  "sum.golang.org",
  "repo.maven.apache.org",
  "repo1.maven.org",
  "rubygems.org",
  "*.rubygems.org",
  "registry-1.docker.io",
];

export const DEFAULT_ENV_SCRUB: readonly string[] = [
  "*_TOKEN",
  "*_SECRET",
  "*_SECRET_KEY",
  "*_API_KEY",
  "*_APIKEY",
  "*_PASSWORD",
  "*_ACCESS_KEY",
  "*_PRIVATE_KEY",
  "AWS_*",
  "GH_TOKEN",
  "GITHUB_TOKEN",
  "SSH_AUTH_SOCK",
  "CYPHER_ENGINE_SOCKET",
];

const STANDARD: LevelMatrix = {
  workspace: { read: "allow", write: "allow" },
  external: { read: "allow", write: "elevate" },
  sensitive: { read: "elevate", write: "elevate" },
};

export const DEFAULT_LEVELS: Record<LevelName, LevelMatrix> = {
  "read-only": {
    workspace: { read: "allow", write: "elevate" },
    external: { read: "allow", write: "elevate" },
    sensitive: { read: "elevate", write: "elevate" },
  },
  standard: STANDARD,
  "full-access": {
    workspace: { read: "allow", write: "allow" },
    external: { read: "allow", write: "allow" },
    sensitive: { read: "allow", write: "allow" },
  },
};

export const DEFAULT_APPROVAL: Record<string, ApprovalMode> = {
  "external.write": "assisted",
  "protected.write": "assisted",
  "sensitive.read": "ask",
  "sensitive.write": "ask",
  network: "assisted",
  tools: "ask",
};

export function defaultConfig(): PermissionsConfig {
  return {
    version: 1,
    defaultLevel: "standard",
    levels: structuredClone(DEFAULT_LEVELS),
    approval: { ...DEFAULT_APPROVAL },
    judge: { model: null, timeoutMs: 20_000, minConfidence: 0.8 },
    ask: { timeoutMs: null },
    sensitive: [...DEFAULT_SENSITIVE],
    protected: [...DEFAULT_PROTECTED],
    self: [...DEFAULT_SELF],
    writableSupport: [...DEFAULT_WRITABLE_SUPPORT],
    envScrub: [...DEFAULT_ENV_SCRUB],
    network: { localBinding: true, unixSockets: [], mode: "open", allowDomains: [...DEFAULT_ALLOW_DOMAINS] },
    tools: { mcp: "allow", overrides: {} },
    rules: [],
  };
}

const DECISIONS: readonly Decision[] = ["allow", "elevate", "deny"];
const MODES: readonly ApprovalMode[] = ["deny", "ask", "assisted"];
const LEVELS: readonly LevelName[] = ["read-only", "standard", "full-access"];
const ZONES: readonly Zone[] = ["workspace", "external", "sensitive"];

/** Higher is stricter. */
const DECISION_RANK: Record<Decision, number> = { allow: 0, elevate: 1, deny: 2 };
const MODE_RANK: Record<ApprovalMode, number> = { assisted: 0, ask: 1, deny: 2 };

type Json = Record<string, unknown>;

function isObject(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string" && item.trim() !== "") : [];
}

/** `{ "add": [...], "remove": [...] }` edits a default list; a bare array adds. */
function editList(base: string[], value: unknown): string[] {
  if (Array.isArray(value)) return [...new Set([...base, ...strings(value)])];
  if (!isObject(value)) return base;
  const remove = new Set(strings(value.remove));
  return [...new Set([...base.filter((item) => !remove.has(item)), ...strings(value.add)])];
}

function applyLevels(target: Record<LevelName, LevelMatrix>, value: unknown, tightenOnly: boolean): void {
  if (!isObject(value)) return;
  for (const level of LEVELS) {
    const matrix = value[level];
    if (!isObject(matrix)) continue;
    for (const zone of ZONES) {
      const cell = matrix[zone];
      if (!isObject(cell)) continue;
      for (const capability of ["read", "write"] as const) {
        const decision = cell[capability];
        if (typeof decision !== "string" || !DECISIONS.includes(decision as Decision)) continue;
        const current = target[level][zone][capability];
        if (tightenOnly && DECISION_RANK[decision as Decision] < DECISION_RANK[current]) continue;
        target[level][zone][capability] = decision as Decision;
      }
    }
  }
}

function applyApproval(target: Record<string, ApprovalMode>, value: unknown, tightenOnly: boolean): void {
  if (!isObject(value)) return;
  for (const [key, mode] of Object.entries(value)) {
    if (typeof mode !== "string" || !MODES.includes(mode as ApprovalMode)) continue;
    const current = target[key] ?? "ask";
    if (tightenOnly && MODE_RANK[mode as ApprovalMode] < MODE_RANK[current]) continue;
    target[key] = mode as ApprovalMode;
  }
}

function applyToolOverrides(target: Record<string, Decision>, value: unknown, tightenOnly: boolean, base: Decision): void {
  if (!isObject(value)) return;
  for (const [tool, decision] of Object.entries(value)) {
    if (typeof decision !== "string" || !DECISIONS.includes(decision as Decision)) continue;
    const current = target[tool] ?? base;
    if (tightenOnly && DECISION_RANK[decision as Decision] < DECISION_RANK[current]) continue;
    target[tool] = decision as Decision;
  }
}

/** Merge the user's file over the defaults. Unknown or malformed fields are ignored. */
export function mergeUserConfig(base: PermissionsConfig, raw: unknown): PermissionsConfig {
  const config = structuredClone(base);
  if (!isObject(raw)) return config;
  if (typeof raw.defaultLevel === "string" && LEVELS.includes(raw.defaultLevel as LevelName)) {
    config.defaultLevel = raw.defaultLevel as LevelName;
  }
  applyLevels(config.levels, raw.levels, false);
  applyApproval(config.approval, raw.approval, false);
  if (isObject(raw.judge)) {
    if (typeof raw.judge.model === "string" || raw.judge.model === null) config.judge.model = raw.judge.model || null;
    if (typeof raw.judge.timeoutMs === "number" && raw.judge.timeoutMs > 0) config.judge.timeoutMs = raw.judge.timeoutMs;
    if (typeof raw.judge.minConfidence === "number") {
      config.judge.minConfidence = Math.min(1, Math.max(0.5, raw.judge.minConfidence));
    }
  }
  if (isObject(raw.ask)) {
    const timeout = raw.ask.timeoutMs;
    if (timeout === null || (typeof timeout === "number" && timeout > 0)) config.ask.timeoutMs = timeout;
  }
  config.sensitive = editList(config.sensitive, raw.sensitive);
  config.protected = editList(config.protected, raw.protected);
  config.writableSupport = editList(config.writableSupport, raw.writableSupport);
  config.envScrub = editList(config.envScrub, raw.envScrub);
  // The self-protection list cannot be edited: removing it would let the
  // agent rewrite its own policy.
  if (isObject(raw.network)) {
    if (typeof raw.network.localBinding === "boolean") config.network.localBinding = raw.network.localBinding;
    config.network.unixSockets = strings(raw.network.unixSockets);
    if (raw.network.mode === "open" || raw.network.mode === "filtered") config.network.mode = raw.network.mode;
    config.network.allowDomains = editList(config.network.allowDomains, raw.network.allowDomains);
  }
  if (isObject(raw.tools)) {
    if (typeof raw.tools.mcp === "string" && DECISIONS.includes(raw.tools.mcp as Decision)) {
      config.tools.mcp = raw.tools.mcp as Decision;
    }
    applyToolOverrides(config.tools.overrides, raw.tools.overrides, false, config.tools.mcp);
  }
  if (Array.isArray(raw.rules)) {
    config.rules = raw.rules.filter(
      (rule): rule is Rule =>
        isObject(rule) &&
        (rule.capability === "read" || rule.capability === "write") &&
        typeof rule.path === "string" &&
        (rule.path.startsWith("/") || rule.path.startsWith("tool:") || rule.path.startsWith("net:")),
    );
  }
  return config;
}

/** A project's file can add sensitive/protected patterns and make decisions stricter, nothing else. */
export function mergeProjectConfig(base: PermissionsConfig, raw: unknown): PermissionsConfig {
  const config = structuredClone(base);
  if (!isObject(raw)) return config;
  const additions = (value: unknown) => (Array.isArray(value) ? strings(value) : isObject(value) ? strings(value.add) : []);
  // An exception (`!pattern`) would widen access, so a project cannot add one.
  const tighten = (list: string[], value: unknown) => [
    ...new Set([...list, ...additions(value).filter((pattern) => !pattern.startsWith("!"))]),
  ];
  config.sensitive = tighten(config.sensitive, raw.sensitive);
  config.protected = tighten(config.protected, raw.protected);
  applyLevels(config.levels, raw.levels, true);
  applyApproval(config.approval, raw.approval, true);
  // A project may ask for the filtered network, never for the open one.
  if (isObject(raw.network) && raw.network.mode === "filtered") config.network.mode = "filtered";
  if (isObject(raw.tools)) {
    if (typeof raw.tools.mcp === "string" && DECISIONS.includes(raw.tools.mcp as Decision)) {
      if (DECISION_RANK[raw.tools.mcp as Decision] > DECISION_RANK[config.tools.mcp]) {
        config.tools.mcp = raw.tools.mcp as Decision;
      }
    }
    applyToolOverrides(config.tools.overrides, raw.tools.overrides, true, config.tools.mcp);
  }
  return config;
}

function readJson(path: string): unknown {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return undefined;
  }
}

export interface LoadedConfig {
  config: PermissionsConfig;
  /** Problems worth showing in `/perm`; the config still loads. */
  warnings: string[];
}

export function loadConfig(agentDir: string, projectRoots: readonly string[]): LoadedConfig {
  const warnings: string[] = [];
  const userPath = join(agentDir, CONFIG_FILE);
  let config = defaultConfig();
  if (existsSync(userPath)) {
    const raw = readJson(userPath);
    if (raw === undefined) warnings.push(`${userPath} is not valid JSON; using the defaults`);
    config = mergeUserConfig(config, raw);
  }
  for (const root of projectRoots) {
    const projectPath = join(root, PROJECT_CONFIG_FILE);
    if (!existsSync(projectPath)) continue;
    const raw = readJson(projectPath);
    if (raw === undefined) warnings.push(`${projectPath} is not valid JSON; ignored`);
    config = mergeProjectConfig(config, raw);
  }
  // Safety rules that no file can turn off.
  for (const key of ["sensitive.read", "sensitive.write"]) {
    if (config.approval[key] === "assisted") config.approval[key] = "ask";
  }
  return { config, warnings };
}

/** Append one persistent rule to the user's file, keeping everything else as written. */
export function appendRule(agentDir: string, rule: Rule): void {
  const path = join(agentDir, CONFIG_FILE);
  const raw = readJson(path);
  const root: Json = isObject(raw) ? raw : { version: 1 };
  const rules = Array.isArray(root.rules) ? root.rules : [];
  rules.push(rule);
  root.rules = rules;
  mkdirSync(dirname(path), { recursive: true });
  const temporary = `${path}.tmp-${process.pid}`;
  writeFileSync(temporary, `${JSON.stringify(root, null, 2)}\n`, { mode: 0o600 });
  renameSync(temporary, path);
}

const LEVEL_FROM_SANDBOX: Record<string, LevelName> = {
  "read-only": "read-only",
  "workspace-write": "standard",
  "danger-full-access": "full-access",
};

/** The chat's level from Cypher's per-chat sandbox setting, else the configured default. */
export function resolveLevel(env: string | undefined, config: PermissionsConfig): LevelName {
  if (env && LEVEL_FROM_SANDBOX[env]) return LEVEL_FROM_SANDBOX[env];
  if (env && LEVELS.includes(env as LevelName)) return env as LevelName;
  return config.defaultLevel;
}

export function approvalMode(config: PermissionsConfig, key: string): ApprovalMode {
  const mode = config.approval[key] ?? "ask";
  if ((key === "sensitive.read" || key === "sensitive.write" || key === "self.write") && mode === "assisted") {
    return "ask";
  }
  if (key === "self.write") return mode === "deny" ? "deny" : "ask";
  return mode;
}
