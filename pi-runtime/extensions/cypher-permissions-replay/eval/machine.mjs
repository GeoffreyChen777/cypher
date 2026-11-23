// The eval's fake machine: harness.mjs's `createMachine`, but loading the
// extension from a given `extensions` directory (so a baseline copy and the
// worktree's code run the same cases), and passing the owner's words in the
// format that extension reads.
//
// Kept apart from harness.mjs on purpose: the harness imports the worktree's
// extension statically, and the eval must not depend on (or be edited with)
// the code it measures.
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

/**
 * The owner's words as an extension reads them.
 * - "full": every consent source (typed, reply, answer, card, plan) with its fields, as the new engine passes them.
 * - "legacy": what the engine on origin/main passed: typed messages (a reply is typed) and the plan's
 *   instruction, at most ten, the instruction first; no answers, no cards, no extra fields.
 */
export function ownerWordsFor(owner, mode) {
  if (!owner) return undefined;
  const messages = owner.messages ?? [];
  if (mode === "full") return { messages, agentWritten: owner.agentWritten === true };
  const plan = messages.filter((word) => word.source === "plan").map((word) => ({ text: word.text, source: "plan" }));
  const typed = messages.filter((word) => word.source === "typed" || word.source === "reply").map((word) => ({ text: word.text, source: "typed" }));
  return { messages: [...plan.slice(0, 1), ...typed.slice(-(10 - Math.min(1, plan.length)))], agentWritten: owner.agentWritten === true };
}

function findProgram(UNKNOWN) {
  return (program, cwd) => {
    if (!program || program === UNKNOWN) return undefined;
    if (program.includes("/")) {
      if (cwd === undefined && !isAbsolute(program)) return undefined;
      return isAbsolute(program) ? program : resolve(cwd, program);
    }
    return `/usr/bin/${program}`;
  };
}

function processFacts(item) {
  const of = (target) => ("pids" in target ? target.pids.map((pid) => item.processes?.[pid] ?? "foreign") : [item.processes?.pgrep ?? "foreign"]);
  return {
    owns: (target) => of(target).every((kind) => kind === "chat"),
    facts: (target) => {
      const kinds = of(target);
      return { owned: kinds.every((kind) => kind === "chat"), inRoots: kinds.every((kind) => kind === "chat" || kind === "roots"), installedApp: kinds.some((kind) => kind === "app") };
    },
  };
}

/** Load `<extensions>/cypher-permissions/{core,shell}.ts` and build the fake machine for `cases`. */
export async function createMachine({ extensions, cases, ownerWords = "full", judgeTimeoutMs }) {
  const { Permissions } = await import(pathToFileURL(join(extensions, "cypher-permissions", "core.ts")).href);
  // The fake machine's read-only directories, on any host (an extension older than the hook reads the real disk).
  const commandsModule = await import(pathToFileURL(join(extensions, "cypher-permissions", "commands.ts")).href);
  commandsModule.setSystemDirectory?.((path) => /^\/(Applications|Library|System|usr|opt\/local)\//.test(path));
  const { UNKNOWN } = await import(pathToFileURL(join(extensions, "cypher-permissions", "shell.ts")).href);
  const scratch = realpathSync(mkdtempSync(join(tmpdir(), "cypher-eval-")));
  const home = join(scratch, "home");
  const tmp = join(scratch, "tmp");
  mkdirSync(tmp, { recursive: true });
  const agentDir = join(home, ".cypher", "pi-runtime", "agent");
  mkdirSync(agentDir, { recursive: true });
  if (judgeTimeoutMs) writeFileSync(join(agentDir, "permissions.json"), JSON.stringify({ judge: { timeoutMs: judgeTimeoutMs } }));
  mkdirSync(join(home, ".cypher-development"), { recursive: true });
  mkdirSync(join(home, ".cypher", "orgs", "org_FIXTURE", "user_FIXTURE"), { recursive: true });
  const expand = (text) => text.replace(/(^|[^\w.~-])\/tmp(?=\/|\b)/g, `$1${tmp}`).replaceAll("@HOME@", home);
  const plain = (text) => (typeof text === "string" ? text.replaceAll(home, "@HOME@").replaceAll(tmp, "/tmp") : text);
  const made = new Set();
  function repo(path) {
    if (made.has(path)) return;
    made.add(path);
    mkdirSync(path, { recursive: true });
    spawnSync("git", ["init", "-q", "-b", "main", path]);
    spawnSync("git", ["-C", path, "-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "init"]);
  }
  for (const cwd of new Set(cases.map((item) => item.cwd))) {
    try {
      repo(expand(cwd));
    } catch {}
  }
  for (const item of cases) if (item.checkout) spawnSync("git", ["-C", expand(item.cwd), "switch", "-q", "-c", item.checkout]);
  const resolveIn = findProgram(UNKNOWN);

  let replays = 0;
  async function replay(item, { complete, branch: conversation, proposal } = {}) {
    const cwd = expand(item.cwd);
    // A chat of its own per replay: the extension may keep a ledger per chat on disk (ledger.ts), and
    // one case's creations must not become another's.
    const chatId = `chat-replay-${++replays}`;
    const permissions = new Permissions({ agentDir, chatId, cwd, home, landstrip: { available: true, binary: "/nonexistent/landstrip" }, scratch: [tmp] });
    permissions.appendEntry = () => {};
    const records = [];
    permissions.audit.write = (record) => records.push(record);
    permissions.resolveIn = resolveIn;
    // What this chat made earlier: planned and settled with its output when the corpus has it (as a
    // real run records it, so a lone creator's printed name counts), else noted from the command alone.
    for (const [index, earlier] of (item.ran ?? []).entries()) {
      try {
        // The transcript shows the result with the extension's own note after it ("[permissions] Ran outside the
        // sandbox …"); the run settles on the command's output without it (cypher-permissions.ts `bashOutcome`).
        const shown = item.ranOutputs?.[index];
        const output = typeof shown === "string" ? shown.replace(/\n*\[permissions\][^\n]*(?:\n[^\n]*)*$/, "") : shown;
        if (typeof output === "string" && typeof permissions.planCreated === "function") {
          const plan = permissions.planCreated(expand(earlier), cwd);
          if (plan) permissions.settleCreated(plan, { exitCode: 0, output: expand(output) });
          else permissions.noteCreated(expand(earlier), cwd);
        } else permissions.noteCreated(expand(earlier), cwd);
      } catch {}
    }
    const processes = processFacts(item);
    permissions.ownsProcesses = processes.owns;
    permissions.processFacts = processes.facts;
    permissions.ownerWords = ownerWordsFor(item.owner ? JSON.parse(expand(JSON.stringify(item.owner))) : undefined, ownerWords);
    permissions.releaseFacts = (_dir, commit, version) => {
      const facts = item.release?.[commit];
      return facts ? { onDefault: facts.onDefault, hasVersion: facts.versions.includes(version) } : {};
    };
    permissions.originRepo = () => item.originRepo;
    const user = (item.user ?? ["(the owner's request; not in the audit)"]).map((content) => ({ type: "message", message: { role: "user", content } }));
    const base = conversation ? JSON.parse(expand(JSON.stringify(conversation))) : user;
    const extra = conversation ? [] : (item.branch ?? []);
    const branch = [...base, ...JSON.parse(expand(JSON.stringify(extra)))];
    const judged = [];
    const answers = [];
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
      branch: () => branch,
      complete: async (system, prompt, signal) => {
        judged.push(prompt);
        const answer = await complete(system, prompt, signal, item);
        answers.push(answer);
        return answer;
      },
    };
    const before = permissions.grants.all().filter((grant) => grant.scope !== "once").length;
    let result;
    const command = item.command !== undefined ? expand(item.command) : undefined;
    if (item.capability === "escalate") result = await permissions.requestEscalation(command, proposal ?? item.proposal ?? { label: "replay" }, ctx);
    else if (item.tool === "request_permission") result = await permissions.requestPermission({ capability: item.capability, paths: item.paths.map(expand) }, ctx);
    else result = await permissions.authorize(item.paths.map((path) => ({ capability: item.capability, path: expand(path) })), { tool: item.tool }, ctx);
    const audit = records.at(-1) ?? {};
    const remembered = permissions.grants.all().filter((grant) => grant.scope !== "once").length > before;
    let outcome;
    if (dialogs.length) outcome = "ask";
    else if (!result.ok) outcome = "denied";
    else if (audit.decidedBy === "judge") outcome = remembered ? "approve-remember" : "approve-once";
    else outcome = audit.decidedBy ? `auto-${audit.decidedBy}` : "allowed";
    return {
      outcome,
      tier: audit.tier,
      kind: plain(audit.kind),
      judged: judged.length > 0,
      audit: JSON.parse(plain(JSON.stringify(audit))),
      onceOnly: plain(audit.onceOnly),
      explicit: plain(audit.explicit?.reason ?? (typeof audit.explicit === "string" ? audit.explicit : undefined)),
      reviewer: plain(audit.reviewer),
      answer: answers.at(-1),
      dialog: dialogs[0]?.title ? plain(String(dialogs[0].title)).slice(0, 400) : undefined,
    };
  }

  return { home, tmp, expand, plain, replay, cleanup: () => rmSync(scratch, { recursive: true, force: true }) };
}
