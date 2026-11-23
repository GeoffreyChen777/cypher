/**
 * Graded risk: how far a danger reaches and whether it can be taken back,
 * as data (design: docs/design/permission-control.md §7.3.8). Every action
 * the classifier calls a danger has a row: how reversible it is, who sees
 * it, and therefore who may approve it.
 *
 * - `judge`: reversible, and seen by the team at most (a branch pushed, a
 *   pull request opened or edited): when the code has checked that it acts
 *   only on what this task made (risk.ts `teamStep`), it is a local step the
 *   reviewing model decides with the task's context. No owner quote needed.
 * - `consent`: hard to take back, or reaching the public, production or
 *   another person's work: the explicit check (explicit.ts) runs as before.
 *
 * The always-ask floor (a secret leaving the machine, the permission system
 * itself, no reviewing model) is above every row and unchanged.
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
];

const BY_ACTION = new Map(GRADES.map((grade) => [grade.action, grade]));

export function gradeOf(action: string): Grade | undefined {
  return BY_ACTION.get(action);
}

/** Whether the reviewing model alone may approve the action, once its `own` fact holds: reversible, the team's at most. */
export function judgeDecides(action: string): boolean {
  const grade = BY_ACTION.get(action);
  return grade !== undefined && grade.approver === "judge" && grade.own !== undefined && grade.reversible !== "irreversible" && grade.audience !== "public";
}
