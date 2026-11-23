/**
 * The always-ask floor, as one table (design: docs/design/permission-control.md §7.3.1, round 3).
 *
 * These rows are the only things that put a question to the owner without the reviewing model
 * deciding it. Everything else — every tier and grade, pushes, merges, releases, deletes, the
 * installed app, lines the reader can't follow, other hosts — goes to the judge, and its verdict
 * decides (core.ts). The classifier's other findings are evidence in the judge's prompt, never a gate.
 *
 * - `secret-off-machine`: a secret or credential leaving this machine (risk.ts `exfiltrates`,
 *   `pathRisk`: `@file` sends, uploads of secret files, expanded credential variables).
 * - `permission-system`: a change to the permission system itself (§3.6: its policy files, the
 *   settings that load it, its extension's code, its audit log and ledger, the session files a grant
 *   is restored from), by a command or a file tool.
 * - `owner-rule`: the owner's own setting says to ask (a command rule set to `ask` or `deny` that
 *   matches, or a kind of request set to "Ask me"). Not a classifier's call: the owner asked to be
 *   asked, and only they can change it.
 * - `no-judge`: no reviewing model could decide — none configured, review paused after failures,
 *   or an error or timeout after its retry (core.ts `JUDGE_ATTEMPTS`).
 *
 * `stage`: `before` rows are tested before the judge is asked (and the judge isn't asked at all);
 * `after` rows on the judge's outcome.
 */

export type FloorId = "secret-off-machine" | "permission-system" | "owner-rule" | "no-judge";

/** What a request offers the floor's tests. */
export interface FloorSubject {
  /** The classifier's findings for the request: a bash line's own and each command's (risk.ts `Assessment`). */
  assessments?: readonly { key?: string; kind?: string; floor?: true }[];
  /** A file tool's write into the permission system: the path (config.ts `isPermissionSystem`). */
  permissionSystemPath?: string;
  /** The owner's own rule or setting that asks, in words. */
  ownerRule?: string;
  /** How the review went (core.ts `Review.state`), and its reason. */
  review?: { state: string; reason: string };
}

export interface FloorRow {
  id: FloorId;
  stage: "before" | "after";
  /** What the owner is told, before the detail. */
  reason: string;
  /** The detail when the row applies, else undefined. */
  test(subject: FloorSubject): string | undefined;
}

/** The risk.ts finding a row reads: its key, set only with `floor: true`. */
function finding(subject: FloorSubject, key: string): string | undefined {
  const hit = subject.assessments?.find((assessment) => assessment.floor === true && assessment.key === key);
  return hit ? (hit.kind ?? key) : undefined;
}

export const FLOOR: readonly FloorRow[] = [
  {
    id: "secret-off-machine",
    stage: "before",
    reason: "a secret or credential would leave this machine",
    test: (subject) => finding(subject, "secret-off-machine"),
  },
  {
    id: "permission-system",
    stage: "before",
    reason: "it changes the permission system itself",
    test: (subject) => subject.permissionSystemPath ?? finding(subject, "permission-system"),
  },
  {
    id: "owner-rule",
    stage: "before",
    reason: "your own setting says to ask",
    test: (subject) => subject.ownerRule,
  },
  {
    id: "no-judge",
    stage: "after",
    reason: "no reviewing model could decide",
    test: (subject) => (subject.review && (subject.review.state === "unavailable" || subject.review.state === "paused") ? subject.review.reason : undefined),
  },
];

export interface FloorHit {
  id: FloorId;
  reason: string;
  detail: string;
}

/** The first floor row of `stage` that applies, with its detail; undefined when none does. */
export function floorOf(subject: FloorSubject, stage: FloorRow["stage"]): FloorHit | undefined {
  for (const row of FLOOR) {
    if (row.stage !== stage) continue;
    const detail = row.test(subject);
    if (detail !== undefined) return { id: row.id, reason: row.reason, detail };
  }
  return undefined;
}
