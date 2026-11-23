/**
 * Lifecycle closure: the standard, contained follow-ups an action implies,
 * as data. Merging a pull request implies deleting its head branch (here
 * and on the remote) and the worktree it was checked out in; what this task
 * created it may delete, stop or close again; what it started it may stop;
 * a branch it created and pushed it may delete from the remote again.
 *
 * An antecedent is either an action the owner consented to (explicit.ts:
 * "merge #52" covers `gh pr merge 52`, and so its follow-ups) or one the
 * ledger recorded (ledger.ts: this chat created the simulator, merged the
 * pull request, pushed the branch it made). `followUpOf` answers whether a
 * target is a follow-up of one of them; the target's relation to the
 * antecedent (`Relation`) is resolved once here, for every kind, with the
 * facts the caller has (a pull request's head branch, a worktree's branch).
 *
 * A follow-up is a fact for the reviewing model and for the explicit check,
 * never a bypass of the always-ask floor: what the permission system,
 * secrets leaving the machine or the installed app are about still asks.
 */

import type { LedgerEntity, LedgerKind } from "./ledger.ts";
import { entryCovers } from "./ledger.ts";

/** What can be done to an entity, as tools.ts names actions. */
export type LifeAction = "create" | "merge" | "push" | "start" | "delete" | "stop" | "close" | "erase";

/** How a follow-up's target relates to its antecedent's entity. */
export type Relation =
  /** The entity itself (or something inside it: a file in a folder it made). */
  | "itself"
  /** The same branch on a remote. */
  | "remote-copy"
  /** A pull request's head branch, local or on any remote. */
  | "head-branch"
  /** A worktree whose checkout is a pull request's head branch. */
  | "head-worktree";

export interface FollowUp {
  /** The antecedent action, and the kinds of entity it is done to ("*": any). */
  after: LifeAction;
  of: readonly LedgerKind[] | "*";
  /** The actions it implies on the related entities. */
  then: readonly LifeAction[];
  on: Relation;
  /** The antecedent must be one this task created (a push implies nothing about a branch the owner made). */
  created?: boolean;
  /** Why, for the reviewing model and the audit. */
  why: string;
}

/** The lifecycle table. */
export const LIFECYCLE: readonly FollowUp[] = [
  { after: "merge", of: ["pr"], then: ["delete"], on: "head-branch", why: "a merged pull request's head branch is deleted after the merge" },
  { after: "merge", of: ["pr"], then: ["delete"], on: "head-worktree", why: "the worktree of a merged pull request's head branch is removed after the merge" },
  { after: "create", of: "*", then: ["delete", "stop", "close", "erase"], on: "itself", why: "what this task created it may clean up" },
  { after: "start", of: ["process", "simulator", "container", "host"], then: ["stop"], on: "itself", why: "what this task started it may stop" },
  { after: "push", of: ["branch"], then: ["delete"], on: "remote-copy", created: true, why: "a branch this task created and pushed it may delete again" },
];

/**
 * Implied steps (round 4): what a requested action needs done in the same line as its ordinary means,
 * as data. A consent that covers the antecedent target of a line covers these steps of the same line
 * too (explicit.ts `impliedStep`), so the reviewing model is told they are part of the request instead
 * of reading them as beyond it. Only plain steps: a forcing or bypassing attribute still has to be named
 * (explicit.ts `ATTRIBUTE_FLAGS`), and another remote or repository than the antecedent's never is one.
 * - `checkedOut`: the step's branch must be the one checked out where the line runs: the branch the new
 *   tag or release marks (a tag made elsewhere says nothing about which branch it is on).
 */
export interface ImpliedStep {
  after: { kinds: readonly string[]; actions: readonly string[] };
  then: { kinds: readonly string[]; actions: readonly string[] };
  checkedOut?: boolean;
  why: string;
}

export const IMPLIED_STEPS: readonly ImpliedStep[] = [
  { after: { kinds: ["tag"], actions: ["push", "create"] }, then: { kinds: ["branch"], actions: ["push"] }, checkedOut: true, why: "pushing a tag publishes the commit it marks, so the branch it was made on is pushed with it" },
  { after: { kinds: ["tag", "release"], actions: ["release", "publish"] }, then: { kinds: ["tag"], actions: ["push", "create"] }, why: "a release is cut from its tag, which is made and pushed for it" },
  { after: { kinds: ["tag", "release"], actions: ["release", "publish"] }, then: { kinds: ["branch"], actions: ["push"] }, checkedOut: true, why: "a release is cut from the branch it marks, which is pushed for it" },
  { after: { kinds: ["pr"], actions: ["create"] }, then: { kinds: ["branch"], actions: ["push"] }, why: "opening a pull request needs its head branch pushed" },
  { after: { kinds: ["package"], actions: ["publish", "release"] }, then: { kinds: ["tag"], actions: ["push", "create"] }, why: "a package release is tagged with its version" },
];

/** An action done to an entity: an antecedent, or the target being checked. */
export interface Acted {
  action: LifeAction | string;
  entity: LedgerEntity;
}

/** What resolving relations needs. */
export interface LifecycleFacts {
  /** A pull request's head branch (its entity may already say). */
  prHead?(pr: LedgerEntity): string | undefined;
  /** The branch checked out in a worktree. */
  worktreeBranch?(path: string): string | undefined;
  /** Whether this task created the entity (the ledger). */
  created?(entity: LedgerEntity): boolean;
  /** Two entities are the same thing (entities.ts `relation`); `entryCovers` when not given. */
  same?(made: LedgerEntity, target: LedgerEntity): boolean;
}

/** Whether `target` stands in `relation` to `antecedent`. */
export function related(relation: Relation, antecedent: LedgerEntity, target: LedgerEntity, facts: LifecycleFacts = {}): boolean {
  const same = facts.same ?? entryCovers;
  switch (relation) {
    case "itself":
      return same(antecedent, target);
    case "remote-copy":
      return antecedent.kind === "branch" && target.kind === "branch" && target.remote !== undefined && target.name === antecedent.name && (antecedent.repo === undefined || target.repo === undefined || antecedent.repo === target.repo);
    case "head-branch": {
      if (antecedent.kind !== "pr" || target.kind !== "branch") return false;
      const head = antecedent.head ?? facts.prHead?.(antecedent);
      return head !== undefined && head === target.name;
    }
    case "head-worktree": {
      if (antecedent.kind !== "pr" || target.kind !== "worktree") return false;
      const head = antecedent.head ?? facts.prHead?.(antecedent);
      return head !== undefined && facts.worktreeBranch?.(target.name) === head;
    }
  }
}

/** The rule and antecedent that make `target` a follow-up, if any. */
export function followUpOf(target: Acted, antecedents: readonly Acted[], facts: LifecycleFacts = {}): { rule: FollowUp; antecedent: Acted } | undefined {
  for (const rule of LIFECYCLE) {
    if (!rule.then.includes(target.action as LifeAction)) continue;
    for (const antecedent of antecedents) {
      if (antecedent.action !== rule.after) continue;
      if (rule.of !== "*" && !rule.of.includes(antecedent.entity.kind)) continue;
      if (rule.created && facts.created?.(antecedent.entity) !== true) continue;
      if (related(rule.on, antecedent.entity, target.entity, facts)) return { rule, antecedent };
    }
  }
  return undefined;
}

/**
 * The antecedents a target could follow from, to ask a consent about
 * (explicit.ts): for each rule that implies the target's action, the
 * antecedent entities related to it (the pull requests whose head the
 * branch is, as `prsWithHead` knows them).
 */
export function antecedentsFor(target: Acted, facts: LifecycleFacts & { prsWithHead?(branch: string): readonly LedgerEntity[] }): { rule: FollowUp; antecedent: Acted }[] {
  const out: { rule: FollowUp; antecedent: Acted }[] = [];
  for (const rule of LIFECYCLE) {
    if (!rule.then.includes(target.action as LifeAction) || rule.created) continue;
    let candidates: LedgerEntity[] = [];
    if (rule.on === "head-branch" && target.entity.kind === "branch") candidates = [...(facts.prsWithHead?.(target.entity.name) ?? [])];
    else if (rule.on === "head-worktree" && target.entity.kind === "worktree") {
      const branch = facts.worktreeBranch?.(target.entity.name);
      candidates = branch ? [...(facts.prsWithHead?.(branch) ?? [])] : [];
    }
    for (const entity of candidates) {
      if (rule.of !== "*" && !rule.of.includes(entity.kind)) continue;
      const antecedent = { action: rule.after, entity };
      if (related(rule.on, entity, target.entity, facts)) out.push({ rule, antecedent });
    }
  }
  return out;
}
