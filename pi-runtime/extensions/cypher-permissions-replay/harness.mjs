// The replay's machine: a fake home with the main checkout and the chats'
// worktrees, program lookup that finds every program as the system's (so the
// result does not depend on what this host has installed), the process facts
// each `kill` case records, and one call that puts a fixture case (or one of
// its probes) through the extension's real decision path.
//
// Shared by the stubbed replay test (cypher-permissions-replay.test.mjs) and
// the live runner (live.mjs), which differ only in how the reviewing model
// answers (`complete`) and in the conversation the case was asked in.
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { Permissions } from "../cypher-permissions/core.ts";
import { UNKNOWN } from "../cypher-permissions/shell.ts";
import { setSystemDirectory } from "../cypher-permissions/commands.ts";

// The fake machine's read-only directories (a `DEVELOPER_DIR=/Applications/Xcode.app/…` names the installed toolchain), on any host.
setSystemDirectory((path) => /^\/(Applications|Library|System|usr|opt\/local)\//.test(path));

const here = dirname(fileURLToPath(import.meta.url));

export const fixture = JSON.parse(readFileSync(join(here, "audit-2026-11.json"), "utf8"));

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

/** The stubbed reviewing model: the case's (or probe's) recorded, rule-made or assumed verdict. */
export function stubbedModel(verdict) {
  // A recorded or assumed ask carries the concern a judge would name for what the line does (judge.ts
  // `CONCERN_TABLE`, set per case in the fixture since round 3); an ask without one runs once.
  return async () => JSON.stringify(verdict ?? { decision: "ask-user", reason: "no verdict for this case" });
}

/**
 * Build the fake machine for `cases` (the fixture's by default). `judgeTimeoutMs`
 * sets the reviewing model's timeout in the fake agent directory's
 * permissions.json (the live runner waits longer than the extension's
 * default, and reports the latency).
 */
export function createMachine({ judgeTimeoutMs, cases = fixture.cases } = {}) {
  const scratch = realpathSync(mkdtempSync(join(tmpdir(), "cypher-replay-")));
  const home = join(scratch, "home");
  /** The cases' `/tmp`: scratch space, apart from the fake home. */
  const tmp = join(scratch, "tmp");
  mkdirSync(tmp, { recursive: true });
  const agentDir = join(home, ".cypher", "pi-runtime", "agent");
  mkdirSync(agentDir, { recursive: true });
  if (judgeTimeoutMs) writeFileSync(join(agentDir, "permissions.json"), JSON.stringify({ judge: { timeoutMs: judgeTimeoutMs } }));
  mkdirSync(join(home, ".cypher-development"), { recursive: true });
  mkdirSync(join(home, ".cypher", "orgs", "org_FIXTURE", "user_FIXTURE"), { recursive: true });

  // `/tmp` first: the fake home may itself be under /tmp (Linux).
  const expand = (text) => text.replace(/(^|[^\w.~-])\/tmp(?=\/|\b)/g, `$1${tmp}`).replaceAll("@HOME@", home);
  const plain = (text) => text?.replaceAll(home, "@HOME@").replaceAll(tmp, "/tmp");

  function repo(path) {
    mkdirSync(path, { recursive: true });
    spawnSync("git", ["init", "-q", "-b", "main", path]);
    spawnSync("git", ["-C", path, "-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "init"]);
  }
  for (const cwd of new Set(cases.map((item) => item.cwd))) repo(expand(cwd));
  // A chat's worktree is on its own branch (`checkout`): work it may delete when it is done.
  for (const item of cases) {
    if (item.checkout) spawnSync("git", ["-C", expand(item.cwd), "switch", "-q", "-c", item.checkout]);
  }

  /**
   * Put one case (or one of its probes) through the decision path.
   * `complete` answers for the reviewing model (default: the stubbed
   * verdict); `branch` replaces the conversation the case carries (the live
   * runner passes the real one, sanitized); `proposal` is the agent's label
   * and reason for an escalation.
   */
  let replays = 0;
  async function replay(item, probe, { complete, branch: conversation, proposal } = {}) {
    const verdict = probe ? probe.verdict : item.verdict;
    const command = probe?.command ?? item.command;
    const cwd = expand(item.cwd);
    // A chat of its own: what an earlier case's commands made (its on-disk ledger, created.ts) is never this one's.
    const chatId = `chat-replay-${++replays}`;
    const permissions = new Permissions({ agentDir, chatId, cwd, home, landstrip: { available: true, binary: "/nonexistent/landstrip" }, scratch: [tmp] });
    permissions.appendEntry = () => {};
    // The audit record this call writes, kept here so concurrent replays do not read each other's.
    const records = [];
    permissions.audit.write = (record) => records.push(record);
    permissions.resolveIn = findProgram;
    // What the chat ran earlier in its turn: what it made is its own (created.ts).
    for (const earlier of item.ran ?? []) permissions.noteCreated(expand(earlier), cwd);
    const processes = processFacts(item);
    permissions.ownsProcesses = processes.owns;
    permissions.processFacts = processes.facts;
    const owner = probe && "owner" in probe ? probe.owner : item.owner;
    permissions.ownerWords = owner ? { messages: owner.messages, agentWritten: owner.agentWritten === true } : undefined;
    permissions.releaseFacts = (_dir, commit, version) => {
      const facts = item.release?.[commit];
      return facts ? { onDefault: facts.onDefault, hasVersion: facts.versions.includes(version) } : {};
    };
    permissions.originRepo = () => undefined;
    const user = (item.user ?? ["(the owner's request; not in the audit)"]).map((content) => ({ type: "message", message: { role: "user", content } }));
    const base = conversation ? JSON.parse(expand(JSON.stringify(conversation))) : user;
    const extra = probe?.branch ?? (conversation ? [] : (item.branch ?? []));
    const branch = [...base, ...JSON.parse(expand(JSON.stringify(extra)))];
    const judged = [];
    const answers = [];
    const dialogs = [];
    const model = complete ?? stubbedModel(verdict);
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
      branch: () => branch,
      complete: async (system, prompt, signal) => {
        judged.push(prompt);
        const answer = await model(system, prompt, signal);
        answers.push(answer);
        return answer;
      },
    };
    const before = permissions.grants.all().filter((grant) => grant.scope !== "once").length;
    let result;
    if (item.capability === "escalate") {
      result = await permissions.requestEscalation(expand(command), proposal ?? { label: "replay" }, ctx);
    } else if (item.tool === "request_permission") {
      result = await permissions.requestPermission({ capability: item.capability, paths: item.paths.map(expand) }, ctx);
    } else {
      result = await permissions.authorize(item.paths.map((path) => ({ capability: item.capability, path: expand(path) })), { tool: item.tool }, ctx);
    }
    const audit = records.at(-1) ?? {};
    // A `request_permission` approved once leaves a one-shot grant for the next file tool call: not remembered.
    const remembered = permissions.grants.all().filter((grant) => grant.scope !== "once").length > before;
    let outcome;
    if (dialogs.length) outcome = "ask";
    else if (!result.ok) outcome = "denied";
    else if (audit.decidedBy === "judge") outcome = remembered ? "approve-remember" : "approve-once";
    // Nothing was decided at all: the defaults allow it (a read of an attachment).
    else outcome = audit.decidedBy ? `auto-${audit.decidedBy}` : "allowed";
    return {
      outcome,
      tier: audit.tier,
      kind: plain(audit.kind),
      judged: judged.length > 0,
      audit,
      onceOnly: plain(audit.onceOnly),
      explicit: plain(audit.explicit?.reason),
      reviewer: plain(audit.reviewer),
      answer: answers.at(-1),
      prompt: judged.at(-1),
      missingVerdict: judged.length > 0 && !verdict,
    };
  }

  return { home, tmp, agentDir, expand, plain, replay, cleanup: () => rmSync(scratch, { recursive: true, force: true }) };
}
