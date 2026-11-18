// Replays the permission questions that reached the owner (the audit log
// from 2026-11-11 to 2026-11-18, sanitized in cypher-permissions-replay/)
// through the classifier and the decision path, with the reviewing model
// stubbed by each case's verdict: what now runs on the model's approval
// (once, or remembered for the chat) and what still asks the user.
//
// The machine is faked: a home with the main checkout and the chats'
// worktrees, program lookup that finds every program as the system's (so the
// result does not depend on what this host has installed), and the process
// facts each `kill` case records.
//
// REPLAY_REPORT=1 prints a table of every case instead of failing on the first difference.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

import { Permissions } from "./cypher-permissions/core.ts";
import { UNKNOWN } from "./cypher-permissions/shell.ts";

const here = dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(readFileSync(join(here, "cypher-permissions-replay", "audit-2026-11.json"), "utf8"));
const report = process.env.REPLAY_REPORT === "1";

const scratch = realpathSync(mkdtempSync(join(tmpdir(), "cypher-replay-")));
after(() => rmSync(scratch, { recursive: true, force: true }));
const home = join(scratch, "home");
/** The cases' `/tmp`: scratch space, apart from the fake home. */
const tmp = join(scratch, "tmp");
mkdirSync(tmp, { recursive: true });
const agentDir = join(home, ".cypher", "pi-runtime", "agent");
mkdirSync(agentDir, { recursive: true });
mkdirSync(join(home, ".cypher-development"), { recursive: true });
mkdirSync(join(home, ".cypher", "orgs", "org_FIXTURE", "user_FIXTURE"), { recursive: true });

// `/tmp` first: the fake home may itself be under /tmp (Linux).
const expand = (text) => text.replace(/(^|[^\w.~-])\/tmp(?=\/|\b)/g, `$1${tmp}`).replaceAll("@HOME@", home);

function repo(path) {
  mkdirSync(path, { recursive: true });
  spawnSync("git", ["init", "-q", "-b", "main", path]);
  spawnSync("git", ["-C", path, "-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "init"]);
}
for (const cwd of new Set(fixture.cases.map((item) => item.cwd))) repo(expand(cwd));

/** Every program is found, as the system's; a path names itself. */
function findProgram(program, cwd) {
  if (!program || program === UNKNOWN) return undefined;
  if (program.includes("/")) {
    if (cwd === undefined && !isAbsolute(program)) return undefined;
    return isAbsolute(program) ? program : resolve(cwd, program);
  }
  return `/usr/bin/${program}`;
}

/** The replay's process facts: `chat` (carries this chat's marker), `roots` (runs in its folders), `foreign`, `app` (the installed app's). */
function processFacts(item) {
  const of = (target) => ("pids" in target ? target.pids.map((pid) => item.processes?.[pid] ?? "foreign") : [item.processes?.pgrep ?? "foreign"]);
  return {
    owns: (target) => of(target).every((kind) => kind === "chat"),
    facts: (target) => {
      const kinds = of(target);
      return {
        owned: kinds.every((kind) => kind === "chat"),
        inRoots: kinds.every((kind) => kind === "chat" || kind === "roots"),
        installedApp: kinds.some((kind) => kind === "app"),
      };
    },
  };
}

function lastAudit() {
  const lines = readFileSync(join(agentDir, "permission-audit.jsonl"), "utf8").trim().split("\n");
  return JSON.parse(lines.at(-1));
}

async function replay(item) {
  const cwd = expand(item.cwd);
  const permissions = new Permissions({ agentDir, chatId: "chat-replay", cwd, home, landstrip: { available: true, binary: "/nonexistent/landstrip" }, scratch: [tmp] });
  permissions.appendEntry = () => {};
  permissions.resolveIn = findProgram;
  const processes = processFacts(item);
  permissions.ownsProcesses = processes.owns;
  permissions.processFacts = processes.facts;
  const judged = [];
  const dialogs = [];
  const ctx = {
    cwd,
    hasUI: true,
    ui: {
      async select(title, options) {
        dialogs.push({ title, options });
        return "Deny";
      },
      notify() {},
    },
    branch: () => [{ type: "message", message: { role: "user", content: "(the owner's request; not in the audit)" } }],
    complete: async (_system, prompt) => {
      judged.push(prompt);
      return item.verdict ? JSON.stringify(item.verdict) : '{"decision":"ask-user","reason":"no verdict for this case"}';
    },
  };
  const before = permissions.grants.all().length;
  let result;
  if (item.capability === "escalate") {
    result = await permissions.requestEscalation(expand(item.command), { label: "replay" }, ctx);
  } else if (item.tool === "request_permission") {
    result = await permissions.requestPermission({ capability: item.capability, paths: item.paths.map(expand) }, ctx);
  } else {
    result = await permissions.authorize(item.paths.map((path) => ({ capability: item.capability, path: expand(path) })), { tool: item.tool }, ctx);
  }
  const audit = lastAudit();
  const remembered = permissions.grants.all().length > before;
  let outcome;
  if (dialogs.length) outcome = "ask";
  else if (!result.ok) outcome = "denied";
  else if (audit.decidedBy === "judge") outcome = remembered ? "approve-remember" : "approve-once";
  else outcome = `auto-${audit.decidedBy}`;
  const plain = (text) => text?.replaceAll(home, "@HOME@").replaceAll(tmp, "/tmp");
  return { outcome, tier: audit.tier, kind: plain(audit.kind), judged: judged.length > 0, audit, onceOnly: plain(audit.onceOnly), missingVerdict: judged.length > 0 && !item.verdict };
}

test("replay: the owner's questions from the audit, with the reviewing model's verdicts stubbed", async () => {
  const rows = [];
  const failures = [];
  for (const item of fixture.cases) {
    const got = await replay(item);
    rows.push({ item, got });
    const want = item.expect;
    if (got.missingVerdict) failures.push(`#${item.id}: the model was asked but the case has no verdict`);
    if (!want) {
      failures.push(`#${item.id}: no expectation (got ${got.outcome}, ${got.tier}: ${got.kind ?? ""})`);
      continue;
    }
    if (got.outcome !== want.outcome) failures.push(`#${item.id} [${item.group}]: ${got.outcome}, expected ${want.outcome} (${got.tier}: ${got.kind ?? ""}; ${got.onceOnly ?? ""})`);
    if (want.tier && got.tier !== want.tier) failures.push(`#${item.id} [${item.group}]: tier ${got.tier}, expected ${want.tier} (${got.kind ?? ""})`);
    if (want.kind && !new RegExp(want.kind).test(got.kind ?? "")) failures.push(`#${item.id} [${item.group}]: kind ${got.kind}, expected /${want.kind}/`);
    if (got.judged && got.audit.judgeVerdict === undefined) failures.push(`#${item.id}: the audit lacks the model's verdict`);
  }
  if (report) {
    for (const { item, got } of rows) {
      console.log([`#${item.id}`, item.group ?? "-", item.recorded.tier ?? "-", got.tier, got.outcome, got.judged ? "judged" : "-", (got.kind ?? "").slice(0, 70), (got.onceOnly ?? "").slice(0, 60)].join("\t"));
    }
    const summary = new Map();
    for (const { item, got } of rows) {
      const key = `${item.group ?? "-"}\t${got.outcome}`;
      summary.set(key, (summary.get(key) ?? 0) + 1);
    }
    console.log([...summary].sort().map(([key, n]) => `${String(n).padStart(4)}  ${key}`).join("\n"));
    writeFileSync(join(tmpdir(), "cypher-replay-report.json"), JSON.stringify(rows.map(({ item, got }) => ({ id: item.id, group: item.group, outcome: got.outcome, tier: got.tier, kind: got.kind, judged: got.judged, onceOnly: got.onceOnly })), null, 1));
    return;
  }
  assert.deepEqual(failures, []);
  assert.equal(fixture.cases.length, 174);
});
