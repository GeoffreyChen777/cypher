/**
 * Target identity: what a command acts on, and what a consent names, as
 * typed entities, compared by one relation instead of by their strings.
 *
 * "Merge the branch fix-badge" and `gh pr merge 52` (the pull request from
 * fix-badge into main), `cypher/x` and `GeoffreyChen777/cypher/x`,
 * `origin/x` and `x`, a release and its tag, a file and the folder it is
 * in, `gamma` and `gamma-cluster`, a simulator's UDID and its name: each
 * pair names the same thing, or one holds the other, while their strings
 * differ. `relation` decides it from the entities and what is known about
 * them (`EntityContext`: the current repository, its remotes, pull request
 * facts, simulators), and says "unknown" when that isn't enough. Nothing
 * here knows a particular command; commands become entities in tools.ts.
 *
 * The relation, for `relation(a, b)`:
 * - `same`: both name one thing (symmetric, transitive).
 * - `contains`: acting on `b` is acting on part of `a` (a folder and a file
 *   in it, a repository and its branch, a pull request and its head
 *   branch, a package and one of its versions, a host family `gamma` and
 *   `gamma-cluster`); antisymmetric and transitive.
 * - `contained`: the converse.
 * - `unrelated`: different things.
 * - `unknown`: can't tell from what is known (a pull request whose head
 *   branch isn't known, a relative path with no directory).
 * A consent covers an entity only when the entity is `same` as or
 * `contained` in one the consent names; `unknown` never covers.
 *
 * A pull request's base is deliberately `unrelated` to it: naming `main`
 * must not cover every pull request into main. A model may still identify
 * a new pull request by its base (`identifies`).
 */

import { isAbsolute, normalize as normalizePath, resolve } from "node:path";

export type EntityKind =
  | "repo"
  | "remote"
  | "branch"
  | "pr"
  | "tag"
  | "release"
  | "path"
  | "worktree"
  | "process"
  | "simulator"
  | "host"
  | "package"
  | "container"
  | "image"
  | "resource"
  | "namespace"
  | "context"
  | "cluster"
  | "bucket"
  | "project";

export const ENTITY_KINDS: readonly EntityKind[] = [
  "repo", "remote", "branch", "pr", "tag", "release", "path", "worktree", "process", "simulator", "host", "package", "container", "image", "resource", "namespace", "context", "cluster", "bucket", "project",
];

/** Kinds that live in a repository: qualifying them with the current one changes nothing. */
const IN_REPO: ReadonlySet<EntityKind> = new Set(["branch", "pr", "tag", "release"]);

export interface Entity {
  kind: EntityKind;
  /**
   * Its name: a repository's `owner/name` (or a bare `name`), a branch, a
   * pull request's number (empty for one not opened yet), a tag, a path, a
   * process id or name, a simulator's UDID or name, a host, a package's
   * name, a container, an image, a resource's name.
   */
  name: string;
  /** The repository (`owner/name`) a branch, pull request, tag or release is in. */
  repo?: string;
  /** The remote a branch is on (`origin` when unsaid). */
  remote?: string;
  /** A pull request's head and base branches, when known. */
  head?: string;
  base?: string;
  /** A package's version (none: every version). */
  version?: string;
  /** A resource's type (`deployment`, `helm-release`), a container's engine. */
  type?: string;
  /** A resource's namespace or context. */
  scope?: string;
  /** A simulator's UDID, when known besides its name. */
  udid?: string;
}

/** A pull request as this chat knows it. */
export interface PullRequestFact {
  number: string;
  repo?: string;
  head?: string;
  base?: string;
  /** `open`, `merged` or `closed`, when known (gh's state, lower case). */
  state?: string;
}

/** What is known besides the entities themselves. */
export interface EntityContext {
  /** The `owner/name` of the repository at hand (origin's), lower case. */
  currentRepo?: string;
  /** Remote name → `owner/name`. `origin` is the current repository when not given. */
  remotes?: Readonly<Record<string, string>>;
  /** Pull requests known in this chat (opened by it, or read from the command line). */
  prs?: readonly PullRequestFact[];
  /** Simulators known by UDID and name. */
  simulators?: readonly { udid: string; name: string }[];
  /** For relative paths and `~`. */
  cwd?: string;
  home?: string;
}

export type Relation = "same" | "contains" | "contained" | "unrelated" | "unknown";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** `owner/name` from a GitHub/GitLab URL, `host/owner/name`, `owner/name`; a bare name stays bare. Lower case. */
export function repoOf(text: string): string {
  const url = /^(?:[a-z][\w+.-]*:\/\/)?(?:[^@/]*@)?(?:github\.com|gitlab\.com|[\w.-]+\.[a-z]{2,})[/:]([\w.-]+)\/([\w.-]+?)(?:\.git)?(?:[/#?]|$)/i.exec(text);
  if (url) return `${url[1]}/${url[2]}`.toLowerCase();
  const parts = text.replace(/\.git$/, "").split("/").filter(Boolean);
  return (parts.length >= 2 ? `${parts.at(-2)}/${parts.at(-1)}` : (parts[0] ?? "")).toLowerCase();
}

function remoteRepo(remote: string | undefined, context: EntityContext): string | undefined {
  const name = remote ?? "origin";
  const mapped = context.remotes?.[name];
  if (mapped) return repoOf(mapped);
  return name === "origin" ? context.currentRepo : undefined;
}

function knownRemotes(context: EntityContext): string[] {
  return [...new Set(["origin", ...Object.keys(context.remotes ?? {})])];
}

/**
 * An entity in its canonical form: repositories qualified with the current
 * one, `refs/heads/` and a remote's prefix off a branch, `#` off a pull
 * request (its head and base filled in from what is known), paths absolute
 * and normalised, hosts without user, scheme or port, simulators with
 * their UDID and name both when known.
 */
export function canonical(entity: Entity, context: EntityContext = {}): Entity {
  const out: Entity = { ...entity, name: entity.name.trim() };
  if (out.repo !== undefined) out.repo = repoOf(out.repo) || undefined;
  switch (out.kind) {
    case "repo": {
      const name = repoOf(out.name);
      // A bare name that is the current repository's is it.
      out.name = !name.includes("/") && context.currentRepo?.split("/")[1] === name ? context.currentRepo : name;
      return out;
    }
    case "remote":
      out.repo ??= remoteRepo(out.name, context);
      return out;
    case "branch": {
      let name = out.name.replace(/^refs\/heads\//, "").replace(/^refs\/remotes\//, "remotes/").replace(/^remotes\//, "");
      const slash = name.indexOf("/");
      if (slash > 0 && out.remote === undefined && knownRemotes(context).includes(name.slice(0, slash))) {
        out.remote = name.slice(0, slash);
        name = name.slice(slash + 1);
      }
      out.name = name;
      out.remote ??= "origin";
      out.repo ??= remoteRepo(out.remote, context);
      return out;
    }
    case "pr": {
      const qualified = /^([\w.-]+\/[\w.-]+)[#!](\d+)$/.exec(out.name);
      if (qualified) out.repo ??= qualified[1].toLowerCase();
      const number = qualified?.[2] ?? /(?:^|\/pull\/|\/merge_requests\/|^#|^!)(\d+)$/.exec(out.name)?.[1] ?? (/^\d+$/.test(out.name) ? out.name : undefined);
      if (number) out.name = number;
      if (/\/(?:pull|merge_requests)\/\d+/.test(entity.name)) out.repo ??= repoOf(entity.name);
      out.repo ??= context.currentRepo;
      const fact = number
        ? context.prs?.find((pr) => pr.number === number && (!pr.repo || !out.repo || repoOf(pr.repo) === out.repo))
        : undefined;
      if (fact?.head) out.head ??= fact.head;
      if (fact?.base) out.base ??= fact.base;
      if (out.head) out.head = out.head.replace(/^refs\/heads\//, "");
      if (out.base) out.base = out.base.replace(/^refs\/heads\//, "");
      return out;
    }
    case "tag":
    case "release":
      out.name = out.name.replace(/^refs\/tags\//, "");
      out.repo ??= context.currentRepo;
      return out;
    case "path":
    case "worktree": {
      const home = context.home;
      let path = out.name;
      if (path === "~" || path.startsWith("~/")) {
        if (!home) return { ...out, name: path };
        path = home + path.slice(1);
      }
      if (!isAbsolute(path)) {
        if (!context.cwd) return { ...out, name: path };
        path = resolve(context.cwd, path);
      }
      path = normalizePath(path);
      out.name = path.length > 1 ? path.replace(/\/+$/, "") : path;
      return out;
    }
    case "host":
      out.name = out.name.toLowerCase().replace(/^[a-z][\w+.-]*:\/\//, "").replace(/^[^@/]*@/, "").replace(/[/?#].*$/, "").replace(/:\d+$/, "").replace(/\.$/, "");
      return out;
    case "simulator": {
      const known = context.simulators ?? [];
      if (UUID.test(out.name)) {
        out.udid = out.name.toUpperCase();
        const named = known.find((sim) => sim.udid.toUpperCase() === out.udid);
        if (named) out.name = named.name;
      } else if (out.udid) out.udid = out.udid.toUpperCase();
      else {
        const named = known.filter((sim) => sim.name === out.name);
        if (named.length === 1) out.udid = named[0].udid.toUpperCase();
      }
      return out;
    }
    case "package": {
      const at = out.name.lastIndexOf("@");
      if (at > 0 && out.version === undefined) {
        out.version = out.name.slice(at + 1) || undefined;
        out.name = out.name.slice(0, at);
      }
      out.name = out.name.toLowerCase();
      return out;
    }
    default:
      return out;
  }
}

function flip(relation: Relation): Relation {
  return relation === "contains" ? "contained" : relation === "contained" ? "contains" : relation;
}

/** Whether two repositories (canonical names) are one: `owner/name` both; a bare `name` holds every `owner/name`. */
function repoRelation(a: string | undefined, b: string | undefined): Relation {
  if (a === undefined || b === undefined) return a === b ? "same" : "unknown";
  if (a === b) return "same";
  const aBare = !a.includes("/");
  const bBare = !b.includes("/");
  if (aBare && !bBare) return b.split("/")[1] === a ? "contains" : "unrelated";
  if (bBare && !aBare) return a.split("/")[1] === b ? "contained" : "unrelated";
  return "unrelated";
}

/** Two things in repositories: unrelated when they are in different ones, unknown when one's isn't known. */
function inRepos(a: Entity, b: Entity): Relation | undefined {
  const repos = repoRelation(a.repo, b.repo);
  if (repos === "unrelated") return "unrelated";
  if (repos === "unknown") return "unknown";
  return undefined;
}

/** Host `a` holds host `b`: `b` is `a` followed by a separator (`web` → `web.example.com`, `gamma` → `gamma-cluster`). */
function hostHolds(a: string, b: string): boolean {
  if (b.length <= a.length || !b.startsWith(a)) return false;
  const sep = b[a.length];
  return (sep === "." && a.length >= 3) || ((sep === "-" || sep === "_") && a.length >= 4);
}

function pathHolds(a: string, b: string): boolean {
  return a === "/" ? b !== "/" : b.startsWith(`${a}/`);
}

/** How `a` and `b` relate (both are made canonical first). */
export function relation(a: Entity, b: Entity, context: EntityContext = {}): Relation {
  return relate(canonical(a, context), canonical(b, context));
}

function relate(a: Entity, b: Entity): Relation {
  // Repositories hold what is in them.
  if (a.kind === "repo" || b.kind === "repo") {
    if (a.kind === "repo" && b.kind === "repo") return repoRelation(a.name, b.name);
    // A remote is the repository it points at.
    if (a.kind === "repo" && b.kind === "remote") return b.repo === undefined ? "unknown" : repoRelation(a.name, b.repo);
    if (b.kind === "repo" && a.kind === "remote") return flip(relate(b, a));
    if (a.kind === "repo") {
      if (!IN_REPO.has(b.kind)) return "unrelated";
      const repos = repoRelation(a.name, b.repo);
      return repos === "same" || repos === "contains" ? "contains" : repos;
    }
    return flip(relate(b, a));
  }
  if (a.kind === "remote" || b.kind === "remote") {
    if (a.kind === "remote" && b.kind === "remote") {
      if (a.name === b.name) return "same";
      return a.repo && b.repo ? (repoRelation(a.repo, b.repo) === "same" ? "same" : "unrelated") : "unknown";
    }
    if (a.kind === "remote") {
      if (b.kind !== "branch") return IN_REPO.has(b.kind) && a.repo && b.repo ? (a.repo === b.repo ? "contains" : "unrelated") : IN_REPO.has(b.kind) ? "unknown" : "unrelated";
      // The repositories decide when both are known; else the remote's name.
      if (a.repo && b.repo) return a.repo === b.repo ? "contains" : "unrelated";
      return b.remote === a.name ? "contains" : "unknown";
    }
    return flip(relate(b, a));
  }
  switch (a.kind) {
    case "branch":
      if (b.kind === "branch") {
        const repos = inRepos(a, b);
        if (repos === "unrelated") return repos;
        if (a.name !== b.name) return "unrelated";
        if (repos === "unknown") return a.remote === b.remote ? "same" : "unknown";
        return "same";
      }
      if (b.kind === "pr") return flip(relate(b, a));
      return "unrelated";
    case "pr":
      if (b.kind === "pr") {
        const repos = inRepos(a, b);
        if (repos) return repos;
        if (a.name && b.name) {
          if (a.name !== b.name) return "unrelated";
          // One number with contradicting branches: one of the facts is wrong.
          const clash = (x?: string, y?: string) => x !== undefined && y !== undefined && x !== y;
          return clash(a.head, b.head) || clash(a.base, b.base) ? "unknown" : "same";
        }
        if (!a.name && !b.name && a.head && b.head) {
          // A new pull request by its branches: one that leaves the base unsaid holds every base.
          if (a.head !== b.head) return "unrelated";
          if (a.base === b.base) return "same";
          if (a.base === undefined) return "contains";
          if (b.base === undefined) return "contained";
          return "unrelated";
        }
        return "unknown";
      }
      if (b.kind === "branch") {
        const repos = inRepos(a, b);
        if (repos) return repos;
        if (a.head === undefined) return "unknown";
        return a.head === b.name ? "contains" : "unrelated";
      }
      return "unrelated";
    case "tag":
    case "release":
      if (b.kind === "tag" || b.kind === "release") {
        const repos = inRepos(a, b);
        if (repos) return repos;
        return a.name === b.name ? "same" : "unrelated";
      }
      return "unrelated";
    case "path":
    case "worktree":
      if (b.kind !== "path" && b.kind !== "worktree") return "unrelated";
      if (!isAbsolute(a.name) || !isAbsolute(b.name)) return "unknown";
      if (a.name === b.name) return "same";
      if (pathHolds(a.name, b.name)) return "contains";
      if (pathHolds(b.name, a.name)) return "contained";
      return "unrelated";
    case "host":
      if (b.kind !== "host") return "unrelated";
      if (!a.name || !b.name) return "unknown";
      if (a.name === b.name) return "same";
      if (hostHolds(a.name, b.name)) return "contains";
      if (hostHolds(b.name, a.name)) return "contained";
      return "unrelated";
    case "simulator":
      if (b.kind !== "simulator") return "unrelated";
      if (a.udid && b.udid) return a.udid === b.udid ? "same" : "unrelated";
      if (!a.udid && !b.udid && a.name && b.name && !/^(all|booted|unavailable)$/i.test(a.name)) return a.name === b.name ? "same" : "unknown";
      if (a.udid && !b.udid && a.name !== a.udid && a.name === b.name) return "same";
      if (b.udid && !a.udid && b.name !== b.udid && b.name === a.name) return "same";
      return "unknown";
    case "process":
      if (b.kind !== "process") return "unrelated";
      return a.name === b.name ? "same" : /^\d+$/.test(a.name) && /^\d+$/.test(b.name) ? "unrelated" : "unknown";
    case "package":
      if (b.kind !== "package") return "unrelated";
      if (a.name !== b.name) return "unrelated";
      if (a.version === b.version) return "same";
      if (a.version === undefined) return "contains";
      if (b.version === undefined) return "contained";
      return "unrelated";
    case "image":
      if (b.kind !== "image") return "unrelated";
      if (a.name === b.name) return "same";
      // An image without its tag or digest holds every tag of it.
      if (b.name.startsWith(`${a.name}:`) || b.name.startsWith(`${a.name}@`)) return "contains";
      if (a.name.startsWith(`${b.name}:`) || a.name.startsWith(`${b.name}@`)) return "contained";
      return "unrelated";
    // Round 6 (W2): a namespace holds the resources scoped to it; the others are names of their own kind.
    case "namespace":
      if (b.kind === "namespace") return a.name === b.name ? "same" : "unrelated";
      if (b.kind === "resource") return b.scope === undefined ? "unknown" : b.scope === a.name ? "contains" : "unrelated";
      return "unrelated";
    case "context":
    case "cluster":
    case "project":
      return b.kind === a.kind ? (a.name.toLowerCase() === b.name.toLowerCase() ? "same" : "unrelated") : "unrelated";
    case "bucket":
      if (b.kind !== "bucket") return "unrelated";
      if (a.name === b.name) return "same";
      if (b.name.startsWith(`${a.name}/`)) return "contains";
      if (a.name.startsWith(`${b.name}/`)) return "contained";
      return "unrelated";
    case "container":
    case "resource":
      if (a.kind === "resource" && b.kind === "namespace") return flip(relate(b, a));
      if (b.kind !== a.kind) return "unrelated";
      if ((a.type ?? "") !== (b.type ?? "") && a.type && b.type) return "unrelated";
      if ((a.scope ?? "") !== (b.scope ?? "")) return a.scope && b.scope ? "unrelated" : "unknown";
      return a.name === b.name ? "same" : "unrelated";
  }
  return "unrelated";
}

/** `named` covers `acted`: they are one, or `named` holds it. */
export function covers(named: Entity, acted: Entity, context: EntityContext = {}): boolean {
  const how = relation(acted, named, context);
  return how === "same" || how === "contained";
}

/**
 * The model's target identifies one the command acts on: they are related
 * at all (one, or one holds the other), or the model named a new pull
 * request by the branch it goes into.
 */
export function identifies(claimed: Entity, acted: Entity, context: EntityContext = {}): boolean {
  const how = relation(claimed, acted, context);
  if (how === "same" || how === "contains" || how === "contained") return true;
  const pr = canonical(acted, context);
  const named = canonical(claimed, context);
  return pr.kind === "pr" && !pr.name && named.kind === "branch" && pr.base === named.name;
}

/**
 * The names that, written in a message, name this entity or one that holds
 * it: a branch as `x` or `origin/x`, a path or a folder it is in (as
 * written, from home, or the folder's own name), a host or a family of
 * hosts it is in, a package with or without its version, a simulator's
 * name or UDID, a repository's `owner/name` or `name`. Every name is one
 * whose entity `covers` this one (a test holds it to that).
 */
export function namesOf(entity: Entity, context: EntityContext = {}): string[] {
  const e = canonical(entity, context);
  const out = new Set<string>();
  const add = (name: string | undefined) => {
    if (name && name.trim()) out.add(name);
  };
  switch (e.kind) {
    case "branch":
      add(e.name);
      add(`${e.remote ?? "origin"}/${e.name}`);
      break;
    case "pr":
      if (e.name) {
        add(`#${e.name}`);
        if (e.repo) add(`${e.repo}#${e.name}`);
      }
      break;
    case "tag":
    case "release":
      add(e.name);
      break;
    case "path":
    case "worktree": {
      if (!isAbsolute(e.name)) {
        add(e.name);
        break;
      }
      const home = context.home ? normalizePath(context.home).replace(/\/+$/, "") : undefined;
      const cwd = context.cwd ? normalizePath(context.cwd).replace(/\/+$/, "") : undefined;
      let depth = 0;
      for (let path = e.name; path !== "/" && path !== home && path !== cwd; path = path.slice(0, path.lastIndexOf("/")) || "/", depth++) {
        add(path);
        if (home && path.startsWith(`${home}/`)) add(`~${path.slice(home.length)}`);
        if (cwd && path.startsWith(`${cwd}/`)) add(path.slice(cwd.length + 1));
        // Its own name or its folder's, when it says something ("screenshots", not "a"): a
        // name by itself is no folder of its own (it may be anywhere), so only those two.
        const leaf = path.slice(path.lastIndexOf("/") + 1);
        if (depth < 2 && leaf.length >= 3) add(leaf);
      }
      break;
    }
    case "host": {
      add(e.name);
      for (let i = 1; i < e.name.length; i++) {
        const prefix = e.name.slice(0, i);
        if (hostHolds(prefix, e.name)) add(prefix);
      }
      break;
    }
    case "package":
      add(e.name);
      if (e.version) add(`${e.name}@${e.version}`);
      break;
    case "simulator":
      add(e.name);
      add(e.udid);
      break;
    case "repo":
      add(e.name);
      // Its bare name, unless that is the current repository's and this is another.
      if (e.name.includes("/") && canonical({ kind: "repo", name: e.name.split("/")[1] }, context).name === e.name.split("/")[1]) add(e.name.split("/")[1]);
      else if (e.name === context.currentRepo) add(e.name.split("/")[1]);
      break;
    case "image": {
      add(e.name);
      add(e.name.replace(/[:@][^/:@]*$/, ""));
      break;
    }
    default:
      add(e.name);
  }
  return [...out];
}

/** What people call a kind, for the model's claims. */
const KIND_WORDS: Readonly<Record<string, EntityKind>> = {
  pr: "pr", "pull request": "pr", "pull-request": "pr", pullrequest: "pr", mr: "pr", "merge request": "pr",
  branch: "branch", ref: "branch",
  tag: "tag", release: "release", version: "tag",
  package: "package", crate: "package", npm: "package", gem: "package", module: "package",
  host: "host", server: "host", url: "host", domain: "host", machine: "host",
  repo: "repo", repository: "repo",
  remote: "remote",
  path: "path", file: "path", dir: "path", directory: "path", folder: "path",
  worktree: "worktree", checkout: "worktree",
  process: "process", pid: "process",
  simulator: "simulator", sim: "simulator", device: "simulator",
  container: "container",
  image: "image",
  resource: "resource", deployment: "resource", service: "resource", "helm release": "resource",
};

/** The entity a model's `{kind, value}` names, or undefined for a kind nobody uses. */
export function claimEntity(claim: { kind: string; value: string }): Entity | undefined {
  const kind = KIND_WORDS[claim.kind.trim().toLowerCase()];
  const value = claim.value.trim();
  if (!kind || !value) return undefined;
  if (kind === "pr") {
    // "the PR from x into main", "x → main", "#52".
    const number = /(?:#|\/pull\/|^)(\d+)\b/.exec(value)?.[1];
    if (number) return { kind, name: number };
    const arrow = /^(?:from\s+)?(\S+?)\s*(?:→|->|=>|\binto\b|\bto\b)\s*(\S+)$/i.exec(value);
    if (arrow) return { kind, name: "", head: arrow[1], base: arrow[2] };
    return { kind, name: "", head: value };
  }
  if (kind === "tag" && /^(release)$/i.test(claim.kind.trim())) return { kind: "release", name: value };
  return { kind, name: value };
}

/** A readable name for an entity, for dialogs and the judge. */
export function describeEntity(entity: Entity): string {
  switch (entity.kind) {
    case "pr":
      return entity.name ? `pull request #${entity.name}${entity.repo ? ` of ${entity.repo}` : ""}` : `a new pull request from ${entity.head ?? "this branch"} into ${entity.base ?? "?"}`;
    case "branch":
      return `branch ${entity.name}${entity.remote ? ` on ${entity.remote}` : ""}`;
    case "package":
      return `package ${entity.name}${entity.version ? `@${entity.version}` : ""}`;
    case "resource":
      return `${entity.type ?? "resource"} ${entity.name}${entity.scope ? ` in ${entity.scope}` : ""}`;
    default:
      return `${entity.kind} ${entity.name}`;
  }
}
