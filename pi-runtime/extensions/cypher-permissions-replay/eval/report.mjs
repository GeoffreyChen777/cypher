#!/usr/bin/env node
// Tables over several evaluated versions ("sides") from run.mjs results, as Markdown
// (sanitized: case ids, reasons and clipped, already-sanitized commands only).
//
//   node eval/report.mjs --results DIR --corpus DIR [--sides before,after,round2] [--names "before (2de5fdae),round 1,round 2"]
//
// For each side it reads <results>/<side>-{stub-recorded,stub-approve,live,live-mutations,stub-regression}.json,
// plus the fresh adversarial batch when it was run separately (<side>-fresh-stub-<stub>.json,
// <side>-live-fresh.json), and the case files under <corpus> for examples. The first side is the baseline.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";

import { mechanismOf, scored } from "./mechanism.mjs";

const { values: args } = parseArgs({ options: { results: { type: "string" }, corpus: { type: "string" }, sides: { type: "string", default: "before,after" }, names: { type: "string" } } });
const SIDES = args.sides.split(",");
const NAMES = Object.fromEntries(SIDES.map((side, index) => [side, args.names?.split(",")[index] ?? side]));
const RUNS = ["stub-recorded", "stub-approve", "live", "live-mutations", "stub-regression"];
const EXTRA = { "stub-recorded": "fresh-stub-recorded", "stub-approve": "fresh-stub-approve", live: "live-fresh" };
const read = (file) => (existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : undefined);
function load(side, run) {
  const main = read(join(args.results, `${side}-${run}.json`));
  const extra = EXTRA[run] ? read(join(args.results, `${side}-${EXTRA[run]}.json`)) : undefined;
  if (!main && !extra) return undefined;
  const rows = [...(main?.rows ?? [])];
  const seen = new Set(rows.map((r) => `${r.file}:${r.id}`));
  for (const r of extra?.rows ?? []) if (!seen.has(`${r.file}:${r.id}`)) rows.push(r);
  return { rows };
}
const data = Object.fromEntries(SIDES.map((side) => [side, Object.fromEntries(RUNS.map((run) => [run, load(side, run)]))]));
const cases = new Map();
for (const name of ["heldout", "adversarial", "adversarial-fresh", "mutations-heldout", "regression-heldout"]) {
  const file = join(args.corpus, `${name}.json`);
  if (existsSync(file)) for (const item of JSON.parse(readFileSync(file, "utf8")).cases) cases.set(`${name}.json:${item.id}`, item);
}
const caseOf = (row) => cases.get(`${row.file}:${row.id}`);

const mech = (row) => mechanismOf({ outcome: row.outcome, explicit: row.explicit, kind: row.kind, onceOnly: row.onceOnly, judged: row.judged, judgeVerdict: row.judgeVerdict, decidedBy: row.decidedBy ?? row.mechanism?.split(":")[1] });
const asks = (row) => row.outcome === "ask";
const approves = (row) => row.outcome && row.outcome !== "ask" && row.outcome !== "denied";
const forcedApproves = (row) => row.adversarialStub && row.adversarialStub.outcome !== "ask" && row.adversarialStub.outcome !== "denied";
// Unscorable cases (no conversation) are left out; see mechanism.mjs `scored`.
const plainRows = (result) => (result?.rows ?? []).filter((r) => r.base === undefined && !r.adversarial && !r.error && scored(r, caseOf(r)));
const pct = (hits, n) => (n ? `${hits}/${n} = ${((100 * hits) / n).toFixed(1)}%` : "–");
const clip = (text, n = 90) => {
  const one = String(text ?? "").replace(/\s+/g, " ").trim();
  return (one.length > n ? `${one.slice(0, n)}…` : one).replace(/\|/g, "\\|").replace(/\b\d{1,3}(\.\d{1,3}){3}\b/g, "IP").replace(/[\w.+-]+@[\w-]+\.[\w.]+/g, "EMAIL");
};
const out = [];
const line = (text = "") => out.push(text);
const table = (head, rows) => {
  line(`| ${head.join(" | ")} |`);
  line(`| ${head.map(() => "---").join(" | ")} |`);
  for (const row of rows) line(`| ${row.join(" | ")} |`);
  line();
};
const isFresh = (r) => r.file === "adversarial-fresh.json";

function mutantsOf(result) {
  const byId = new Map((result?.rows ?? []).filter((r) => !r.error).map((r) => [`${r.file}:${r.id}`, r.outcome]));
  return (result?.rows ?? []).filter((r) => r.base !== undefined && !r.error).map((r) => ({ ...r, baseOutcome: byId.get(`${r.file}:${r.base}`) })).filter((r) => r.baseOutcome !== undefined);
}
const stable = (r) => (r.baseOutcome === "ask") === (r.outcome === "ask");

function headline(side, run) {
  const result = data[side][run];
  if (!result) return {};
  const rows = plainRows(result);
  const approved = rows.filter((r) => r.label?.owner === "allowed");
  const allow = approved.filter((r) => r.label?.should === "allow");
  const denied = rows.filter((r) => r.label?.owner === "denied");
  const adversarial = result.rows.filter((r) => r.adversarial && !r.error);
  const original = adversarial.filter((r) => !isFresh(r));
  const fresh = adversarial.filter(isFresh);
  const mutants = mutantsOf(result);
  const passingBase = mutants.filter((r) => r.baseOutcome !== "ask");
  const errors = result.rows.filter((r) => r.error).length;
  return {
    prompt: approved.length ? pct(approved.filter(asks).length, approved.length) : undefined,
    promptAllow: allow.length ? pct(allow.filter(asks).length, allow.length) : undefined,
    denied: denied.length ? pct(denied.filter(approves).length, denied.length) : undefined,
    adversarial: original.length ? pct(original.filter(approves).length, original.length) : undefined,
    adversarialForced: original.some((r) => r.adversarialStub) ? pct(original.filter(forcedApproves).length, original.filter((r) => r.adversarialStub).length) : undefined,
    fresh: fresh.length ? pct(fresh.filter(approves).length, fresh.length) : undefined,
    freshForced: fresh.some((r) => r.adversarialStub) ? pct(fresh.filter(forcedApproves).length, fresh.filter((r) => r.adversarialStub).length) : undefined,
    stability: mutants.length ? pct(mutants.filter(stable).length, mutants.length) : undefined,
    stabilityPassing: passingBase.length ? pct(passingBase.filter(stable).length, passingBase.length) : undefined,
    errors: String(errors),
  };
}
const h = Object.fromEntries(SIDES.map((side) => [side, Object.fromEntries(RUNS.map((run) => [run, headline(side, run)]))]));
const liveOf = (side) => ({ ...h[side].live, stability: h[side]["live-mutations"].stability, stabilityPassing: h[side]["live-mutations"].stabilityPassing, errors: `${h[side].live.errors ?? "–"} + ${h[side]["live-mutations"].errors ?? "–"}` });

const METRICS = [
  ["prompt rate, owner-approved held-out", "prompt"],
  ["prompt rate, should=allow", "promptAllow"],
  ["false approvals, owner-denied", "denied"],
  ["false approvals, original adversarial (59)", "adversarial"],
  ["…with the adversarial verdict forced (live runs)", "adversarialForced"],
  ["false approvals, fresh adversarial (124, unseen)", "fresh"],
  ["…with the adversarial verdict forced (live runs)", "freshForced"],
  ["mutation stability, all mutants", "stability"],
  ["mutation stability, bases that didn't ask", "stabilityPassing"],
  ["errors", "errors"],
];
for (const [title, run, get] of [["stub: recorded verdict", "stub-recorded", (s) => h[s]["stub-recorded"]], ["stub: approve-all (the code's bar alone)", "stub-approve", (s) => h[s]["stub-approve"]], ["live judge (mutations: fixed sample of 40 bases, seed 20261122)", "live", liveOf]]) {
  line(`## Headline, ${title}`);
  line();
  table(["metric", ...SIDES.map((s) => NAMES[s])], METRICS.filter(([, key]) => !(run !== "live" && key.endsWith("Forced"))).map(([name, key]) => [name, ...SIDES.map((s) => get(s)[key] ?? "–")]));
}

line("## Why owner-approved held-out cases still ask, by mechanism");
line();
const MECHS = ["A consent", "B target", "C provenance", "D lifecycle", "E shell/unknown", "judge said ask", "floor", "other"];
const cols = SIDES.flatMap((side) => ["stub-recorded", "stub-approve", "live"].map((run) => [side, run]));
const mc = cols.map(([side, run]) => {
  const counts = {};
  for (const r of plainRows(data[side][run]).filter((r) => r.label?.owner === "allowed" && asks(r))) counts[mech(r)] = (counts[mech(r)] ?? 0) + 1;
  return counts;
});
table(["mechanism", ...cols.map(([side, run]) => `${NAMES[side]} ${run}`)], MECHS.map((m) => [m, ...mc.map((c) => String(c[m] ?? 0))]));
line("Mechanisms come from the decision's reasons (`eval/mechanism.mjs`, one keyword map for every side). \"judge said ask\": the code let the model decide and the model asked. Lines the rules can't read go to the judge, so E shows up as \"judge said ask\" unless the explicit check names the unreadable part.");
line();

line("## Adversarial cases by class (approved / total)");
line();
const advCols = SIDES.flatMap((side) => [[side, "stub-recorded", false], [side, "stub-approve", false], [side, "live", false], [side, "live", true]]);
const advCount = ([side, run, forced]) => {
  const counts = {};
  for (const r of (data[side][run]?.rows ?? []).filter((x) => x.adversarial && !x.error)) {
    const c = (counts[r.adversarial.class] ??= [0, 0]);
    if (forced && !r.adversarialStub) continue;
    c[1]++;
    if (forced ? forcedApproves(r) : approves(r)) c[0]++;
  }
  return counts;
};
const advData = advCols.map(advCount);
const classes = [...new Set(advData.flatMap((d) => Object.keys(d)))];
table(["class", ...advCols.map(([side, run, forced]) => `${NAMES[side]} ${run}${forced ? " (forced)" : ""}`)], classes.map((c) => [c, ...advData.map((d) => (d[c] && d[c][1] ? `${d[c][0]}/${d[c][1]}` : "–"))]));

// Decision paths from an adversarial-only rerun (<side>-adv-<stub>.json), for rows written before run.mjs kept them.
const details = new Map();
for (const side of SIDES) for (const stub of ["approve", "recorded"]) for (const r of read(join(args.results, `${side}-adv-${stub}.json`))?.rows ?? []) details.set(`${side}:${r.file}:${r.id}`, r);
function decisionPath(r, side) {
  if (!r.explicitCheck && details.has(`${side}:${r.file}:${r.id}`)) r = { ...r, ...details.get(`${side}:${r.file}:${r.id}`) };
  const check = r.explicitCheck ? `explicit check ok: quote "${clip(r.explicitCheck.quote, 50)}" (${r.explicitCheck.quoteSource}), targets ${clip((r.explicitCheck.targets ?? []).join("; "), 80)}, passed ${(r.explicitCheck.passed ?? []).join("+")}` : "no explicit check";
  return `tier ${r.tier ?? "?"}${r.kind ? ` (${clip(r.kind, 60)})` : ""}; judge ${r.judgeVerdict ?? "?"}; ${check}`;
}

for (const side of SIDES.slice(1)) {
  line(`### Every false approval, ${NAMES[side]}`);
  line();
  const list = [];
  for (const run of ["stub-recorded", "stub-approve", "live"]) {
    for (const r of data[side][run]?.rows ?? []) {
      if (r.error) continue;
      const must = r.adversarial || r.label?.owner === "denied";
      if (!must || r.base !== undefined) continue;
      if (approves(r)) list.push({ run, r, outcome: r.outcome, path: decisionPath(r, side) });
      if (forcedApproves(r)) list.push({ run: `${run} (forced verdict)`, r, outcome: r.adversarialStub.outcome, path: r.adversarialStub.explicit ? `explicit: ${clip(r.adversarialStub.explicit, 80)}` : decisionPath(r, side) });
    }
  }
  if (!list.length) {
    line("None.");
    line();
    continue;
  }
  // One row per case, with the runs it passed in.
  const byCase = new Map();
  for (const item of list) {
    const key = item.r.adversarial ? `${item.r.file}:${item.r.id}` : `held-out:${item.r.id}`;
    const got = byCase.get(key) ?? { ...item, runs: [] };
    got.runs.push(`${item.run}: ${item.outcome}`);
    if (!/explicit check ok/.test(got.path) && /explicit check ok/.test(item.path)) got.path = item.path;
    byCase.set(key, got);
  }
  table(
    ["id", "set / class", "command", "passed in", "decision path"],
    [...byCase.values()].map(({ r, runs, path }) => [r.id, r.adversarial ? `${isFresh(r) ? "fresh" : "original"} ${r.adversarial.class} / ${clip(r.adversarial.variant, 45)}` : "owner-denied held-out", `\`${clip(caseOf(r)?.command ?? (caseOf(r)?.paths ?? []).join(" "), 70)}\``, runs.join("<br>"), path]),
  );
}

line("## Mutation stability by family (bases that didn't ask: stable / n)");
line();
const famCols = SIDES.flatMap((side) => [[side, "stub-recorded"], [side, "stub-approve"], [side, "live-mutations"]]);
const famData = famCols.map(([side, run]) => {
  const counts = {};
  for (const r of mutantsOf(data[side][run])) {
    if (r.baseOutcome === "ask") continue;
    const c = (counts[r.mutation?.family ?? "?"] ??= [0, 0]);
    c[1]++;
    if (stable(r)) c[0]++;
  }
  return counts;
});
const families = [...new Set(famData.flatMap((d) => Object.keys(d)))].sort();
table(["family", ...famCols.map(([side, run]) => `${NAMES[side]} ${run}`)], families.map((f) => [f, ...famData.map((d) => (d[f] ? `${d[f][0]}/${d[f][1]}` : "–"))]));

const last = SIDES.at(-1);
for (const run of ["stub-recorded", "stub-approve"]) {
  const broken = mutantsOf(data[last][run]).filter((r) => r.baseOutcome !== "ask" && r.outcome === "ask");
  if (!broken.length) continue;
  line(`Mutants that ask where their base passes (${NAMES[last]} ${run}), by family and reason:`);
  line();
  const groups = {};
  for (const r of broken) {
    const key = `${r.mutation?.family} — ${mech(r)}: ${clip(normalize(r.explicit || r.kind || r.onceOnly || ""), 80)}`;
    (groups[key] ??= []).push(r);
  }
  table(["family — reason", "n", "example"], Object.entries(groups).sort((a, b) => b[1].length - a[1].length).slice(0, 25).map(([key, rows]) => [key, rows.length, `\`${clip(caseOf(rows[0])?.command, 80)}\``]));
}

line("## Regression sample (auto-decided records), ask vs not ask, against the baseline");
line();
const reg = (side) => data[side]["stub-regression"]?.rows ?? [];
for (const side of SIDES.slice(1)) {
  if (!reg(SIDES[0]).length || !reg(side).length) continue;
  const old = new Map(reg(SIDES[0]).map((r) => [r.id, r]));
  const changed = reg(side).filter((r) => old.has(r.id) && asks(old.get(r.id)) !== asks(r));
  line(`${NAMES[side]}: ${reg(side).filter(asks).length}/${reg(side).length} ask (baseline ${reg(SIDES[0]).filter(asks).length}). Newly ask: ${changed.filter(asks).length}; newly pass: ${changed.filter((r) => !asks(r)).length}.`);
  line();
  if (changed.length) table(["id", "change", "recorded", "reason", "command"], changed.map((r) => [r.id, `${old.get(r.id).outcome} → ${r.outcome}`, `${r.recorded?.decidedBy}/${r.recorded?.decision}`, `${mech(r)}: ${clip(r.explicit || r.kind || r.reviewer, 70)}`, `\`${clip(caseOf(r)?.command ?? (caseOf(r)?.paths ?? []).join(" "), 70)}\``]));
}

function normalize(text) {
  return String(text)
    .replace(/"[^"]*"/g, '"…"')
    .replace(/`[^`]*`/g, "`…`")
    .replace(/(@HOME@|\/tmp|\/private|\/Users)\S*/g, "PATH")
    .replace(/#?\b\d+(\.\d+)*\b/g, "N")
    .replace(/\b[0-9a-f]{7,}\b/g, "H");
}

for (const run of ["live", "stub-recorded"]) {
  const rows = plainRows(data[last][run]).filter((r) => r.label?.owner === "allowed" && asks(r));
  if (!rows.length) continue;
  line(`## Remaining asks on owner-approved held-out (${NAMES[last]} ${run}): ${rows.length}, by reason`);
  line();
  const groups = {};
  for (const r of rows) {
    const reason = r.explicit || (mech(r) === "judge said ask" ? `the model asked${r.tier ? ` (${r.tier}: ${r.kind ?? ""})` : ""}` : r.kind || r.onceOnly || "?");
    const key = `${mech(r)} — ${clip(normalize(reason), 100)}`;
    (groups[key] ??= []).push(r);
  }
  table(["mechanism — reason", "n", "examples"], Object.entries(groups).sort((a, b) => b[1].length - a[1].length).slice(0, 40).map(([key, list]) => [key, list.length, list.slice(0, 2).map((r) => `#${r.id} \`${clip(caseOf(r)?.command ?? (caseOf(r)?.paths ?? []).join(" "), 70)}\``).join("<br>")]));
}
console.log(out.join("\n"));
