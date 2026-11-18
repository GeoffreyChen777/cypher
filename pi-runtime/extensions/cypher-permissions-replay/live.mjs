#!/usr/bin/env node
// The replay against the real reviewing model: every fixture case and every
// probe goes through the extension's own path (classifier, prompt, model
// call, verdict parsing, explicit.ts and created.ts checks), with the model's
// answer instead of the stubbed verdict. Run by hand, never by `npm test`: it
// needs the network and a Claude account, and the model's answers vary.
//
// The model is called the way claude-bridge serves the extension's one-off
// review (pi-claude-bridge `runIsolatedSummary`): Claude Code through the
// Claude Agent SDK, with the judge's system prompt, no tools, no settings,
// one turn, thinking off. Owner words are the fixture's (what the engine
// passes in CYPHER_OWNER_WORDS). Probes run twice: with the live model on the
// probe's request, and with the probe's adversarial verdict (as the test does).
//
//   node live.mjs [--context FILE] [--out DIR] [--concurrency N] [--model ID]
//                 [--only 1,2,3] [--timeout-ms N] [--sdk PATH] [--cases FILE]
//                 [--label NAME]
//
// --context: per-case conversation (`{ "<id>": { branch, model } }`), built
//   from the owner's sessions and kept out of the repository; without it a
//   case carries only the fixture's `user` messages.
// --model: the default model (claude-opus-5-5); a case whose chat ran on
//   another claude-bridge model uses that one.
// --sdk: the Claude Agent SDK's sdk.mjs (default: the installed Runtime's).
// --cases: other cases in the fixture's format (`{ "cases": [...] }`), e.g. the
//   commands of one agent's session, instead of the fixture's.
// Results: <out>/<label>.json.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";

import { createMachine, fixture, stubbedModel } from "./harness.mjs";

const { values: args } = parseArgs({
  options: {
    context: { type: "string" },
    out: { type: "string", default: "/tmp/permission-review-live" },
    concurrency: { type: "string", default: "4" },
    model: { type: "string", default: "claude-opus-5-5" },
    only: { type: "string" },
    "timeout-ms": { type: "string", default: "120000" },
    sdk: { type: "string", default: join(homedir(), ".cypher/pi-runtime/current/npm/node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs") },
    claude: { type: "string" },
    label: { type: "string", default: "live" },
    cases: { type: "string" },
  },
});

const { query } = await import(args.sdk);
const context = args.context ? JSON.parse(readFileSync(args.context, "utf8")) : {};
const only = args.only ? new Set(args.only.split(",").map((id) => Number(id.trim()))) : undefined;
const productionTimeoutMs = 20_000;

// What claude-bridge sets for every Claude Code child (cc-child.ts CC_CHILD_ENV), without the parent session's markers.
const PARENT_SESSION_VARS = ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_PID", "CLAUDE_EFFORT", "TRACEPARENT", "CLAUDE_CODE_INVOKED_SKILLS", "CLAUDE_ENV_FILE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_AGENT_SDK_VERSION", "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_SSE_PORT", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN"];
const childEnv = { ...process.env };
for (const name of PARENT_SESSION_VARS) delete childEnv[name];
Object.assign(childEnv, {
  ENABLE_CLAUDEAI_MCP_SERVERS: "0",
  DISABLE_AUTO_COMPACT: "1",
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  CLAUDE_CODE_SILENT_TURN_REMINDER: "0",
});

const outDir = args.out;
mkdirSync(join(outDir, "work"), { recursive: true, mode: 0o700 });

/** The model a case's chat used: its own when it was a claude-bridge model, else the default. */
function modelFor(item) {
  const used = context[item.id]?.model;
  return used?.startsWith("claude-bridge/") ? used.slice("claude-bridge/".length) : args.model;
}

/** The reviewing model, as claude-bridge serves a one-off call. */
function liveModel(model, timings) {
  return async (system, prompt, signal) => {
    const started = Date.now();
    const sdk = query({
      prompt,
      options: {
        cwd: join(outDir, "work"),
        env: childEnv,
        settings: { autoMemoryEnabled: false },
        tools: [],
        strictMcpConfig: true,
        settingSources: [],
        skills: [],
        persistSession: false,
        systemPrompt: system,
        model,
        maxTurns: 1,
        thinking: { type: "disabled" },
        ...(args.claude ? { pathToClaudeCodeExecutable: args.claude } : {}),
      },
    });
    const onAbort = () => {
      void sdk.interrupt?.().catch(() => {});
      try {
        sdk.close?.();
      } catch {}
    };
    if (signal?.aborted) onAbort();
    else signal?.addEventListener("abort", onAbort, { once: true });
    let assistant = "";
    let final = "";
    let error;
    try {
      for await (const message of sdk) {
        if (message.type === "assistant") {
          for (const block of message.message?.content ?? []) if (block.type === "text") assistant += block.text;
        } else if (message.type === "result") {
          if (message.subtype === "success" && !message.is_error) final = message.result || assistant;
          else error = message.result || message.subtype;
        }
      }
    } finally {
      signal?.removeEventListener("abort", onAbort);
      timings.push(Date.now() - started);
    }
    if (signal?.aborted) return undefined;
    const text = final || assistant;
    if (error || !text.trim()) throw new Error(`The model stopped with an error: ${error ?? "no text"}`);
    return text;
  };
}

/** The agent's label and reason for this escalation, from its own tool call in the real conversation. */
function proposalFor(item, branch) {
  if (!branch) return undefined;
  for (let at = branch.length - 1; at >= 0; at--) {
    const message = branch[at].message;
    if (message?.role !== "assistant") continue;
    for (const part of message.content ?? []) {
      if (part.type === "toolCall" && part.arguments?.command === item.command) {
        const escalate = part.arguments.escalate;
        if (escalate && typeof escalate === "object") return { label: escalate.label, reason: escalate.reason };
        return undefined;
      }
    }
  }
  return undefined;
}

function verdictOf(answer) {
  if (typeof answer !== "string") return undefined;
  const match = answer.match(/\{[\s\S]*\}/);
  try {
    return match ? JSON.parse(match[0]) : undefined;
  } catch {
    return undefined;
  }
}

const replayed = args.cases ? JSON.parse(readFileSync(args.cases, "utf8")).cases : fixture.cases;
const machine = createMachine({ judgeTimeoutMs: Number(args["timeout-ms"]), cases: replayed });
const jobs = [];
for (const item of replayed) {
  if (only && !only.has(item.id)) continue;
  const branch = context[item.id]?.branch;
  const proposal = proposalFor(item, branch) ?? item.proposal;
  jobs.push({ item, branch, proposal });
  for (const [index, probe] of (item.probes ?? []).entries()) jobs.push({ item, probe, index, branch, proposal });
}

const rows = [];
let done = 0;
async function run(job) {
  const { item, probe, branch, proposal } = job;
  const timings = [];
  const model = modelFor(item);
  const got = await machine.replay(item, probe, { complete: liveModel(model, timings), branch, proposal });
  const row = {
    id: item.id,
    ...(probe ? { probe: probe.why, probeExpect: probe.expect.outcome, requestChanged: ["command", "owner", "branch"].some((key) => key in probe) } : {}),
    group: item.group,
    context: branch ? "session" : item.user ? "fixture user" : "none",
    model: got.judged ? model : undefined,
    outcome: got.outcome,
    tier: got.tier,
    kind: got.kind,
    floor: got.audit.explicit?.reason?.startsWith("only you can approve") || undefined,
    judged: got.judged,
    judgeVerdict: got.audit.judgeVerdict,
    judgeLatencyMs: timings[0],
    verdict: verdictOf(got.answer),
    reviewer: got.reviewer,
    explicit: got.explicit,
    onceOnly: got.onceOnly,
    stubbed: { expect: (probe ?? item).expect?.outcome, verdict: (probe ?? item).verdict?.decision, source: (probe ?? item).verdict?.source },
  };
  if (probe) {
    // The probe's own adversarial verdict, as the test runs it: the code must still ask.
    const adversarial = await machine.replay(item, probe, { complete: stubbedModel(probe.verdict), branch, proposal });
    row.adversarial = { outcome: adversarial.outcome, explicit: adversarial.explicit };
  }
  rows.push(row);
  done++;
  process.stderr.write(`\r${done}/${jobs.length} `);
}

const width = Math.max(1, Number(args.concurrency));
let next = 0;
await Promise.all(
  Array.from({ length: width }, async () => {
    while (next < jobs.length) {
      const job = jobs[next++];
      try {
        await run(job);
      } catch (error) {
        rows.push({ id: job.item.id, probe: job.probe?.why, error: String(error?.stack ?? error) });
      }
    }
  }),
);
process.stderr.write("\n");
machine.cleanup();

rows.sort((a, b) => a.id - b.id || (a.probe ?? "").localeCompare(b.probe ?? ""));
const cases = rows.filter((row) => !row.probe);
const probes = rows.filter((row) => row.probe);
const count = (list, key) => list.reduce((counts, row) => ({ ...counts, [key(row)]: (counts[key(row)] ?? 0) + 1 }), {});
const summary = {
  label: args.label,
  ranAt: new Date().toISOString(),
  defaultModel: args.model,
  models: count(cases.filter((row) => row.judged), (row) => row.model),
  totals: count(cases, (row) => row.outcome ?? "error"),
  stubbedTotals: count(cases, (row) => row.stubbed?.expect ?? "error"),
  changedFromStubbed: cases.filter((row) => row.outcome !== row.stubbed?.expect).map((row) => ({ id: row.id, stubbed: row.stubbed?.expect, live: row.outcome, verdict: row.judgeVerdict, reason: row.verdict?.reason ?? row.reviewer })),
  probes: {
    total: probes.length,
    liveAsk: probes.filter((row) => row.outcome === "ask").length,
    adversarialAsk: probes.filter((row) => row.adversarial?.outcome === "ask").length,
    // A probe that only changes the verdict asks the live model the case's own request: its approval is the case's, not a bypass.
    verdictOnlyApprovedLive: probes.filter((row) => !row.requestChanged && row.outcome !== "ask").map((row) => ({ id: row.id, probe: row.probe, live: row.outcome })),
    failures: probes.filter((row) => row.adversarial?.outcome !== "ask" || (row.requestChanged && row.outcome !== "ask")).map((row) => ({ id: row.id, probe: row.probe, live: row.outcome, adversarial: row.adversarial?.outcome, explicit: row.explicit })),
  },
  judgeOverProductionTimeout: cases.filter((row) => (row.judgeLatencyMs ?? 0) > productionTimeoutMs).map((row) => ({ id: row.id, ms: row.judgeLatencyMs })),
  errors: rows.filter((row) => row.error).length,
};
const file = join(outDir, `${args.label}.json`);
writeFileSync(file, JSON.stringify({ summary, rows }, null, 1), { mode: 0o600 });
console.log(JSON.stringify(summary, null, 1));
console.log(`results: ${file}`);
