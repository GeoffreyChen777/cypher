#!/usr/bin/env node
// Before/after tables from run.mjs results, as Markdown (sanitized: case ids, reasons and
// clipped, already-sanitized commands only).
//
//   node eval/report.mjs --results DIR --corpus DIR [--before before] [--after after] [--live live-v2]
//
// Reads <results>/<side>-{stub-recorded,stub-approve,live,live-mutations,stub-regression}.json
// for each side that exists, and the case files under <corpus> for examples.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseArgs } from "node:util";

import { mechanismOf } from "./mechanism.mjs";

const { values: args } = parseArgs({ options: { results: { type: "string" }, corpus: { type: "string" }, before: { type: "string", default: "before" }, after: { type: "string", default: "after" }, live: { type: "string", default: "live" } } });
const RUNS = ["stub-recorded", "stub-approve", "live", "live-mutations", "stub-regression"];
const SIDES = [args.before, args.after];
const load = (side, run) => {
  const file = join(args.results, `${side}-${run === "live" ? args.live : run}.json`);
  return existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : undefined;
};
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
const plainRows = (result) => (result?.rows ?? []).filter((r) => r.base === undefined && !r.adversarial && !r.error);
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

function headline(result) {
  if (!result) return {};
  const rows = plainRows(result);
  const approved = rows.filter((r) => r.label?.owner === "allowed");
  const allow = approved.filter((r) => r.label?.should === "allow");
  const denied = rows.filter((r) => r.label?.owner === "denied");
  const adversarial = result.rows.filter((r) => r.adversarial && !r.error);
  const forced = adversarial.filter((r) => r.adversarialStub);
  const byId = new Map(result.rows.filter((r) => !r.error).map((r) => [`${r.file}:${r.id}`, r.outcome]));
  const mutants = result.rows.filter((r) => r.base !== undefined && !r.error).map((r) => ({ ...r, baseOutcome: byId.get(`${r.file}:${r.base}`) })).filter((r) => r.baseOutcome !== undefined);
  const stable = (r) => (r.baseOutcome === "ask") === (r.outcome === "ask");
  const passingBase = mutants.filter((r) => r.baseOutcome !== "ask");
  return {
    prompt: pct(approved.filter(asks).length, approved.length),
    promptAllow: pct(allow.filter(asks).length, allow.length),
    denied: pct(denied.filter(approves).length, denied.length),
    adversarial: adversarial.length ? pct(adversarial.filter(approves).length, adversarial.length) : "–",
    forced: forced.length ? pct(forced.filter((r) => r.adversarialStub.outcome !== "ask" && r.adversarialStub.outcome !== "denied").length, forced.length) : "–",
    stability: mutants.length ? pct(mutants.filter(stable).length, mutants.length) : "–",
    stabilityPassing: passingBase.length ? pct(passingBase.filter(stable).length, passingBase.length) : "–",
    errors: result.rows.filter((r) => r.error).length,
    mutants,
  };
}

const B = args.before;
const A = args.after;
const h = Object.fromEntries(SIDES.map((side) => [side, Object.fromEntries(RUNS.map((run) => [run, headline(data[side][run])]))]));
const live = (side) => ({ ...h[side].live, stability: h[side]["live-mutations"].stability, stabilityPassing: h[side]["live-mutations"].stabilityPassing });

line("## Headline (held-out)");
line();
table(
  ["metric", `${B} stub recorded`, `${A} stub recorded`, `${B} stub approve-all`, `${A} stub approve-all`, `${B} live`, `${A} live`],
  [
    ["prompt rate, owner-approved", "prompt"],
    ["prompt rate, should=allow", "promptAllow"],
    ["false approvals, owner-denied", "denied"],
    ["false approvals, adversarial", "adversarial"],
    ["false approvals, adversarial (forced adversarial verdict)", "forced"],
    ["mutation stability, all", "stability"],
    ["mutation stability, bases that didn't ask", "stabilityPassing"],
    ["errors", "errors"],
  ].map(([name, key]) => [name, h[B]["stub-recorded"][key] ?? "–", h[A]["stub-recorded"][key] ?? "–", h[B]["stub-approve"][key] ?? "–", h[A]["stub-approve"][key] ?? "–", live(B)[key] ?? "–", live(A)[key] ?? "–"]),
);

line("## Why owner-approved held-out cases still ask, by mechanism");
line();
const MECHS = ["A consent", "B target", "C provenance", "D lifecycle", "E shell/unknown", "judge said ask", "floor", "other"];
const mechCount = (side, run) => {
  const rows = plainRows(data[side][run]).filter((r) => r.label?.owner === "allowed" && asks(r));
  const counts = {};
  for (const r of rows) counts[mech(r)] = (counts[mech(r)] ?? 0) + 1;
  return counts;
};
const mc = Object.fromEntries(SIDES.flatMap((side) => ["stub-recorded", "stub-approve", "live"].map((run) => [`${side}/${run}`, mechCount(side, run)])));
table(
  ["mechanism", ...SIDES.flatMap((side) => ["stub-recorded", "stub-approve", "live"].map((run) => `${side} ${run}`))],
  MECHS.map((m) => [m, ...SIDES.flatMap((side) => ["stub-recorded", "stub-approve", "live"].map((run) => String(mc[`${side}/${run}`][m] ?? 0)))]),
);
line("Mechanisms come from the decision's reasons (`eval/mechanism.mjs`, the same keyword map for both sides). \"judge said ask\": the code would have let the model decide and the model asked; \"E shell/unknown\": the line could not be read and no verdict decided it.");
line();

line("## Adversarial cases, by class (approved / total)");
line();
const advClass = (result, forced) => {
  const counts = {};
  for (const r of (result?.rows ?? []).filter((x) => x.adversarial && !x.error)) {
    const c = (counts[r.adversarial.class] ??= [0, 0]);
    c[1]++;
    const outcome = forced ? r.adversarialStub?.outcome : r.outcome;
    if (outcome && outcome !== "ask" && outcome !== "denied") c[0]++;
  }
  return counts;
};
const classes = [...new Set((data[A]["stub-recorded"]?.rows ?? data[B]["stub-recorded"]?.rows ?? []).filter((r) => r.adversarial).map((r) => r.adversarial.class))];
const advCols = [[B, "stub-recorded"], [A, "stub-recorded"], [B, "stub-approve"], [A, "stub-approve"], [B, "live"], [A, "live"]];
const advData = advCols.map(([side, run]) => advClass(data[side][run]));
table(["class", ...advCols.map(([side, run]) => `${side} ${run}`)], classes.map((c) => [c, ...advData.map((d) => (d[c] ? `${d[c][0]}/${d[c][1]}` : "–"))]));

line(`### Every adversarial case that passes (${A})`);
line();
// Decision paths of adversarial rows from a separate adversarial-only run (`<side>-adv-<stub>.json`), when the main run predates them.
const detail = new Map();
for (const stub of ["recorded", "approve"]) {
  const extra = load(A, `adv-${stub}`);
  for (const r of extra?.rows ?? []) detail.set(`stub-${stub}:${r.id}`, r);
  if (stub === "approve") for (const r of extra?.rows ?? []) detail.set(`live:${r.id}`, r);
}
const passes = [];
for (const run of ["stub-recorded", "stub-approve", "live"]) {
  for (const r of (data[A][run]?.rows ?? []).filter((x) => x.adversarial && !x.error)) {
    if (approves(r)) passes.push({ run, r, outcome: r.outcome });
    if (r.adversarialStub && r.adversarialStub.outcome !== "ask" && r.adversarialStub.outcome !== "denied") passes.push({ run: `${run} (forced verdict)`, r, outcome: r.adversarialStub.outcome });
  }
}
if (!passes.length) line("None.");
else {
  table(
    ["run", "id", "class / variant", "command", "outcome", "decision path"],
    passes.map(({ run, r: row, outcome }) => {
      const r = row.explicitCheck ? row : { ...row, ...(detail.get(`${run.split(" ")[0]}:${row.id}`) ?? {}) };
      const item = caseOf(r);
      const check = r.explicitCheck ? `explicit check ok: quote "${clip(r.explicitCheck.quote, 50)}" (${r.explicitCheck.quoteSource}), targets ${clip((r.explicitCheck.targets ?? []).join("; "), 80)}, passed ${(r.explicitCheck.passed ?? []).join("+")}` : "no explicit check";
      return [run, r.id, `${r.adversarial.class} / ${clip(r.adversarial.variant, 50)}`, `\`${clip(item?.command, 70)}\``, outcome, `tier ${r.tier ?? "?"} (${clip(r.kind, 60)}); judge ${r.judgeVerdict ?? "?"}; ${check}`];
    }),
  );
}

line("## Mutation stability by family (bases that didn't ask: stable / n)");
line();
const fam = (result) => {
  const counts = {};
  for (const r of headline(result).mutants ?? []) {
    if (r.baseOutcome === "ask") continue;
    const c = (counts[r.mutation?.family ?? "?"] ??= [0, 0]);
    c[1]++;
    if ((r.baseOutcome === "ask") === (r.outcome === "ask")) c[0]++;
  }
  return counts;
};
const famCols = [[B, "stub-recorded"], [A, "stub-recorded"], [B, "stub-approve"], [A, "stub-approve"], [B, "live-mutations"], [A, "live-mutations"]];
const famData = famCols.map(([side, run]) => fam(data[side][run]));
const families = [...new Set(famData.flatMap((d) => Object.keys(d)))].sort();
table(["family", ...famCols.map(([side, run]) => `${side} ${run}`)], families.map((f) => [f, ...famData.map((d) => (d[f] ? `${d[f][0]}/${d[f][1]}` : "–"))]));

for (const run of ["stub-recorded", "stub-approve"]) {
  const broken = (headline(data[A][run]).mutants ?? []).filter((r) => r.baseOutcome !== "ask" && r.outcome === "ask");
  if (!broken.length) continue;
  line(`Mutants that ask where their base passes (${A} ${run}), by family and reason:`);
  line();
  const groups = {};
  for (const r of broken) {
    const key = `${r.mutation?.family} — ${mech(r)}: ${clip(normalize(r.explicit || r.kind || r.onceOnly || ""), 80)}`;
    (groups[key] ??= []).push(r);
  }
  table(["family — reason", "n", "example"], Object.entries(groups).sort((a, b) => b[1].length - a[1].length).slice(0, 25).map(([key, rows]) => [key, rows.length, `\`${clip(caseOf(rows[0])?.command, 80)}\``]));
}

line("## Regression sample (auto-decided records), ask vs not ask");
line();
const reg = (side) => data[side]["stub-regression"]?.rows ?? [];
if (reg(B).length && reg(A).length) {
  const old = new Map(reg(B).map((r) => [r.id, r]));
  const changed = reg(A).filter((r) => old.has(r.id) && asks(old.get(r.id)) !== asks(r));
  line(`${B}: ${reg(B).filter(asks).length}/${reg(B).length} ask; ${A}: ${reg(A).filter(asks).length}/${reg(A).length} ask. Newly ask: ${changed.filter(asks).length}; newly pass: ${changed.filter((r) => !asks(r)).length}.`);
  line();
  if (changed.length)
    table(["id", "change", "recorded", `${A} reason`, "command"], changed.map((r) => [r.id, `${old.get(r.id).outcome} → ${r.outcome}`, `${r.recorded?.decidedBy}/${r.recorded?.decision}`, `${mech(r)}: ${clip(r.explicit || r.kind || r.reviewer, 70)}`, `\`${clip(caseOf(r)?.command ?? (caseOf(r)?.paths ?? []).join(" "), 70)}\``]));
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
  const rows = plainRows(data[A][run]).filter((r) => r.label?.owner === "allowed" && asks(r));
  if (!rows.length) continue;
  line(`## Remaining asks on owner-approved held-out (${A} ${run}): ${rows.length}, by reason`);
  line();
  const groups = {};
  for (const r of rows) {
    const reason = r.explicit || (mech(r) === "judge said ask" ? `the model asked${r.tier ? ` (${r.tier}: ${r.kind ?? ""})` : ""}` : r.kind || r.onceOnly || "?");
    const key = `${mech(r)} — ${clip(normalize(reason), 100)}`;
    (groups[key] ??= []).push(r);
  }
  table(
    ["mechanism — reason", "n", "examples"],
    Object.entries(groups)
      .sort((a, b) => b[1].length - a[1].length)
      .slice(0, 40)
      .map(([key, list]) => [key, list.length, list.slice(0, 2).map((r) => `#${r.id} \`${clip(caseOf(r)?.command ?? (caseOf(r)?.paths ?? []).join(" "), 70)}\``).join("<br>")]),
  );
}
console.log(out.join("\n"));
