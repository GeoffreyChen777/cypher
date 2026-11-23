/**
 * Graded risk: how far a danger reaches and whether it can be taken back,
 * as data (design: docs/design/permission-control.md §7.3.8). Every action
 * the classifier calls a danger has a row: how reversible it is, who sees
 * it, and therefore who may approve it.
 *
 * - `judge`: reversible, and seen by the team at most (a branch pushed, a
 *   pull request opened or edited): when the code has checked that it acts
 *   only on what this task made (risk.ts `teamStep`), it is a local step the
 *   reviewing model decides with the task's context.
 * - `consent`: hard to take back, or reaching the public, production or
 *   another person's work: a danger.
 *
 * Since round 3 (2026-11-23) a grade is evidence for the reviewing model,
 * never a gate: every danger goes to the judge, whose verdict decides, and
 * its prompt carries the grade of what the line does (`gradesOf`). Only the
 * floor (floor.ts) still asks the owner by itself.
 *
 * A row's `own` names the code-checked fact that makes the action this
 * task's: never the model's word. A row without it is consent-gated
 * whatever it acts on.
 */

export type Reversibility = "undoable" | "recoverable" | "irreversible";
export type Audience = "self" | "team" | "public";
export type Approver = "judge" | "consent";

export interface Grade {
  action: string;
  reversible: Reversibility;
  audience: Audience;
  approver: Approver;
  /** The fact that makes it this task's own, checked by risk.ts. */
  own?: "branch" | "pull-request";
  /** In words, for the dialog and the judge. */
  says: string;
}

export const GRADES: readonly Grade[] = [
  { action: "push-branch", reversible: "undoable", audience: "team", approver: "judge", own: "branch", says: "pushes a branch this task made (not the default branch, no force)" },
  { action: "pr-open", reversible: "undoable", audience: "team", approver: "judge", own: "branch", says: "opens a pull request (draft or ready) from a branch this task made into the default branch" },
  { action: "pr-update", reversible: "undoable", audience: "team", approver: "judge", own: "pull-request", says: "edits or marks ready a pull request this task opened, or the one of a branch it made" },
  { action: "delete-merged-branch", reversible: "recoverable", audience: "team", approver: "judge", own: "branch", says: "deletes a merged branch this task made (lifecycle.ts follow-ups)" },
  { action: "merge-default", reversible: "recoverable", audience: "team", approver: "consent", says: "merges into the default branch" },
  { action: "push-default", reversible: "recoverable", audience: "team", approver: "consent", says: "pushes to the default or a protected branch" },
  { action: "push-foreign", reversible: "recoverable", audience: "team", approver: "consent", says: "pushes a branch this task didn't make" },
  { action: "pr-comment", reversible: "irreversible", audience: "team", approver: "consent", says: "comments on or reviews a pull request (notifies people)" },
  { action: "tag", reversible: "irreversible", audience: "public", approver: "consent", says: "pushes a tag (starts a release)" },
  { action: "release", reversible: "irreversible", audience: "public", approver: "consent", says: "publishes a release or a package" },
  { action: "deploy", reversible: "recoverable", audience: "public", approver: "consent", says: "deploys" },
  { action: "force-push", reversible: "irreversible", audience: "team", approver: "consent", says: "force-pushes or deletes a remote branch" },
  { action: "delete-unmerged", reversible: "irreversible", audience: "self", approver: "consent", says: "deletes work that isn't merged or isn't this task's" },
  { action: "send-data", reversible: "irreversible", audience: "public", approver: "consent", says: "sends data off this machine" },
  // Installs (round 4): where the software lands decides its reach, not the word "install".
  { action: "install-user", reversible: "undoable", audience: "self", approver: "judge", says: "installs a developer tool into this user's own tool directory (~/go/bin, ~/.cargo/bin, ~/.local/bin, ~/.dotnet/tools, ~/.rustup); deleting it undoes it, and the work may need it" },
  { action: "install-system", reversible: "recoverable", audience: "self", approver: "consent", says: "installs or removes software for the whole machine (a system package manager, a global prefix)" },
];

/** What the classifier's kinds (packs.ts) say an install reaches: the kind's words, as data. */
export const USER_INSTALL = "installs a developer tool for this user";
export const SYSTEM_INSTALL = "installs software for the whole machine";

/** Grades read from what the classifier says a command does (risk.ts `kind`), for commands whose targets don't tell (round 4). */
export const KIND_GRADES: readonly { kind: string; grade: string }[] = [
  { kind: USER_INSTALL, grade: "install-user" },
  { kind: SYSTEM_INSTALL, grade: "install-system" },
];

/** The grades the classifier's kinds give (`KIND_GRADES`), each once. */
export function kindGrades(kinds: readonly (string | undefined)[]): Grade[] {
  const out: Grade[] = [];
  for (const kind of kinds) {
    const row = KIND_GRADES.find((item) => kind !== undefined && kind.startsWith(item.kind));
    const grade = row && BY_ACTION.get(row.grade);
    if (grade && !out.includes(grade)) out.push(grade);
  }
  return out;
}

const BY_ACTION = new Map(GRADES.map((grade) => [grade.action, grade]));

export function gradeOf(action: string): Grade | undefined {
  return BY_ACTION.get(action);
}

/** Whether the reviewing model alone may approve the action, once its `own` fact holds: reversible, the team's at most. */
export function judgeDecides(action: string): boolean {
  const grade = BY_ACTION.get(action);
  return grade !== undefined && grade.approver === "judge" && grade.own !== undefined && grade.reversible !== "irreversible" && grade.audience !== "public";
}

/**
 * Which grade a dangerous line's target has, by what the tool table says it does (explicit.ts `Target`:
 * its kind and action), as data. The first row that matches wins; a forcing or bypassing attribute
 * (explicit.ts `ATTRIBUTE_FLAGS`) adds `force-push`. Evidence for the judge's prompt (round 3).
 */
export const TARGET_GRADES: readonly { kinds: readonly string[]; actions: readonly string[]; grade: string; when?: "default-branch" }[] = [
  { kinds: ["pr"], actions: ["merge"], grade: "merge-default" },
  { kinds: ["pr"], actions: ["comment", "review"], grade: "pr-comment" },
  { kinds: ["pr"], actions: ["create"], grade: "pr-open" },
  { kinds: ["pr"], actions: ["edit", "ready", "reopen", "close"], grade: "pr-update" },
  { kinds: ["branch"], actions: ["push"], grade: "push-default", when: "default-branch" },
  { kinds: ["branch"], actions: ["push"], grade: "push-foreign" },
  { kinds: ["branch"], actions: ["delete"], grade: "force-push" },
  { kinds: ["tag"], actions: ["push", "create"], grade: "tag" },
  { kinds: ["tag", "release", "package"], actions: ["release", "publish", "unpublish"], grade: "release" },
  { kinds: ["host"], actions: ["send"], grade: "send-data" },
  { kinds: ["path", "worktree", "process", "simulator", "container", "image", "resource", "branch", "tag", "repo"], actions: ["delete", "erase", "stop"], grade: "delete-unmerged" },
];

const DEFAULT_BRANCH = /^(?:[\w.-]+\/)?(main|master|trunk|develop)$/;

/** The grades of a line's targets (`TARGET_GRADES`), each once, in the order found. */
export function gradesOf(targets: readonly { kind: string; action: string; value: string }[], attributes: readonly string[] = []): Grade[] {
  const out: Grade[] = [];
  const add = (action: string) => {
    const grade = BY_ACTION.get(action);
    if (grade && !out.includes(grade)) out.push(grade);
  };
  for (const target of targets) {
    const row = TARGET_GRADES.find((item) => item.kinds.includes(target.kind) && item.actions.includes(target.action) && (item.when !== "default-branch" || DEFAULT_BRANCH.test(target.value)));
    if (row) add(row.grade);
  }
  if (attributes.length) add("force-push");
  return out;
}

/** A grade in words, for the judge: "merges into the default branch (recoverable, seen by the team)". */
export function describeGrade(grade: Grade): string {
  const audience = grade.audience === "self" ? "seen by no one else" : grade.audience === "team" ? "seen by the team" : "public";
  return `${grade.says} (${grade.reversible}, ${audience})`;
}
