// Graded risk (grades.ts, risk.ts `teamStep`) and the judge's concern list
// (judge.ts `CONCERNS`, core.ts `asksNeedConcern`). Property tests draw from a
// seeded PRNG: PERMISSIONS_SEED reseeds them, and a failure names the seed.
import assert from "node:assert/strict";
import { test } from "node:test";

import { GRADES, gradeOf, judgeDecides } from "./cypher-permissions/grades.ts";
import { assessCommand } from "./cypher-permissions/risk.ts";
import { asksOnly, viewOf } from "./cypher-permissions/packs.ts";
import { CONCERNS, judgePrompt, JUDGE_SYSTEM_PROMPT, parseVerdict } from "./cypher-permissions/judge.ts";
import { asksNeedConcern } from "./cypher-permissions/core.ts";
import { UNKNOWN } from "./cypher-permissions/shell.ts";

const SEED = Number(process.env.PERMISSIONS_SEED ?? 20261123);

function prng(seed) {
  let state = seed >>> 0;
  const next = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const int = (n) => Math.floor(next() * n);
  const pick = (list) => list[int(list.length)];
  const word = (length = 3 + int(6)) => Array.from({ length }, () => pick("abcdefghijklmnopqrstuvwxyz0123456789")).join("");
  return { next, int, pick, word };
}

const REPO = "/work/repo";

/**
 * A fake machine: one repository at /work/repo with `main` checked out unless `current` says
 * otherwise; `own` the ledger's branches, `workspace` the chat's own workspace branch.
 */
function facts({ current = "feature", branches = [], own = [], workspace = [], prs = [], tools = [] } = {}) {
  const local = new Set(["main", current, ...branches, ...own, ...workspace]);
  return {
    classify: (path, cwd) => {
      const abs = path.startsWith("/") ? path : cwd === undefined ? undefined : `${cwd}/${path}`;
      if (abs === undefined) return undefined;
      return { path: abs, zone: abs.startsWith("/work") ? "workspace" : "external" };
    },
    disposable: (path) => path.startsWith("/work"),
    protectedTargets: ["prod", "production", "release/*"],
    inspect: [],
    ownsProcesses: () => undefined,
    git: (dir) => (dir?.startsWith(REPO) ? { current, defaults: ["main", "master"], isBranch: (name) => local.has(name) } : undefined),
    inWorkspace: (dir) => dir.startsWith("/work"),
    workspaceBranches: () => workspace,
    created: { has: () => false, worktree: () => false, branch: (name) => own.includes(name), named: () => false, pullRequest: (n) => prs.includes(n) },
    kubeContext: () => undefined,
    programIsLocal: () => false,
    projectTool: (name, cwd) => tools.some(([tool, dir]) => tool === name && (cwd ?? REPO).startsWith(dir)),
  };
}

const assess = (words, machine, cwd = REPO) => assessCommand({ words, cwd, reads: [], writes: [] }, machine);

test("grades: every row is data with a reversibility, an audience and an approver; only reversible, non-public rows with an own-fact are the judge's", () => {
  const actions = new Set();
  for (const grade of GRADES) {
    assert.ok(!actions.has(grade.action), `one row per action: ${grade.action}`);
    actions.add(grade.action);
    assert.ok(["undoable", "recoverable", "irreversible"].includes(grade.reversible), grade.action);
    assert.ok(["self", "team", "public"].includes(grade.audience), grade.action);
    assert.equal(judgeDecides(grade.action), grade.approver === "judge" && grade.own !== undefined && grade.reversible !== "irreversible" && grade.audience !== "public", grade.action);
    assert.equal(gradeOf(grade.action), grade);
  }
  // The consent-gated ones the plan names stay so.
  for (const action of ["merge-default", "push-default", "push-foreign", "tag", "release", "deploy", "force-push", "delete-unmerged", "send-data", "pr-comment"]) {
    assert.equal(judgeDecides(action), false, action);
  }
  for (const action of ["push-branch", "pr-open", "pr-update"]) assert.equal(judgeDecides(action), true, action);
  assert.equal(judgeDecides("no such action"), false);
});

test("team steps: pushing this task's own branch and opening a pull request from it are local, for the judge", () => {
  const mine = facts({ current: "cypher/fix-x", own: ["cypher/fix-x"] });
  for (const words of [
    ["git", "push", "-u", "origin", "cypher/fix-x"],
    ["git", "push", "origin", "HEAD"],
    ["git", "push"],
    ["git", "-C", REPO, "push", "origin", "cypher/fix-x:cypher/fix-x"],
    ["gh", "pr", "create", "--base", "main", "--head", "cypher/fix-x", "--title", "Fix x", "--body-file", "-"],
    ["gh", "pr", "create", "--draft", "--fill"],
    ["gh", "pr", "ready"],
    ["gh", "pr", "edit", "--title", "Better title"],
    ["git", "--no-pager", "push", "-u", "origin", "cypher/fix-x"],
    ["glab", "mr", "create", "--source-branch", "cypher/fix-x", "--target-branch", "main", "--title", "Fix x", "--draft"],
    ["glab", "mr", "update", "--title", "Better"],
  ]) {
    const got = assess(words, mine);
    assert.equal(got.tier, "local", words.join(" "));
    assert.equal(got.once, true, `${words.join(" ")}: never remembered`);
    assert.match(got.kind, /reversible, seen by the team/);
  }
  // The chat's own workspace branch counts as its own; a pull request it opened, by number.
  const workspace = facts({ current: "cypher/ws", workspace: ["cypher/ws"], prs: ["77"] });
  assert.equal(assess(["git", "push", "-u", "origin", "cypher/ws"], workspace).tier, "local");
  assert.equal(assess(["gh", "pr", "edit", "77", "--body", "x"], workspace).tier, "local");
  assert.equal(assess(["gh", "pr", "ready", "#77"], workspace).tier, "local");
});

test("team steps: the default branch, another remote or repository, force, tags, merging, a PR from someone else's branch stay dangers", () => {
  // (A plain push of a non-default branch that is no one's in particular was local before grades.ts and stays so: risk.ts gitPushDanger.)
  const mine = facts({ current: "cypher/fix-x", own: ["cypher/fix-x"], branches: ["someone/else"], prs: ["77"] });
  for (const words of [
    ["git", "push", "origin", "main"],
    ["git", "push", "origin", "cypher/fix-x:main"],
    ["git", "push", "--force", "origin", "cypher/fix-x"],
    ["git", "push", "-f", "origin", "cypher/fix-x"],
    ["git", "push", "origin", "+cypher/fix-x"],
    ["git", "push", "--tags", "origin", "cypher/fix-x"],
    ["git", "push", "origin", "--delete", "cypher/fix-x"],
    ["git", "push", "origin", UNKNOWN],
    ["git", "-c", "core.sshCommand=evil", "push", "origin", "cypher/fix-x"],
    ["gh", "pr", "create", "--base", "release-2", "--head", "cypher/fix-x"],
    ["gh", "pr", "create", "--head", "someone/else"],
    ["gh", "pr", "create", "--repo", "other/repo", "--head", "cypher/fix-x"],
    ["gh", "pr", "create", "--head", "fork:cypher/fix-x"],
    ["gh", "pr", "merge", "77", "--squash"],
    ["gh", "pr", "comment", "77", "--body", "hi"],
    ["gh", "pr", "edit", "78", "--title", "x"],
    ["gh", "pr", "edit", "77", "--base", "other"],
    ["gh", "pr", "close", "78"],
    ["glab", "mr", "create", "--source-branch", "someone/else", "--target-branch", "main"],
    ["glab", "mr", "create", "--source-branch", "cypher/fix-x", "--target-branch", "develop"],
    ["glab", "mr", "merge", "77"],
    ["glab", "mr", "create", "--head", "cypher/fix-x"],
    ["git", "--git-dir=/elsewhere/.git", "push", "origin", "cypher/fix-x"],
  ]) {
    assert.equal(assess(words, mine).tier, "danger", words.join(" "));
  }
  // A URL remote is not a team step (whatever else decides it).
  assert.ok(!(assess(["git", "push", "https://github.com/x/y", "cypher/fix-x"], mine).kind ?? "").includes("seen by the team"));
  // On the default branch, nothing is the task's own.
  const onMain = facts({ current: "main", own: ["main"] });
  assert.equal(assess(["git", "push"], onMain).tier, "danger");
  assert.equal(assess(["gh", "pr", "create", "--fill"], onMain).tier, "danger");
});

test(`team steps, property (seed ${SEED}): local only when every branch is the task's own, not a default, with only plain options`, () => {
  const rand = prng(SEED);
  const PLAIN = ["-u", "--set-upstream", "-q", "--verbose", "--no-verify"];
  const UNSAFE = ["--force", "-f", "--force-with-lease", "--tags", "--follow-tags", "--all", "--mirror", "--delete", "-d", "--prune", "--atomic", "-o", "--push-option=x", "--receive-pack=x"];
  for (let round = 0; round < 400; round++) {
    const ownName = `cypher/${rand.word()}`;
    const foreign = `${rand.word()}/${rand.word()}`;
    const viaLedger = rand.next() < 0.5;
    const machine = facts({ current: ownName, own: viaLedger ? [ownName] : [], workspace: viaLedger ? [] : [ownName], branches: [foreign] });
    const branches = Array.from({ length: 1 + rand.int(2) }, () => rand.pick([ownName, ownName, foreign, "main", "master", "release/1", "HEAD", `${ownName}:main`, `+${ownName}`, `refs/tags/${rand.word()}`]));
    const options = Array.from({ length: rand.int(3) }, () => (rand.next() < 0.7 ? rand.pick(PLAIN) : rand.pick(UNSAFE)));
    const remote = rand.pick(["origin", "origin", "upstream", "git@github.com:x/y.git", "../elsewhere"]);
    const words = ["git", "push", ...options, remote, ...branches];
    const got = assess(words, machine);
    const ownOnly = branches.every((branch) => branch === ownName || branch === "HEAD");
    const plain = options.every((option) => PLAIN.includes(option));
    const named = /^[\w.-]+$/.test(remote);
    const expectTeam = ownOnly && plain && named;
    if (expectTeam) assert.equal(got.tier, "local", `seed ${SEED} round ${round}: ${words.join(" ")}`);
    // Never a team step when anything but the task's own branches would be published.
    if (!expectTeam) assert.ok(!(got.kind ?? "").includes("seen by the team"), `seed ${SEED} round ${round}: ${words.join(" ")} → ${got.tier} ${got.kind}`);
    // Nothing that touches a default, a protected branch, a tag or a force is local through the grade.
    if (branches.some((branch) => /^(main|master|release\/1)$|:main$|^\+|^refs\/tags\//.test(branch))) assert.ok(!(got.kind ?? "").includes("seen by the team"), `seed ${SEED} round ${round}: ${words.join(" ")}`);
  }
});

test(`team steps, property (seed ${SEED}): gh pr create is local only from the task's own branch into a default branch, in this repository`, () => {
  const rand = prng(SEED + 1);
  for (let round = 0; round < 300; round++) {
    const ownName = `cypher/${rand.word()}`;
    const machine = facts({ current: rand.next() < 0.5 ? ownName : "main", own: [ownName], branches: ["other/b"] });
    const head = rand.pick([undefined, ownName, "other/b", "main", `fork:${ownName}`]);
    const base = rand.pick([undefined, "main", "master", "develop", "release/2"]);
    const repo = rand.next() < 0.2 ? ["--repo", "o/r"] : [];
    const extra = rand.pick([[], ["--draft"], ["--title", "t", "--body", "b"], ["--fill"], ["--web"]]);
    const words = ["gh", "pr", "create", ...(head ? ["--head", head] : []), ...(base ? ["--base", base] : []), ...repo, ...extra];
    const got = assess(words, machine);
    const effectiveHead = head ?? (machine.git(REPO).current);
    const expected = effectiveHead === ownName && (base === undefined || base === "main" || base === "master") && repo.length === 0 && !extra.includes("--web");
    assert.equal(got.tier, expected ? "local" : "danger", `seed ${SEED} round ${round}: ${words.join(" ")} on ${machine.git(REPO).current}`);
  }
});

test("queries: help/version and the system tools' read-only forms are not dangers; their changing forms are", () => {
  const machine = facts();
  for (const words of [["pmset", "-g", "therm"], ["pmset", "-g"], ["networksetup", "-listallnetworkservices"], ["csrutil", "status"], ["spctl", "--status"], ["pmset", "--help"], ["systemsetup", "-getcomputersleep"]]) {
    assert.notEqual(assess(words, machine).tier, "danger", words.join(" "));
  }
  for (const words of [["pmset", "-a", "sleep", "0"], ["pmset", "sleepnow"], ["networksetup", "-setairportpower", "en0", "off"], ["csrutil", "disable"], ["spctl", "--master-disable"], ["nvram", "boot-args=-v"], ["pmset", "repeat", "cancel"]]) {
    assert.equal(assess(words, machine).tier, "danger", words.join(" "));
  }
  assert.equal(asksOnly(viewOf(["pi", "--help"])), true);
  assert.equal(asksOnly(viewOf(["gh", "pr", "create", "--help"])), true);
  assert.equal(asksOnly(viewOf(["gh", "pr", "create", "--title", "x", "--help"])), false, "an operand before --help: not a pure question");
  assert.equal(asksOnly(viewOf(["sudo", "-v"])), false, "-v is not a help flag (sudo -v refreshes credentials)");
  assert.equal(asksOnly(viewOf(["tool", UNKNOWN, "--help"])), false);
});

test("queries: npx of the project's pinned tool resolves from where the command runs", () => {
  const machine = facts({ tools: [["wrangler", `${REPO}/apps/edge`]] });
  assert.notEqual(assess(["npx", "wrangler", "dev"], machine, `${REPO}/apps/edge`).tier, "danger");
  assert.equal(assess(["npx", "wrangler", "dev"], machine, REPO).tier, "danger", "not installed where it runs");
  assert.equal(assess(["npx", "cowsay"], machine, `${REPO}/apps/edge`).tier, "danger");
});

test("judge: a local ask must name a concern from the closed list; dangers, network, tools and sensitive files keep every ask", () => {
  assert.deepEqual([...CONCERNS], ["installed-app", "other-task", "secret", "beyond-request", "irreversible"]);
  for (const concern of CONCERNS) assert.ok(JUDGE_SYSTEM_PROMPT.includes(`"${concern}"`), concern);
  assert.equal(asksNeedConcern({ tier: "local", capability: "escalate", zone: "outside the sandbox" }), true);
  assert.equal(asksNeedConcern({ tier: "local", capability: "read", zone: "external" }), true);
  assert.equal(asksNeedConcern({ tier: "local", capability: "read", zone: "sensitive" }), false);
  assert.equal(asksNeedConcern({ tier: "danger", capability: "escalate", zone: "outside the sandbox" }), false);
  assert.equal(asksNeedConcern({ tier: "local", capability: "network", zone: "external" }), false);
  assert.equal(asksNeedConcern({ tier: "local", capability: "tool", zone: "external" }), false);
  assert.equal(asksNeedConcern({ tier: "local", capability: "escalate", zone: "outside the sandbox", knownConcern: "installed-app" }), false);
  assert.equal(asksNeedConcern({ tier: "local", capability: "escalate", zone: "outside the sandbox", knownConcern: "secret" }), false);
  assert.equal(parseVerdict('{"decision":"ask-user","reason":"x","concern":"none"}').concern, undefined);
});

test("judge: the owner's words, answers and plan card reach it for local requests too, never pushed out by agent text", () => {
  const ownerWords = {
    agentWritten: true,
    messages: [
      { source: "card", text: "Plan: Fix the scroll bug. Steps: 1. fix 2. open a PR" },
      { source: "answer", text: "Reuse the runtime", question: "Which runtime?\nOptions: Reuse the runtime | Build a new one" },
      { source: "typed", text: "pr 给main" },
    ],
  };
  const notices = Array.from({ length: 10 }, (_, i) => `(A notice from your workspace, not a message from your owner) #${i}`);
  const prompt = judgePrompt({ capability: "escalate", paths: [], zone: "outside the sandbox", command: "git push -u origin cypher/x", level: "workspace", workspaceRoots: ["/work/repo"], tier: "local", userMessages: notices, recentToolCalls: [], ownerWords });
  assert.match(prompt, /## Your owner's own words \(trusted; oldest first: what they typed, their answers/);
  assert.match(prompt, /\[card\]/);
  assert.match(prompt, /\[answer\] The agent asked:/);
  assert.match(prompt, /pr 给main/);
  const installed = judgePrompt({ capability: "escalate", paths: [], zone: "outside the sandbox", command: "sqlite3 -readonly x", level: "workspace", workspaceRoots: [], tier: "local", userMessages: [], recentToolCalls: [], ownerWords, installedRead: true });
  assert.match(installed, /installed app data: yes/);
});
