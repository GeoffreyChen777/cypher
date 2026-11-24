/**
 * Typed targets by declaration (round 6, W2; design §7.3): per tool, which argv position or option names
 * which kind of entity, and — for REST calls (`gh api`, `glab api`, `curl` to a forge's API) — which
 * HTTP method × route is which effect class on which entity. Denial matching (denials.ts) and the
 * other-task floor read these typed targets; no per-command parsing.
 *
 * - `TARGET_SCHEMAS`: per program, its option roles and its subcommands' effects and operand kinds.
 *   An option's role is `target` (it names what is acted on), `within` (where the named operands are —
 *   a namespace, a repository — and the target itself only when no operand is named: `kubectl delete all
 *   --all -n X` acts on namespace X) or `env` (the cluster, context, profile it runs against: never a
 *   target, so two deletes in one context don't match each other).
 * - `API_ROUTES`: method × route pattern → effect and entities (`:name` one segment, `:name*` the rest).
 *
 * `typedTargets(words)` reads one command (already unwrapped) by these tables.
 */

import { basename } from "node:path";

import type { EffectClass } from "./effects.ts";
import type { EntityKind } from "./entities.ts";
import { UNKNOWN } from "./shell.ts";

export type OptionRole = "target" | "within" | "env";

export interface SubSchema {
  sub: readonly string[];
  effect: EffectClass;
  /** What the operands after `sub` name, in order; `type`: a resource type word (`types` may make it a kind). */
  operands?: readonly (EntityKind | "type")[];
  /** The last operand kind repeats. */
  rest?: boolean;
}

export interface TargetSchema {
  programs: readonly string[];
  /** Options whose value names an entity, by role. */
  options?: Readonly<Record<string, { kind: EntityKind; role: OptionRole }>>;
  /** Other options that take a value (their values are no operands). */
  values?: readonly string[];
  subs?: readonly SubSchema[];
  /** Resource-type words that name an entity kind of their own (`kubectl delete ns X`: a namespace). */
  types?: Readonly<Record<string, EntityKind>>;
  /** Other names of resource types, by their canonical name (`deploy` → `deployment`). */
  typeNames?: Readonly<Record<string, string>>;
  /** Operand URL schemes that name an entity (`s3://bucket/key`: the bucket's object). */
  schemes?: Readonly<Record<string, EntityKind>>;
  /** A REST call: the subcommand, the options that set the method, the options that make it a POST. */
  api?: { sub: readonly string[]; method: readonly string[]; body: readonly string[]; put?: readonly string[] };
}

const KUBE_TYPES: Readonly<Record<string, EntityKind>> = { ns: "namespace", namespace: "namespace", namespaces: "namespace" };
/** Resource types by their short and plural names (round 6 step 2, data): `deploy/api` and `deployment api` are one target. */
const KUBE_TYPE_NAMES: Readonly<Record<string, string>> = {
  deploy: "deployment", deployments: "deployment", po: "pod", pods: "pod", svc: "service", services: "service", sts: "statefulset", statefulsets: "statefulset",
  ds: "daemonset", daemonsets: "daemonset", cm: "configmap", configmaps: "configmap", secrets: "secret", ing: "ingress", ingresses: "ingress", jobs: "job",
  cj: "cronjob", cronjobs: "cronjob", pvc: "persistentvolumeclaim", persistentvolumeclaims: "persistentvolumeclaim", rs: "replicaset", replicasets: "replicaset",
  sa: "serviceaccount", serviceaccounts: "serviceaccount", hpa: "horizontalpodautoscaler", no: "node", nodes: "node", pv: "persistentvolume", crd: "customresourcedefinition",
};
const KUBE_OPTIONS = {
  "-n": { kind: "namespace", role: "within" },
  "--namespace": { kind: "namespace", role: "within" },
  "--context": { kind: "context", role: "env" },
  "--cluster": { kind: "cluster", role: "env" },
  "--kube-context": { kind: "context", role: "env" },
} as const;
const REPO_OPTIONS = { "-R": { kind: "repo", role: "within" }, "--repo": { kind: "repo", role: "within" } } as const;

export const TARGET_SCHEMAS: readonly TargetSchema[] = [
  {
    programs: ["kubectl", "oc"],
    options: KUBE_OPTIONS,
    values: ["-f", "--filename", "-l", "--selector", "-o", "--output", "--replicas", "--grace-period", "--timeout", "-c", "--container", "--kubeconfig", "--field-selector"],
    types: KUBE_TYPES,
    typeNames: KUBE_TYPE_NAMES,
    subs: [
      { sub: ["delete"], effect: "change", operands: ["type", "resource"], rest: true },
      { sub: ["drain"], effect: "stop", operands: ["resource"] },
      { sub: ["cordon"], effect: "stop", operands: ["resource"] },
      { sub: ["scale"], effect: "install", operands: ["type", "resource"], rest: true },
      { sub: ["rollout"], effect: "install", operands: ["type", "type", "resource"] },
      { sub: ["apply"], effect: "install" },
      { sub: ["patch"], effect: "install", operands: ["type", "resource"] },
      { sub: ["edit"], effect: "install", operands: ["type", "resource"] },
      { sub: ["label"], effect: "install", operands: ["type", "resource"] },
      { sub: ["get"], effect: "read", operands: ["type", "resource"], rest: true },
      { sub: ["describe"], effect: "read", operands: ["type", "resource"], rest: true },
      { sub: ["logs"], effect: "read", operands: ["resource"] },
      { sub: ["exec"], effect: "run", operands: ["resource"] },
    ],
  },
  {
    programs: ["helm"],
    options: { "-n": { kind: "namespace", role: "within" }, "--namespace": { kind: "namespace", role: "within" }, "--kube-context": { kind: "context", role: "env" } },
    values: ["-f", "--values", "--set", "--set-string", "--version", "--timeout", "-o", "--output", "--kubeconfig", "--repo"],
    subs: [
      { sub: ["uninstall"], effect: "change", operands: ["resource"], rest: true },
      { sub: ["delete"], effect: "change", operands: ["resource"], rest: true },
      { sub: ["install"], effect: "install", operands: ["resource"] },
      { sub: ["upgrade"], effect: "install", operands: ["resource"] },
      { sub: ["rollback"], effect: "install", operands: ["resource"] },
    ],
  },
  {
    programs: ["docker", "podman"],
    options: { "--context": { kind: "context", role: "env" }, "-H": { kind: "host", role: "env" }, "--host": { kind: "host", role: "env" }, "--connection": { kind: "host", role: "env" } },
    // `-f` is no value option here: `docker rm -f`, `rmi -f` force (`ps -f` filters, but a read names nothing).
    values: ["--filter", "--format", "-t", "--time", "-s", "--signal", "--platform"],
    subs: [
      { sub: ["rm"], effect: "change", operands: ["container"], rest: true },
      { sub: ["container", "rm"], effect: "change", operands: ["container"], rest: true },
      { sub: ["rmi"], effect: "change", operands: ["image"], rest: true },
      { sub: ["image", "rm"], effect: "change", operands: ["image"], rest: true },
      { sub: ["volume", "rm"], effect: "change", operands: ["resource"], rest: true },
      { sub: ["network", "rm"], effect: "change", operands: ["resource"], rest: true },
      { sub: ["stop"], effect: "stop", operands: ["container"], rest: true },
      { sub: ["kill"], effect: "stop", operands: ["container"], rest: true },
      { sub: ["restart"], effect: "stop", operands: ["container"], rest: true },
      { sub: ["push"], effect: "push", operands: ["image"] },
      { sub: ["image", "push"], effect: "push", operands: ["image"] },
      { sub: ["exec"], effect: "run", operands: ["container"] },
      { sub: ["logs"], effect: "read", operands: ["container"] },
      { sub: ["inspect"], effect: "read", operands: ["container"], rest: true },
    ],
  },
  {
    programs: ["gh"],
    options: { ...REPO_OPTIONS, "--hostname": { kind: "host", role: "env" } },
    values: ["-X", "--method", "-f", "-F", "--field", "--raw-field", "-H", "--header", "--input", "-q", "--jq", "-t", "--template", "--cache", "-p", "--preview"],
    api: { sub: ["api"], method: ["-X", "--method"], body: ["-f", "-F", "--field", "--raw-field", "--input"] },
  },
  {
    programs: ["glab"],
    options: { ...REPO_OPTIONS },
    values: ["-X", "--method", "-f", "-F", "--field", "--raw-field", "-H", "--header", "--input", "--hostname"],
    api: { sub: ["api"], method: ["-X", "--method"], body: ["-f", "-F", "--field", "--raw-field", "--input"] },
  },
  {
    programs: ["curl", "http", "https", "xh"],
    values: ["-d", "--data", "--data-raw", "--data-binary", "--data-urlencode", "-F", "--form", "-H", "--header", "-o", "--output", "-u", "--user", "-A", "--user-agent", "-e", "--referer", "-T", "--upload-file", "-w", "--write-out", "-m", "--max-time", "--json", "-b", "--cookie", "-c", "--cookie-jar"],
    api: { sub: [], method: ["-X", "--request"], body: ["-d", "--data", "--data-raw", "--data-binary", "--data-urlencode", "-F", "--form", "--json"], put: ["-T", "--upload-file"] },
  },
  {
    programs: ["aws"],
    options: { "--profile": { kind: "context", role: "env" }, "--region": { kind: "context", role: "env" }, "--bucket": { kind: "bucket", role: "target" }, "--cluster": { kind: "cluster", role: "within" } },
    values: ["--output", "--query", "--endpoint-url", "--key", "--prefix"],
    schemes: { "s3://": "bucket" },
    subs: [
      { sub: ["s3", "rm"], effect: "change" },
      { sub: ["s3", "rb"], effect: "change" },
      { sub: ["s3", "mv"], effect: "change" },
      { sub: ["s3", "sync"], effect: "change" },
      { sub: ["s3", "cp"], effect: "change" },
      { sub: ["s3api", "delete-bucket"], effect: "change" },
      { sub: ["s3api", "delete-object"], effect: "change" },
      { sub: ["s3", "ls"], effect: "read" },
    ],
  },
  {
    programs: ["gcloud", "gsutil"],
    options: { "--project": { kind: "project", role: "within" }, "--configuration": { kind: "context", role: "env" } },
    values: ["--format", "--zone", "--region", "--filter", "-m"],
    schemes: { "gs://": "bucket" },
    subs: [
      { sub: ["rm"], effect: "change" },
      { sub: ["rb"], effect: "change" },
      { sub: ["storage", "rm"], effect: "change" },
      { sub: ["storage", "buckets", "delete"], effect: "change" },
      { sub: ["projects", "delete"], effect: "change", operands: ["project"] },
      { sub: ["compute", "instances", "delete"], effect: "change", operands: ["resource"], rest: true },
      { sub: ["compute", "instances", "stop"], effect: "stop", operands: ["resource"], rest: true },
      { sub: ["container", "clusters", "delete"], effect: "change", operands: ["cluster"] },
      { sub: ["run", "services", "delete"], effect: "change", operands: ["resource"] },
      { sub: ["sql", "instances", "delete"], effect: "change", operands: ["resource"] },
    ],
  },
];

/** One REST route: the methods, the path after the API's root, the effect and which parameters are which entities. */
export interface ApiRoute {
  methods: readonly string[];
  route: string;
  effect: EffectClass;
  /** Parameter → entity kind; `repo` is `:owner/:repo` (GitHub) or `:project` (GitLab). */
  entities?: Readonly<Record<string, EntityKind>>;
}

const WRITE = ["POST", "PUT", "PATCH", "DELETE"];

/** GitHub (`repos/…`) and GitLab (`projects/…`, under `/api/v4/`) routes. Most specific first. */
export const API_ROUTES: readonly ApiRoute[] = [
  { methods: ["PUT", "POST"], route: "repos/:owner/:repo/pulls/:pr/merge", effect: "merge", entities: { pr: "pr" } },
  { methods: ["PUT", "POST"], route: "projects/:project/merge_requests/:pr/merge", effect: "merge", entities: { pr: "pr" } },
  { methods: ["POST"], route: "repos/:owner/:repo/merges", effect: "merge" },
  { methods: ["PATCH"], route: "repos/:owner/:repo/pulls/:pr", effect: "change", entities: { pr: "pr" } },
  { methods: ["PUT", "DELETE"], route: "projects/:project/merge_requests/:pr", effect: "change", entities: { pr: "pr" } },
  { methods: ["POST"], route: "repos/:owner/:repo/pulls/:pr/reviews", effect: "comment", entities: { pr: "pr" } },
  { methods: ["POST"], route: "repos/:owner/:repo/issues/:pr/comments", effect: "comment", entities: { pr: "pr" } },
  { methods: ["POST"], route: "projects/:project/merge_requests/:pr/notes", effect: "comment", entities: { pr: "pr" } },
  { methods: ["DELETE"], route: "repos/:owner/:repo/git/refs/heads/:branch*", effect: "change", entities: { branch: "branch" } },
  { methods: ["PATCH", "POST"], route: "repos/:owner/:repo/git/refs/heads/:branch*", effect: "push", entities: { branch: "branch" } },
  { methods: ["DELETE"], route: "repos/:owner/:repo/git/refs/tags/:tag*", effect: "change", entities: { tag: "tag" } },
  { methods: ["PATCH", "POST"], route: "repos/:owner/:repo/git/refs/tags/:tag*", effect: "publish", entities: { tag: "tag" } },
  { methods: ["POST"], route: "repos/:owner/:repo/git/refs", effect: "push" },
  { methods: ["DELETE"], route: "repos/:owner/:repo/branches/:branch/protection", effect: "change", entities: { branch: "branch" } },
  { methods: ["DELETE"], route: "projects/:project/repository/branches/:branch*", effect: "change", entities: { branch: "branch" } },
  { methods: ["DELETE"], route: "projects/:project/repository/tags/:tag*", effect: "change", entities: { tag: "tag" } },
  { methods: ["DELETE", "PATCH"], route: "repos/:owner/:repo/releases/:release", effect: "change", entities: { release: "release" } },
  { methods: ["POST"], route: "repos/:owner/:repo/releases", effect: "publish" },
  { methods: ["POST"], route: "projects/:project/releases", effect: "publish" },
  { methods: ["PUT", "DELETE"], route: "repos/:owner/:repo/contents/:path*", effect: "push" },
  { methods: ["DELETE"], route: "repos/:owner/:repo", effect: "change" },
  { methods: ["PATCH"], route: "repos/:owner/:repo", effect: "change" },
  { methods: ["DELETE"], route: "projects/:project", effect: "change" },
  { methods: ["POST"], route: "gists", effect: "send" },
  { methods: WRITE, route: "repos/:owner/:repo/:rest*", effect: "change" },
  { methods: WRITE, route: "projects/:project/:rest*", effect: "change" },
  { methods: ["GET", "HEAD"], route: ":rest*", effect: "read" },
];

/** An entity `typedTargets` found, with its role. */
export interface TypedTarget {
  kind: EntityKind;
  name: string;
  repo?: string;
  scope?: string;
  type?: string;
}

export interface Typed {
  effects: EffectClass[];
  /** What the command acts on: operands, `target` options, a route's entities; a `within` option when nothing narrower is named. */
  targets: TypedTarget[];
  /** Where it acts (`within` options) and what it runs against (`env`): for the evidence, not denial matching. */
  qualifiers: TypedTarget[];
}

const SCHEMA_BY_PROGRAM = new Map<string, TargetSchema>();
for (const schema of TARGET_SCHEMAS) for (const program of schema.programs) SCHEMA_BY_PROGRAM.set(program, schema);

/** Whether the schema table declares `program`. */
export function hasSchema(program: string): boolean {
  return SCHEMA_BY_PROGRAM.has(basename(program));
}

/** A route path (`repos/acme/web/pulls/52/merge`) against a pattern; the parameters, or undefined. */
export function matchRoute(pattern: string, path: string): Record<string, string> | undefined {
  const want = pattern.split("/");
  const have = path.split("/").filter(Boolean);
  const out: Record<string, string> = {};
  for (let i = 0; i < want.length; i++) {
    const part = want[i];
    if (part.endsWith("*") && part.startsWith(":")) {
      const rest = have.slice(i);
      if (!rest.length) return undefined;
      out[part.slice(1, -1)] = rest.map(decodeURIComponent).join("/");
      return out;
    }
    const value = have[i];
    if (value === undefined) return undefined;
    if (part.startsWith(":")) out[part.slice(1)] = decodeURIComponent(value);
    else if (part !== value) return undefined;
  }
  return have.length === want.length ? out : undefined;
}

/** The API path of a URL or a `gh api` endpoint (`https://api.github.com/repos/…`, `/api/v4/projects/…`, `repos/…`), query read off. */
export function apiPath(word: string): string | undefined {
  let path = word;
  const url = /^[a-z][\w+.-]*:\/\/([^/?#]+)(\/[^?#]*)?/i.exec(word);
  if (url) {
    const host = url[1].toLowerCase();
    path = url[2] ?? "/";
    const v4 = path.indexOf("/api/v4/");
    const v3 = path.indexOf("/api/v3/");
    if (v4 >= 0) path = path.slice(v4 + "/api/v4/".length);
    else if (v3 >= 0) path = path.slice(v3 + "/api/v3/".length);
    else if (!/^api\./.test(host)) return undefined;
  }
  path = path.replace(/[?#].*$/, "").replace(/^\/+/, "");
  return path || undefined;
}

/** The route's effect and entities for a method and path. */
export function routeOf(method: string, path: string): { effect: EffectClass; targets: TypedTarget[] } | undefined {
  for (const row of API_ROUTES) {
    if (!row.methods.includes(method)) continue;
    const params = matchRoute(row.route, path);
    if (!params) continue;
    const repo = params.owner && params.repo ? `${params.owner}/${params.repo}` : params.project;
    const targets: TypedTarget[] = [];
    for (const [param, kind] of Object.entries(row.entities ?? {})) if (params[param]) targets.push({ kind, name: params[param], ...(repo ? { repo } : {}) });
    if (!targets.length && repo && row.effect !== "read") targets.push({ kind: "repo", name: repo });
    return { effect: row.effect, targets };
  }
  return undefined;
}

/**
 * REST calls that destroy what their route names (round 6 step 2, data): a DELETE, or a write whose body sets one of
 * `DESTROYING_FIELDS` (`state=closed` closes a pull request or issue).
 */
export const DESTROYING_METHODS: ReadonlySet<string> = new Set(["DELETE"]);
export const DESTROYING_FIELDS = /^(state=closed|state_event=close|archived=true)$/i;

/** Whether a command is a REST call (`TargetSchema.api`) that destroys (`DESTROYING_METHODS`, `DESTROYING_FIELDS`). */
export function apiDestroys(words: readonly string[]): boolean {
  const schema = SCHEMA_BY_PROGRAM.get(basename(words[0] ?? ""));
  if (!schema?.api) return false;
  for (const [index, word] of words.entries()) {
    const glued = schema.api.method.map((option) => (word.startsWith(`${option}=`) ? word.slice(option.length + 1) : option.length === 2 && word.startsWith(option) && word.length > 2 ? word.slice(2) : undefined)).find((value) => value !== undefined);
    const method = schema.api.method.includes(word) ? words[index + 1] : glued;
    if (method && DESTROYING_METHODS.has(method.toUpperCase())) return true;
    if (schema.api.body.includes(word) && DESTROYING_FIELDS.test(words[index + 1] ?? "")) return true;
  }
  return false;
}

/** A subcommand word that deletes (data, round 6 step 2). */
export const DELETING_SUBCOMMAND = /^(delete|remove|rm|rb|terminate|destroy|purge|deregister|drop)(-[a-z-]+)?$/;

/** One command's words (unwrapped), read by its schema: undefined when the table has none for it. */
export function typedTargets(words: readonly string[]): Typed | undefined {
  const program = basename(words[0] ?? "");
  const schema = SCHEMA_BY_PROGRAM.get(program);
  if (!schema) return undefined;
  const out: Typed = { effects: [], targets: [], qualifiers: [] };
  const takes = new Set([...Object.keys(schema.options ?? {}), ...(schema.values ?? []), ...(schema.api?.method ?? []), ...(schema.api?.body ?? []), ...(schema.api?.put ?? [])]);
  const positionals: string[] = [];
  const options: [string, string][] = [];
  for (let i = 1; i < words.length; i++) {
    const word = words[i];
    if (word === "--") {
      positionals.push(...words.slice(i + 1));
      break;
    }
    const eq = word.startsWith("--") ? word.indexOf("=") : -1;
    if (eq > 0) {
      options.push([word.slice(0, eq), word.slice(eq + 1)]);
      continue;
    }
    if (word.startsWith("-") && word.length > 1) {
      if (takes.has(word)) {
        options.push([word, words[i + 1] ?? UNKNOWN]);
        i++;
      } else if (/^-[A-Za-z]$/.test(word.slice(0, 2)) && takes.has(word.slice(0, 2)) && word.length > 2) options.push([word.slice(0, 2), word.slice(2)]);
      else options.push([word, ""]);
      continue;
    }
    positionals.push(word);
  }
  const within: TypedTarget[] = [];
  for (const [option, value] of options) {
    const spec = schema.options?.[option];
    if (!spec || !value || value === UNKNOWN) continue;
    const entity = { kind: spec.kind, name: value };
    if (spec.role === "target") out.targets.push(entity);
    else if (spec.role === "within") within.push(entity);
    out.qualifiers.push(entity);
  }
  // A REST call: its method and route.
  if (schema.api && schema.api.sub.every((word, index) => positionals[index] === word)) {
    const endpoint = positionals.slice(schema.api.sub.length).map(apiPath).find((path) => path !== undefined);
    if (endpoint !== undefined) {
      const given = options.find(([option]) => schema.api!.method.includes(option))?.[1]?.toUpperCase();
      const method = given ?? (options.some(([option]) => schema.api!.put?.includes(option)) ? "PUT" : options.some(([option]) => schema.api!.body.includes(option)) ? "POST" : "GET");
      const route = routeOf(method, endpoint);
      if (route) {
        out.effects.push(route.effect);
        out.targets.push(...route.targets);
      }
    }
    return out.effects.length || out.targets.length ? out : undefined;
  }
  // Subcommands: the longest that matches.
  const sub = [...(schema.subs ?? [])].sort((a, b) => b.sub.length - a.sub.length).find((row) => row.sub.every((word, index) => positionals[index] === word));
  const operands = sub ? positionals.slice(sub.sub.length) : [];
  // A subcommand the table has no row for whose verb deletes (`aws s3api delete-objects`, `ec2 terminate-instances`): a change (round 6 step 2).
  if (!sub && schema.subs && positionals.slice(0, 3).some((word) => DELETING_SUBCOMMAND.test(word))) out.effects.push("change");
  if (sub) {
    out.effects.push(sub.effect);
    const kinds = sub.operands ?? [];
    let type: string | undefined;
    for (const [index, word] of operands.entries()) {
      const kind = index < kinds.length ? kinds[index] : sub.rest ? kinds.at(-1) : undefined;
      if (!kind || word === UNKNOWN) break;
      if (kind === "type") {
        // `deployment/web`: a type and a name in one word.
        if (word.includes("/")) {
          const [given, name] = [word.slice(0, word.indexOf("/")), word.slice(word.indexOf("/") + 1)];
          const t = schema.typeNames?.[given.toLowerCase()] ?? given;
          const own = schema.types?.[t];
          out.targets.push(own ? { kind: own, name } : { kind: "resource", name, type: t });
          continue;
        }
        type = schema.typeNames?.[word.toLowerCase()] ?? word;
        continue;
      }
      const own = kind === "resource" && type ? schema.types?.[type] : undefined;
      out.targets.push(own ? { kind: own, name: word } : { kind, name: word, ...(kind === "resource" && type ? { type } : {}) });
    }
  }
  // Operands with a scheme (`s3://bucket/key`): their entity.
  for (const word of positionals) {
    for (const [scheme, kind] of Object.entries(schema.schemes ?? {})) if (word.startsWith(scheme) && word.length > scheme.length) out.targets.push({ kind, name: word.slice(scheme.length).replace(/\/+$/, "") });
  }
  // Where the named operands are; with none named, the target itself (`delete all --all -n X`: namespace X).
  const scope = within.find((entity) => entity.kind === "namespace")?.name;
  for (const target of out.targets) if (scope && target.kind === "resource" && target.scope === undefined) target.scope = scope;
  if (!out.targets.length && sub && sub.effect !== "read") out.targets.push(...within);
  return out.effects.length || out.targets.length || out.qualifiers.length ? out : undefined;
}
