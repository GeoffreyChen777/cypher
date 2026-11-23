// Target identity (entities.ts), the tool table (tools.ts) and the consent
// sources (explicit.ts): unit tests, and property tests over generated
// entities and command lines (a seeded generator, so a failure repeats).
import assert from "node:assert/strict";
import { test } from "node:test";

import { canonical, covers, ENTITY_KINDS, identifies, namesOf, relation } from "./cypher-permissions/entities.ts";
import { checkExplicit, consentText, lineTargets, parseOwnerWords } from "./cypher-permissions/explicit.ts";
import { judgePrompt, parseVerdict } from "./cypher-permissions/judge.ts";
import { UNKNOWN } from "./cypher-permissions/shell.ts";
import { readTool, TOOLS } from "./cypher-permissions/tools.ts";

// --- A seeded generator -----------------------------------------------------------

function prng(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rand = prng(Number(process.env.PERMISSIONS_SEED ?? 20261122));
const pick = (list) => list[Math.floor(rand() * list.length)];
const word = () => pick(["fix", "badge", "relay", "main", "dev", "x", "feature", "ios", "cypher", "gamma", "web"]) + (rand() < 0.5 ? "" : `-${pick(["a", "b", "2", "cluster", "links"])}`);
const ctx = { currentRepo: "me/app", remotes: { origin: "me/app", upstream: "other/app" }, cwd: "/w/app", home: "/home/me", simulators: [{ udid: "11111111-2222-3333-4444-555555555555", name: "iPhone Test" }] };

/** A random entity of a random kind, drawn from a small name space so pairs relate often. */
function entity() {
  const kind = pick(["repo", "remote", "branch", "pr", "tag", "release", "path", "host", "package", "simulator", "image", "container", "resource"]);
  switch (kind) {
    case "repo":
      return { kind, name: pick(["me/app", "other/app", "app", "me/lib"]) };
    case "remote":
      return { kind, name: pick(["origin", "upstream"]) };
    case "branch":
      return { kind, name: pick(["fix", "main", "feature", "origin/fix", "upstream/fix", "refs/heads/main"]), ...(rand() < 0.3 ? { repo: pick(["me/app", "other/app"]) } : {}) };
    case "pr":
      return rand() < 0.5
        ? { kind, name: pick(["1", "2", "#1"]), ...(rand() < 0.5 ? { head: pick(["fix", "feature"]), base: "main" } : {}) }
        : { kind, name: "", head: pick(["fix", "feature"]), ...(rand() < 0.7 ? { base: pick(["main", "dev"]) } : {}) };
    case "tag":
    case "release":
      return { kind, name: pick(["v1.0", "v1.1", "refs/tags/v1.0"]) };
    case "path":
      return { kind, name: pick(["/w/app", "/w/app/src", "/w/app/src/a.ts", "src", "src/a.ts", "~/shots", "~/shots/1.png", "/w/application", "/tmp/x"]) };
    case "host":
      return { kind, name: pick(["gamma", "gamma-cluster", "gamma-cluster-1", "web", "web.example.com", "me@web.example.com", "ssh://gamma-cluster:22", "gam"]) };
    case "package":
      return { kind, name: pick(["pnpm", "pnpm@9", "pnpm@8", "@me/app", "@me/app@0.3.2", "ripgrep"]) };
    case "simulator":
      return { kind, name: pick(["11111111-2222-3333-4444-555555555555", "iPhone Test", "99999999-2222-3333-4444-555555555555", "Other"]) };
    case "image":
      return { kind, name: pick(["ghcr.io/me/app", "ghcr.io/me/app:1", "ghcr.io/me/app:2", "app"]) };
    case "container":
      return { kind, name: pick(["web", "db"]) };
    default:
      return { kind, name: pick(["web", "db"]), type: "deployment", ...(rand() < 0.5 ? { scope: pick(["prod", "dev"]) } : {}) };
  }
}

// --- Relations ----------------------------------------------------------------------

test("entities: a pull request holds its head branch, not its base nor another branch", () => {
  const pr = { kind: "pr", name: "52", head: "fix-badge", base: "main" };
  assert.equal(relation(pr, { kind: "branch", name: "fix-badge" }, ctx), "contains");
  assert.equal(relation({ kind: "branch", name: "origin/fix-badge" }, pr, ctx), "contained");
  assert.equal(relation(pr, { kind: "branch", name: "main" }, ctx), "unrelated", "naming main covers no pull request into it");
  assert.equal(relation(pr, { kind: "branch", name: "other" }, ctx), "unrelated");
  assert.equal(relation({ kind: "pr", name: "52" }, { kind: "branch", name: "fix-badge" }, ctx), "unknown", "its head not known");
  // Facts known in this chat fill the head in.
  assert.equal(relation({ kind: "pr", name: "#52" }, { kind: "branch", name: "fix-badge" }, { ...ctx, prs: [{ number: "52", head: "fix-badge", base: "main" }] }), "contains");
  assert.equal(identifies({ kind: "branch", name: "main" }, { kind: "pr", name: "", head: "fix", base: "main" }, ctx), true, "a new pull request by the branch it goes into");
  assert.equal(identifies({ kind: "branch", name: "fix" }, { kind: "pr", name: "", head: "fix", base: "main" }, ctx), true);
  assert.equal(identifies({ kind: "branch", name: "dev" }, { kind: "pr", name: "", head: "fix", base: "main" }, ctx), false);
});

test("entities: repositories, remotes, tags and releases", () => {
  assert.equal(relation({ kind: "branch", name: "fix", repo: "Me/App" }, { kind: "branch", name: "fix" }, ctx), "same", "owner/repo-qualified and bare in the current repository");
  assert.equal(relation({ kind: "branch", name: "fix", repo: "other/app" }, { kind: "branch", name: "fix" }, ctx), "unrelated");
  assert.equal(relation({ kind: "branch", name: "origin/fix" }, { kind: "branch", name: "fix" }, ctx), "same");
  assert.equal(relation({ kind: "branch", name: "upstream/fix" }, { kind: "branch", name: "fix" }, ctx), "unrelated", "another remote's repository");
  assert.equal(relation({ kind: "repo", name: "https://github.com/me/app.git" }, { kind: "pr", name: "3" }, ctx), "contains");
  assert.equal(relation({ kind: "repo", name: "app" }, { kind: "repo", name: "me/app" }, {}), "contains", "a bare name holds every owner's");
  assert.equal(relation({ kind: "remote", name: "origin" }, { kind: "branch", name: "fix" }, ctx), "contains");
  assert.equal(relation({ kind: "tag", name: "v1.2.0" }, { kind: "release", name: "refs/tags/v1.2.0" }, ctx), "same", "a release is its tag");
  assert.equal(relation({ kind: "tag", name: "v1.2.0" }, { kind: "branch", name: "v1.2.0" }, ctx), "unrelated");
});

test("entities: paths, hosts, simulators, packages and images", () => {
  assert.equal(relation({ kind: "path", name: "~/shots" }, { kind: "path", name: "/home/me/shots/a.png" }, ctx), "contains");
  assert.equal(relation({ kind: "path", name: "src/../src/a.ts" }, { kind: "path", name: "/w/app/src/a.ts" }, ctx), "same");
  assert.equal(relation({ kind: "path", name: "/w/app" }, { kind: "path", name: "/w/application" }, ctx), "unrelated", "a common prefix is no folder");
  assert.equal(relation({ kind: "path", name: "src" }, { kind: "path", name: "/x" }, {}), "unknown", "relative, with no directory");
  assert.equal(relation({ kind: "worktree", name: "/w/app" }, { kind: "path", name: "/w/app/target" }, ctx), "contains");
  assert.equal(relation({ kind: "host", name: "gamma" }, { kind: "host", name: "deploy@gamma-cluster" }, ctx), "contains");
  assert.equal(relation({ kind: "host", name: "web" }, { kind: "host", name: "https://web.example.com/x" }, ctx), "contains");
  assert.equal(relation({ kind: "host", name: "gam" }, { kind: "host", name: "gamma-cluster" }, ctx), "unrelated", "too short to be a family");
  assert.equal(relation({ kind: "simulator", name: "11111111-2222-3333-4444-555555555555" }, { kind: "simulator", name: "iPhone Test" }, ctx), "same", "by its UDID and its name");
  assert.equal(relation({ kind: "simulator", name: "99999999-2222-3333-4444-555555555555" }, { kind: "simulator", name: "iPhone Test" }, ctx), "unrelated");
  assert.equal(relation({ kind: "simulator", name: "99999999-2222-3333-4444-555555555555" }, { kind: "simulator", name: "Unknown" }, ctx), "unknown");
  assert.equal(relation({ kind: "package", name: "pnpm" }, { kind: "package", name: "pnpm@9" }, ctx), "contains");
  assert.equal(relation({ kind: "package", name: "@me/app@0.3.2" }, { kind: "package", name: "@me/app", version: "0.3.2" }, ctx), "same");
  assert.equal(relation({ kind: "image", name: "ghcr.io/me/app" }, { kind: "image", name: "ghcr.io/me/app:1" }, ctx), "contains");
});

test("entities (property): same is symmetric and transitive, contains antisymmetric and transitive", () => {
  const pool = Array.from({ length: 160 }, entity);
  for (const a of pool) {
    assert.equal(relation(a, a, ctx), relation(a, a, ctx));
    for (const b of pool) {
      const ab = relation(a, b, ctx);
      const ba = relation(b, a, ctx);
      if (ab === "same") assert.equal(ba, "same", `${JSON.stringify(a)} ${JSON.stringify(b)}`);
      if (ab === "contains") assert.equal(ba, "contained", `${JSON.stringify(a)} ${JSON.stringify(b)}`);
      if (ab === "unrelated") assert.equal(ba, "unrelated", `${JSON.stringify(a)} ${JSON.stringify(b)}`);
    }
  }
  let chains = 0;
  for (let i = 0; i < 40_000; i++) {
    const [a, b, c] = [pick(pool), pick(pool), pick(pool)];
    const ab = relation(a, b, ctx);
    const bc = relation(b, c, ctx);
    // Transitive, or unknown where one side knows less (a pull request without its head): never a contradiction.
    if (ab === "same" && bc === "same") assert.ok(["same", "unknown"].includes(relation(a, c, ctx)), `${JSON.stringify([a, b, c])}`);
    if ((ab === "contains" || ab === "same") && (bc === "contains" || bc === "same") && (ab === "contains" || bc === "contains")) {
      chains++;
      assert.ok(["contains", "unknown"].includes(relation(a, c, ctx)), `${JSON.stringify([a, b, c])}`);
    }
  }
  assert.ok(chains > 20, `the generator makes containment chains (${chains})`);
});

test("entities (property): qualifying a name with the current repository changes nothing", () => {
  const pool = Array.from({ length: 200 }, entity);
  const inRepo = new Set(["branch", "pr", "tag", "release"]);
  for (const a of pool) {
    if (!inRepo.has(a.kind) || a.repo || (a.kind === "branch" && /^upstream\//.test(a.name))) continue;
    const qualified = { ...a, repo: "me/app" };
    for (const b of pool) assert.equal(relation(qualified, b, ctx), relation(a, b, ctx), `${JSON.stringify(a)} ${JSON.stringify(b)}`);
  }
});

test("entities (property): a path outside a folder is never in it; unrelated never covers; names name what holds", () => {
  for (let i = 0; i < 500; i++) {
    const dir = `/${word()}/${word()}`;
    const inside = `${dir}/${word()}${rand() < 0.5 ? `/${word()}` : ""}`;
    const outside = `${dir}${pick(["x", "-1", ".bak"])}/${word()}`;
    assert.equal(covers({ kind: "path", name: dir }, { kind: "path", name: inside }), true, `${dir} ${inside}`);
    assert.equal(covers({ kind: "path", name: dir }, { kind: "path", name: outside }), false, `${dir} ${outside}`);
    assert.equal(covers({ kind: "path", name: inside }, { kind: "path", name: dir }), false);
  }
  const pool = Array.from({ length: 150 }, entity);
  for (const a of pool) {
    for (const b of pool) {
      const how = relation(b, a, ctx);
      if (how === "unrelated" || how === "unknown" || how === "contains") assert.equal(covers(a, b, ctx), false, `${JSON.stringify(a)} ${JSON.stringify(b)}`);
    }
    // Each name a message may use for it names it or something holding it
    // (a path's bare folder names aside: they are mentions, not paths).
    for (const name of namesOf(a, ctx)) {
      if ((a.kind === "path" || a.kind === "worktree") && !name.includes("/") && !name.startsWith("~")) continue;
      const named = a.kind === "pr" ? { kind: "pr", name } : { ...canonical(a, ctx), name, version: undefined, udid: undefined, remote: undefined };
      if (a.kind === "branch") Object.assign(named, { name });
      assert.equal(covers(named, a, ctx), true, `${name} for ${JSON.stringify(a)}`);
    }
  }
});

// --- The tool table -------------------------------------------------------------------

test("tools (property): every row's generated command line reads back as its entities", () => {
  const values = { pr: () => String(1 + Math.floor(rand() * 500)), release: () => `v${Math.floor(rand() * 9)}.${Math.floor(rand() * 9)}.0`, repo: () => `${word()}/${word()}`, package: () => word(), image: () => `ghcr.io/${word()}:${Math.floor(rand() * 9)}`, container: word, simulator: () => pick(["11111111-2222-3333-4444-555555555555", "iPhone 15"]), resource: word, worktree: () => `/w/${word()}`, path: () => `/w/${word()}`, branch: () => `feat/${word()}`, tag: () => `v${Math.floor(rand() * 9)}.1.0`, process: () => String(100 + Math.floor(rand() * 90000)), host: word };
  let rows = 0;
  for (const row of TOOLS) {
    if (row.reader || !row.operands?.length) continue;
    rows++;
    for (let i = 0; i < 20; i++) {
      const count = row.rest ? 1 + Math.floor(rand() * 3) : row.operands.length;
      const names = Array.from({ length: count }, (_, index) => values[row.operands[Math.min(index, row.operands.length - 1)]]());
      const typed = row.typed ? ["deployment"] : [];
      const flags = rand() < 0.5 ? ["--yes"] : [];
      const program = pick(row.programs);
      const words = [program, ...row.sub, ...(row.when ? [row.when[0]] : []), ...flags, ...typed, ...names];
      const read = readTool(words);
      assert.ok(read && !("unknown" in read), `${words.join(" ")}: ${JSON.stringify(read)}`);
      assert.equal(read.action, row.action);
      assert.deepEqual(read.entities.map((e) => e.name), names, words.join(" "));
      assert.ok(read.entities.every((e, index) => e.kind === row.operands[Math.min(index, row.operands.length - 1)]));
    }
    // An operand known only when it runs: never read as one.
    const unknown = readTool([row.programs[0], ...row.sub, ...(row.when ? [row.when[0]] : []), UNKNOWN]);
    assert.ok(unknown === undefined || "unknown" in unknown || unknown.entities.every((e) => e.name !== UNKNOWN));
  }
  assert.ok(rows > 30, `rows with operands (${rows})`);
  // Options carrying fields.
  const create = readTool(["gh", "pr", "create", "-R", "me/app", "--base", "dev", "--head", "fix", "--title", "x"]);
  assert.deepEqual(create.entities, [{ kind: "pr", name: "", repo: "me/app", base: "dev", head: "fix" }]);
  const scaled = readTool(["kubectl", "delete", "deployment/web", "-n", "prod"]);
  assert.deepEqual(scaled.entities, [{ kind: "resource", name: "web", type: "deployment", scope: "prod" }]);
  assert.equal(readTool(["vercel", "deploy"]), undefined, "a tool no row covers");
});

// --- Consent sources ---------------------------------------------------------------------

const facts = {
  git: () => ({ current: "fix-badge", defaults: ["main"], isBranch: (name) => /^(main|fix-badge|feature|fix-x|(relay|badge)-\d+)$/.test(name) }),
  tagExists: () => false,
  readFile: () => undefined,
};
const explicitFacts = { originRepo: () => "me/app", release: () => ({}), home: "/home/me" };
const targetsOf = (line) => lineTargets(line.map((words) => ({ words, cwd: "/w/app", danger: true })), facts);
const words = (...messages) => ({ messages, agentWritten: false });

test("consent: the engine's kinds parse; unknown kinds and unanchored ones are left out", () => {
  const parsed = parseOwnerWords(JSON.stringify({
    messages: [
      { text: "fix it", source: "typed" },
      { text: "做", source: "reply", proposal: "I'll open a PR into main and merge it." },
      { text: "做", source: "reply" },
      { text: "Create a private repo", source: "answer", question: "How?\nOptions: Create a private repo | Create a public repo", at: 5 },
      { text: "orphan answer", source: "answer" },
      { text: "Ship\n1. Open a PR", source: "card" },
      { text: "from the future", source: "telepathy" },
      { text: "legacy" },
    ],
    agentWritten: false,
  }));
  assert.deepEqual(parsed.messages.map((m) => m.source), ["typed", "reply", "typed", "answer", "card", "typed"]);
  assert.equal(parsed.messages[3].at, 5);
  // An answer's text for the check: the question and the option picked, never one passed over.
  const answer = parsed.messages[3];
  assert.match(consentText(answer), /Create a private repo/);
  assert.doesNotMatch(consentText(answer), /public/);
});

test("consent: a reply binds the proposal right before it; the code checks the quote is there", () => {
  const read = targetsOf([["gh", "pr", "create", "--base", "main", "--fill"]]);
  const owner = words({ text: "look at the flaky test", source: "typed" }, { text: "做", source: "reply", proposal: "Fixed. Next I'd open a PR into main for review." });
  const claim = { quote: "open a PR into main", source: "reply", targets: [{ kind: "branch", value: "main" }] };
  assert.equal(checkExplicit(claim, read, owner, [], explicitFacts).ok, true);
  assert.equal(checkExplicit(claim, read, owner, [], explicitFacts).quoteSource, "reply");
  // The quote must be in that reply's proposal.
  assert.match(checkExplicit({ ...claim, quote: "open a PR into dev" }, read, owner, [], explicitFacts).reason, /isn't in your/);
  // A reply that says to wait agrees to nothing, whatever the model reads into it.
  const waits = words({ text: "先别，等等", source: "reply", proposal: "Fixed. Next I'd open a PR into main for review." });
  assert.equal(checkExplicit(claim, read, waits, [], explicitFacts).ok, false);
  // A later message about other work moves on.
  const moved = words(...owner.messages, { text: "now look at the release notes formatting instead", source: "typed" });
  assert.equal(checkExplicit(claim, read, moved, [], explicitFacts).ok, false);
  // An old engine sends only typed and plan: the same request still runs on typed words.
  const typed = words({ text: "open a PR into main please", source: "typed" });
  assert.equal(checkExplicit({ quote: "open a PR into main", targets: [{ kind: "pr", value: "fix-badge → main" }] }, read, typed, [], explicitFacts).ok, true);
});

test("consent: an answer counts for the option picked, never another", () => {
  const read = targetsOf([["gh", "repo", "create", "me/tool", "--private"]]);
  const owner = words({ text: "make a repo for the tool", source: "typed" }, { text: "Create the private repo me/tool", source: "answer", question: "Where should it live?\nOptions: Create the private repo me/tool | Create the public repo me/tool" });
  const ok = checkExplicit({ quote: "Create the private repo me/tool", source: "answer", targets: [{ kind: "repo", value: "me/tool" }] }, read, owner, [], explicitFacts);
  assert.equal(ok.ok, true, ok.reason);
  const passedOver = checkExplicit({ quote: "Create the public repo me/tool", source: "answer", targets: [{ kind: "repo", value: "me/tool" }] }, read, owner, [], explicitFacts);
  assert.equal(passedOver.ok, false);
  // Another repository than the one named: no.
  const other = targetsOf([["gh", "repo", "create", "me/other", "--private"]]);
  assert.equal(checkExplicit({ quote: "Create the private repo me/tool", source: "answer", targets: [{ kind: "repo", value: "me/other" }] }, other, owner, [], explicitFacts).ok, false);
});

test("consent: an approved card names its targets; a branch identifies the pull request from it", () => {
  const read = targetsOf([["gh", "pr", "merge", "52", "--squash"]]);
  const card = words({ text: "Ship the badge fix\n1. Open a PR from fix-badge into main\nthen merge it once CI is green", source: "card" });
  const opened = [
    { type: "message", message: { role: "assistant", content: [{ type: "toolCall", id: "c1", arguments: { command: "gh pr create -H fix-badge -B main --fill" } }] } },
    { type: "message", message: { role: "toolResult", toolCallId: "c1", content: [{ type: "text", text: "https://github.com/me/app/pull/52" }] } },
  ];
  const claim = { quote: "merge it once CI is green", source: "card", targets: [{ kind: "branch", value: "fix-badge" }] };
  const got = checkExplicit(claim, read, card, opened, explicitFacts);
  assert.equal(got.ok, true, got.reason);
  // Without knowing that pull request's head, a branch names nothing it acts on.
  assert.equal(checkExplicit(claim, read, card, [], explicitFacts).ok, false);
  // "merge fix-badge into main" covers merging the pull request from it.
  const typed = words({ text: "merge fix-badge into main", source: "typed" });
  assert.equal(checkExplicit({ quote: "merge fix-badge into main", targets: [{ kind: "pr", value: "52" }] }, read, typed, opened, explicitFacts).ok, true);
  assert.equal(checkExplicit({ quote: "merge other-branch into main", targets: [{ kind: "pr", value: "52" }] }, read, words({ text: "merge other-branch into main", source: "typed" }), opened, explicitFacts).ok, false);
});

test("consent (property): a target nobody named never passes, whatever the source", () => {
  const sources = ["typed", "plan", "card", "reply", "answer"];
  for (let i = 0; i < 200; i++) {
    const named = `relay-${Math.floor(rand() * 1000)}`;
    const acted = `badge-${Math.floor(rand() * 1000)}`;
    const source = pick(sources);
    const text = `push ${named} to origin`;
    const word = source === "reply" ? { text: "好", source, proposal: text } : source === "answer" ? { text: `Yes, ${text}`, source, question: `Push?\nOptions: Yes, ${text} | No` } : { text, source };
    const read = lineTargets([{ words: ["git", "push", "origin", acted], cwd: "/w/app", danger: true }], facts);
    const got = checkExplicit({ quote: text, source, targets: [{ kind: "branch", value: acted }] }, read, words(word), [], explicitFacts);
    assert.equal(got.ok, false, `${source}: ${got.reason}`);
    const right = lineTargets([{ words: ["git", "push", "origin", named], cwd: "/w/app", danger: true }], facts);
    const yes = checkExplicit({ quote: text, source, targets: [{ kind: "branch", value: `origin/${named}` }] }, right, words(word), [], explicitFacts);
    assert.equal(yes.ok, true, `${source}: ${yes.reason}`);
  }
});

test("consent: the judge sees each kind, and names the one it relies on", () => {
  const prompt = judgePrompt({
    capability: "escalate", paths: [], zone: "outside the sandbox", command: "gh pr merge 52", level: "standard", workspaceRoots: ["/w"], tier: "danger", userMessages: [], recentToolCalls: [],
    ownerWords: words({ text: "fix it", source: "typed" }, { text: "做", source: "reply", proposal: "Shall I merge PR 52?" }, { text: "Private", source: "answer", question: "Visibility?\nOptions: Private | Public" }, { text: "Plan\n1. Merge", source: "card" }),
  });
  assert.match(prompt, /1\. fix it/);
  assert.match(prompt, /\[reply\] Right after the agent wrote:\n {3}> Shall I merge PR 52\?\n {3}the owner replied: 做/);
  assert.match(prompt, /\[answer\] The agent asked:/);
  assert.match(prompt, /\[card\]/);
  const verdict = parseVerdict('{"decision":"approve-once","reason":"r","consent":{"source":"reply","quote":"merge PR 52","assents":true},"target":{"kind":"pr","value":"52"}}');
  assert.deepEqual([verdict.quote, verdict.source, verdict.targets], ["merge PR 52", "reply", [{ kind: "pr", value: "52" }]]);
  assert.equal(parseVerdict('{"decision":"approve","reason":"r","consent":{"source":"reply","quote":"merge PR 52"}}').source, "reply-unconfirmed", "a reply the model doesn't say assents");
  assert.equal(parseVerdict('{"decision":"approve","reason":"r","userQuote":"merge PR 52"}').quote, "merge PR 52", "the old field still reads");
  assert.ok(ENTITY_KINDS.includes("simulator"));
});

// --- Round 2: languages, attributes, qualified names -------------------------------

import { LANGUAGES, uncoveredScript } from "./cypher-permissions/lexicon.ts";

const bare = (word) => word.replace(/\*$/, "");
const UNSPACED_LANGS = new Set(["ja", "ko"]);
const merge52 = () => targetsOf([["gh", "pr", "merge", "52", "--merge"]]);
const claim52 = (quote, source) => ({ quote, ...(source ? { source } : {}), targets: [{ kind: "pr", value: "52" }] });

test("lexicon: every language's own merge words ask for a merge; its negations and retractions take it back", () => {
  let languages = 0;
  for (const language of LANGUAGES) {
    const verbs = (language.actions.merge ?? []).map(bare).filter((word) => word.length >= 2);
    if (!verbs.length) continue;
    languages++;
    for (const verb of verbs) {
      const text = UNSPACED_LANGS.has(language.code) ? `PR #52 ${verb}` : `${verb} PR #52`;
      const got = checkExplicit(claim52(text), merge52(), words({ text, source: "typed" }), [], explicitFacts);
      assert.equal(got.ok, true, `${language.code} "${text}": ${got.reason}`);
      // A later retraction, in the same language, about it.
      for (const back of language.retract.map(bare)) {
        const later = UNSPACED_LANGS.has(language.code) || language.code === "zh" ? `PR #52 ${back}` : `${back} PR #52`;
        const after = checkExplicit(claim52(text), merge52(), words({ text, source: "typed" }, { text: later, source: "typed" }), [], explicitFacts);
        assert.equal(after.ok, false, `${language.code} later "${later}"`);
      }
    }
    // The message's own "don't", next to the verb.
    for (const not of language.negate.map(bare)) {
      const verb = bare(language.actions.merge[0]);
      const text = UNSPACED_LANGS.has(language.code) ? `PR #52 ${verb}${not}` : `${not} ${verb} PR #52`;
      const got = checkExplicit(claim52(text), merge52(), words({ text, source: "typed" }), [], explicitFacts);
      assert.equal(got.ok, false, `${language.code} "${text}" says not to`);
    }
  }
  assert.ok(languages >= 10, `languages with merge words (${languages})`);
});

test("lexicon: a reply's quote counts from the proposal; found only in the reply it is typed words, checked as such", () => {
  const read = targetsOf([["gh", "pr", "merge", "52", "--merge"]]);
  const proposal = "The fix is in. Next I'd merge PR #52.";
  const agrees = checkExplicit(claim52("merge PR #52", "reply"), read, words({ text: "ok", source: "reply", proposal }), [], explicitFacts);
  assert.deepEqual([agrees.ok, agrees.quoteSource], [true, "reply"], agrees.reason);
  for (const language of LANGUAGES) {
    for (const back of language.retract.map(bare)) {
      const reply = `${back} merge PR #52`;
      // The quote sits in the reply's own words: typed, so its own "wait"/"not yet" counts.
      const got = checkExplicit(claim52("merge PR #52", "reply"), read, words({ text: reply, source: "reply", proposal: "Tests pass." }), [], explicitFacts);
      assert.equal(got.ok, false, `${language.code} "${reply}": ${got.reason}`);
      const typed = checkExplicit(claim52("merge PR #52"), read, words({ text: reply, source: "reply", proposal: "Tests pass." }), [], explicitFacts);
      assert.equal(typed.ok, false, `${language.code} "${reply}" as typed`);
      // Agreeing to the proposal with a "wait": no.
      const waits = checkExplicit(claim52("merge PR #52", "reply"), read, words({ text: back, source: "reply", proposal }), [], explicitFacts);
      assert.equal(waits.ok, false, `${language.code} reply "${back}"`);
    }
  }
  const plain = checkExplicit(claim52("merge PR #52", "reply"), read, words({ text: "merge PR #52", source: "reply", proposal: "Tests pass." }), [], explicitFacts);
  assert.deepEqual([plain.ok, plain.quoteSource], [true, "typed"], plain.reason);
});

test("lexicon: words in a script no row covers ask, and say why", () => {
  assert.equal(uncoveredScript("merge PR 52 please"), undefined);
  assert.equal(uncoveredScript("合并 PR 52，マージ，병합，слей"), undefined);
  assert.equal(uncoveredScript("ادمج PR 52"), "Arabic");
  const read = merge52();
  const arabic = checkExplicit(claim52("ادمج merge PR #52"), read, words({ text: "ادمج merge PR #52", source: "typed" }), [], explicitFacts);
  assert.match(arabic.reason, /Arabic script/);
  const later = checkExplicit(claim52("merge PR #52"), read, words({ text: "merge PR #52", source: "typed" }, { text: "รอก่อน PR #52", source: "typed" }), [], explicitFacts);
  assert.match(later.reason, /Thai script/);
});

const branchName = () =>
  pick([
    () => `${pick(["fix", "feat", "hotfix", "cypher"])}${pick(["/", "-"])}${word()}-${Math.floor(rand() * 100)}`,
    // Bare names a role marker marks: no slash, no digit.
    () => `${pick(["feat", "fix", "hotfix", "topic"])}-${pick(["a", "b", "x", "badge", "relay"])}`,
    () => pick(["develop", "dev", "staging", "trunk"]),
  ])();
const repoName = () => `${pick(["acme", "Owner", "me"])}/${pick(["app", "web", "infra"])}${Math.floor(rand() * 10)}`;

test("attributes (property): a pull request whose base, head or repository differs from the one the consent names never passes", () => {
  let checked = 0;
  for (let i = 0; i < 300; i++) {
    const [head, base, otherBase, otherHead] = [branchName(), branchName(), branchName(), branchName()];
    const repo = repoName().toLowerCase();
    const other = `${pick(["x", "y", "z"])}corp/${pick(["lib", "api"])}`;
    if (new Set([head, base, otherBase, otherHead]).size < 4) continue;
    // Bare names are not known to the repository: only the role marker makes them branches.
    const known = new Set([head, base, otherBase, otherHead].filter((name) => /[/\d]/.test(name)));
    const f = { git: () => ({ current: head, defaults: ["main"], isBranch: (name) => known.has(name) }), tagExists: () => false, readFile: () => undefined };
    const ef = { originRepo: () => repo, release: () => ({}), home: "/home/me" };
    const read = lineTargets([{ words: ["gh", "pr", "create", "--base", base, "--head", head, "-R", repo, "--title", "x"], cwd: "/w/app", danger: true }], f);
    const languages = LANGUAGES.filter((language) => language.actions.create?.length && (language.baseBefore?.length || language.baseAfter?.length) && (language.headBefore?.length || language.headAfter?.length));
    const language = pick(languages);
    const verb = bare(language.actions.create[0]);
    const say = (from, into, inRepo) => {
      // A repository named as one: by the language's word for it, or by its URL.
      const noun = language.nouns.repo?.[0] ? bare(language.nouns.repo[0]) : undefined;
      const where = inRepo ? (noun && rand() < 0.7 ? ` ${noun} ${inRepo}` : ` https://github.com/${inRepo}`) : "";
      if (language.baseAfter?.length) return `PR${where} ${from}${language.headAfter[0]} ${into}${language.baseAfter[0]} ${verb}`;
      return `${verb} PR${where} ${language.headBefore[0]} ${from} ${language.baseBefore[0]} ${into}`;
    };
    const claim = (text) => ({ quote: text, targets: [{ kind: "pr", value: `${head} → ${base}` }] });
    const right = say(head, base, rand() < 0.5 ? repo : undefined);
    const ok = checkExplicit(claim(right), read, words({ text: right, source: pick(["typed", "plan", "card"]) }), [], ef);
    assert.equal(ok.ok, true, `${language.code} "${right}": ${ok.reason}`);
    for (const wrong of [say(head, otherBase), say(otherHead, base), say(head, base, other)]) {
      const got = checkExplicit(claim(wrong), read, words({ text: wrong, source: pick(["typed", "plan", "card"]) }), [], ef);
      assert.equal(got.ok, false, `${language.code} "${wrong}" for ${head} → ${base} in ${repo}`);
      checked++;
    }
  }
  assert.ok(checked >= 600, `${checked}`);
});

test("repositories (property): qualifying, unqualifying or URL-ifying a target in the current repository changes nothing", () => {
  const owner = pick(["GeoffreyChen777", "Owner", "acme"]);
  const name = pick(["cypher", "app"]);
  const cases = (s) => [s, s.toLowerCase(), s.toUpperCase(), s.replace(/./g, (c) => (rand() < 0.5 ? c.toUpperCase() : c.toLowerCase()))];
  const origins = [`https://github.com/${owner}/${name}.git`, `git@github.com:${owner}/${name}.git`, `${owner}/${name}`, `${owner.toLowerCase()}/${name}`];
  for (let i = 0; i < 200; i++) {
    const n = String(1 + Math.floor(rand() * 400));
    const qualified = pick(cases(`${owner}/${name}`));
    const ef = { originRepo: () => pick(origins), release: () => ({}), home: "/home/me" };
    const line = pick([
      ["gh", "pr", "merge", n, "--merge"],
      ["gh", "pr", "merge", n, "--repo", qualified, "--merge"],
      ["gh", "pr", "merge", `https://github.com/${qualified}/pull/${n}`, "--squash"],
      ["glab", "mr", "merge", n, "-R", qualified],
    ]);
    const read = targetsOf([line]);
    const claimValue = pick([n, `#${n}`, `${qualified}#${n}`, `https://github.com/${qualified}/pull/${n}`]);
    const consent = pick([`merge PR #${n}`, `merge ${pick(cases(`${owner}/${name}`))}#${n}`, `merge https://github.com/${qualified}/pull/${n}`, `PR ${n} 没问题合并`]);
    const got = checkExplicit({ quote: consent, targets: [{ kind: "pr", value: claimValue }] }, read, words({ text: consent, source: "typed" }), [], ef);
    assert.equal(got.ok, true, `${line.join(" ")} / ${claimValue} / "${consent}": ${got.reason}`);
    // Another repository on the line against another named in the consent: never, whatever the origin says.
    const theirs = pick(cases("acme/infra"));
    const named = pick([`merge #${n} in acme/web`, `merge acme/web#${n}`, `merge https://github.com/acme/web/pull/${n}`, `PR ${n} 在 acme/web 合并`]);
    for (const origin of [undefined, "acme/web", `${owner}/${name}`]) {
      const facts2 = { ...ef, originRepo: () => origin };
      const line2 = pick([["gh", "pr", "merge", n, "--repo", theirs, "--merge"], ["gh", "pr", "merge", `https://github.com/${theirs}/pull/${n}`], ["glab", "mr", "merge", n, "-R", theirs]]);
      const got2 = checkExplicit({ quote: named, targets: [{ kind: "pr", value: n }] }, targetsOf([line2]), words({ text: named, source: "typed" }), [], facts2);
      assert.equal(got2.ok, false, `${line2.join(" ")} / "${named}" / origin ${origin}`);
    }
    // The same in another repository: never.
    const elsewhere = targetsOf([["gh", "pr", "merge", n, "--repo", "other/elsewhere", "--merge"]]);
    assert.equal(checkExplicit({ quote: consent, targets: [{ kind: "pr", value: claimValue }] }, elsewhere, words({ text: consent, source: "typed" }), [], ef).ok, false, `other repo, "${consent}"`);
  }
  // Branches and tags too.
  const ef = { originRepo: () => `https://github.com/${owner}/${name}`, release: () => ({}), home: "/home/me" };
  for (const value of ["fix-x", "origin/fix-x", "refs/heads/fix-x"]) {
    const read = targetsOf([["git", "push", "origin", "fix-x"]]);
    assert.equal(checkExplicit({ quote: "push fix-x to origin", targets: [{ kind: "branch", value }] }, read, words({ text: "push fix-x to origin", source: "typed" }), [], ef).ok, true, value);
  }
  for (const repo of [undefined, `${owner}/${name}`, `${owner.toUpperCase()}/${name}`]) {
    const read = targetsOf([["gh", "release", "create", "v1.2.3", ...(repo ? ["--repo", repo] : []), "--notes", "x"]]);
    const got = checkExplicit({ quote: "release v1.2.3", targets: [{ kind: "tag", value: "v1.2.3" }] }, read, words({ text: "release v1.2.3", source: "typed" }), [], ef);
    assert.equal(got.ok, true, `${repo}: ${got.reason}`);
  }
});

test("tools: glab merge requests read like gh pull requests", () => {
  const create = targetsOf([["glab", "mr", "create", "--target-branch", "release/2.0", "--source-branch", "fix-x", "--title", "t"]]);
  assert.deepEqual(create.targets.map((t) => [t.kind, t.action, t.head, t.base]), [["pr", "create", "fix-x", "release/2.0"]]);
  const merge = targetsOf([["glab", "mr", "merge", "https://gitlab.com/me/app/-/merge_requests/9"]]);
  assert.deepEqual(merge.targets.map((t) => [t.value, t.repo]), [["9", "me/app"]]);
  const words1 = words({ text: "open an MR from fix-x into release/2.0", source: "typed" });
  assert.equal(checkExplicit({ quote: "open an MR from fix-x into release/2.0", targets: [{ kind: "pr", value: "fix-x → release/2.0" }] }, create, words1, [], explicitFacts).ok, true);
  const intoMain = targetsOf([["glab", "mr", "create", "-b", "main", "-s", "fix-x", "-t", "t"]]);
  assert.equal(checkExplicit({ quote: "open an MR from fix-x into release/2.0", targets: [{ kind: "branch", value: "main" }] }, intoMain, words1, [], explicitFacts).ok, false, "another base");
});

test("attributes (property): ordinary words from every language around a consent never change its outcome", () => {
  const prose = [...new Set(LANGUAGES.flatMap((language) => Object.entries(language.nouns).filter(([kind]) => !["pr", "tag", "version", "branch"].includes(kind)).flatMap(([, list]) => list)).map(bare))]
    .filter((w) => w.length >= 2);
  const adjectives = prose.flatMap((a) => [`${a}-only`, `${a}-based`, `${a}-side`]).slice(0, 400);
  const cases = [
    [["gh", "pr", "merge", "52", "--merge"], "merge PR #52", { kind: "pr", value: "52" }],
    [["gh", "pr", "create", "--base", "main", "--fill"], "open a PR into main", { kind: "pr", value: "fix-badge → main" }],
    [["git", "push", "origin", "fix-badge"], "push fix-badge", { kind: "branch", value: "fix-badge" }],
  ];
  for (const [line, quote, target] of cases) {
    const read = targetsOf([line]);
    const base = checkExplicit({ quote, targets: [target] }, read, words({ text: quote, source: "typed" }), [], explicitFacts).ok;
    assert.equal(base, true, quote);
    for (let i = 0; i < 150; i++) {
      const pad = () => Array.from({ length: 1 + Math.floor(rand() * 3) }, () => pick(rand() < 0.5 ? prose : adjectives)).join(" ");
      const text = `${pad()} ${quote} ${pick(["", "into", "from", "to", "in", "给", "に"])} ${pad()}`;
      const got = checkExplicit({ quote, targets: [target] }, read, words({ text, source: "typed" }), [], explicitFacts);
      assert.equal(got.ok, base, `"${text}": ${got.reason}`);
    }
  }
});

test("replies (property): an unmarked reply binds iff the judge says it assents; a marked one never binds", () => {
  const read = targetsOf([["gh", "pr", "merge", "12", "--merge"]]);
  const proposal = "Checks are green. Next I'd merge PR #12.";
  const claim = (source = "reply") => ({ quote: "merge PR #12", source, targets: [{ kind: "pr", value: "12" }] });
  const reply = (text, source = "reply") => checkExplicit(claim(source), read, words({ text, source: "reply", proposal }), [], explicitFacts);
  // Unmarked replies: generated from every row's carry-on and filler words, and from words of no table at all.
  const unmarked = [...new Set([...LANGUAGES.flatMap((row) => [...row.carryOn, ...row.filler]).map(bare), "按照建议", "go ahead with that", "sounds good, do it", "thanks, looks good", "zzz qux"])]
    .filter((text) => !/[?？¿؟]/.test(text));
  let bound = 0;
  for (const text of unmarked) {
    const marked = /\d/.test(text);
    const got = reply(text);
    if (got.ok) bound++;
    // Binds only when the judge says it assents, never otherwise.
    assert.equal(reply(text, "reply-unconfirmed").ok, false, `"${text}" unconfirmed`);
    if (!marked && got.ok === false) assert.match(got.reason ?? "", /doesn't plainly agree|says not|wrote later|answer was/, `"${text}": ${got.reason}`);
  }
  assert.ok(bound > unmarked.length / 2, `most unmarked replies bind on the judge's word (${bound}/${unmarked.length})`);
  for (const text of ["按照建议", "go ahead", "sounds good, do it"]) assert.equal(reply(text).ok, true, text);
  // Longer than an assent, a reply must cite the proposal's action: whatever the model reads into it, these bind to nothing.
  for (const text of ["go ahead with that", "thanks, looks good", "danke, sieht gut aus", "ログを見せて", "show me the logs first"]) assert.equal(reply(text).ok, false, text);
  for (const text of ["yes, merge it now", "合并吧，没问题的话", "ok merge it once it's green"]) assert.equal(reply(text).ok, true, text);
  // Marked replies never bind, whatever the judge says.
  let checked = 0;
  const bases = ["ok", "好", "はい", "네", "ja", "oui", "sí", "sim", "sì", "да", pick(unmarked)];
  for (const base of bases) {
    for (const mark of ["?", "？", "¿", "؟"]) {
      assert.equal(reply(`${base}${mark}`).ok, false, `"${base}${mark}"`);
      checked++;
    }
    for (const other of ["#13", "fix-other", "v2.0", "PR 13"]) {
      assert.equal(reply(`${base} ${other}`).ok, false, `"${base} ${other}"`);
      checked++;
    }
    for (const defer of LANGUAGES.flatMap((row) => [...(row.defer ?? []), ...row.retract, ...row.negate]).map(bare)) {
      assert.equal(reply(`${base} ${defer}`).ok, false, `"${base} ${defer}"`);
      checked++;
    }
    for (const language of LANGUAGES) {
      for (const action of ["delete", "release", "close", "push"]) {
        for (const verb of (language.actions[action] ?? []).map(bare).filter((v) => v.length >= 2)) {
          assert.equal(reply(`${base} ${verb}`).ok, false, `${language.code} "${base} ${verb}"`);
          checked++;
        }
      }
    }
  }
  // A reply asking about the proposal in its own words asks for nothing, as a reply or as typed words.
  for (const mark of ["?", "？", "¿", "؟"]) {
    const asks = `${pick(["so", "and", "when"])} merge PR #12 ${pick(["now", "today", ""])}${mark}`;
    for (const source of ["reply", "typed", undefined]) assert.equal(checkExplicit({ ...claim(), source }, read, words({ text: asks, source: "reply", proposal }), [], explicitFacts).ok, false, `${source} "${asks}"`);
  }
  assert.ok(checked > 1000, `${checked}`);
});

// --- Round 2, part F: the task's own targets --------------------------------------------------

const ownFacts = (own) => ({ ...explicitFacts, own: () => own });
const ONE = { branches: ["fix-badge"], prs: [{ number: "52", head: "fix-badge", base: "main" }] };
const typedSays = (text) => words({ text, source: "typed" });
const MERGE52 = [["gh", "pr", "merge", "52", "--merge"]];
const PUSH_OWN = [["git", "push", "-u", "origin", "fix-badge"]];
const DELETE_OWN = [["git", "push", "origin", "--delete", "fix-badge"]];
const ownCheck = (text, line, target, own = ONE) => checkExplicit({ quote: text, targets: [target] }, targetsOf(line), typedSays(text), [], ownFacts(own));

test("own targets: an action with no target binds to the task's one branch or pull request", () => {
  assert.equal(ownCheck("ci 没问题就merge", MERGE52, { kind: "pr", value: "52" }).ok, true);
  assert.equal(ownCheck("merge it into main once the checks pass", MERGE52, { kind: "pr", value: "52" }).ok, true);
  assert.equal(ownCheck("commit，然后pr 给main", PUSH_OWN, { kind: "branch", value: "fix-badge" }).ok, true, "a PR asked for covers pushing its head");
  assert.equal(ownCheck("然后删除 remote branch", DELETE_OWN, { kind: "branch", value: "fix-badge" }).ok, true);
  // A conditional clause ("if it's unclear, ask me") is no deferral of what the words ask for.
  const shortcut = "Create a PR to main from the branch we just finished in this chat, and merge it if the checks pass and nothing looks wrong. If it's unclear which branch I mean, ask me.";
  assert.equal(ownCheck(shortcut, MERGE52, { kind: "pr", value: "52" }).ok, true);
  assert.equal(ownCheck(shortcut, PUSH_OWN, { kind: "branch", value: "fix-badge" }).ok, true);
  // Without own facts the same words name nothing.
  assert.equal(checkExplicit({ quote: "ci 没问题就merge", targets: [{ kind: "pr", value: "52" }] }, targetsOf(MERGE52), typedSays("ci 没问题就merge"), [], explicitFacts).ok, false);
});

test("own targets: several or none, another name in the words, or a default branch as what is merged still ask", () => {
  const two = { branches: ["fix-badge", "fix-other"], prs: [{ number: "52" }, { number: "53" }] };
  assert.equal(ownCheck("ci 没问题就merge", MERGE52, { kind: "pr", value: "52" }, two).ok, false);
  assert.equal(ownCheck("pr 给main", PUSH_OWN, { kind: "branch", value: "fix-badge" }, two).ok, false);
  assert.equal(ownCheck("ci 没问题就merge", MERGE52, { kind: "pr", value: "52" }, { branches: [], prs: [] }).ok, false);
  for (const text of ["merge other-branch into main", "merge #53 when green", "merge main into your branch", "merge it, then release 0.3.5"]) assert.equal(ownCheck(text, MERGE52, { kind: "pr", value: "52" }).ok, false, text);
  // A PR request never pushes a default branch, and a branch the task doesn't own isn't implied.
  assert.equal(ownCheck("pr 给main", [["git", "push", "origin", "HEAD:main"]], { kind: "branch", value: "main" }).ok, false);
  assert.equal(ownCheck("pr 给main", [["git", "push", "origin", "fix-x"]], { kind: "branch", value: "fix-x" }).ok, false);
  // "push to main" sends main, not the task's branch.
  assert.equal(ownCheck("push it to main", PUSH_OWN, { kind: "branch", value: "fix-badge" }).ok, false);
});

test("own targets (property): an entity-shaped name the task doesn't own, anywhere in the words, blocks the implicit binding", () => {
  const others = ["fix-other", "#53", "pr 53", "feat/login", "v2.3.0", "relay-7", "other_branch", "0.3.5"];
  const bases = [
    ["ci 没问题就merge", MERGE52, { kind: "pr", value: "52" }],
    ["pr 给main", PUSH_OWN, { kind: "branch", value: "fix-badge" }],
    ["然后删除 remote branch", DELETE_OWN, { kind: "branch", value: "fix-badge" }],
    ["merge it once green", MERGE52, { kind: "pr", value: "52" }],
  ];
  for (const [text, line, target] of bases) assert.equal(ownCheck(text, line, target).ok, true, text);
  for (let i = 0; i < 300; i++) {
    const [text, line, target] = pick(bases);
    const other = pick(others);
    const said = rand() < 0.5 ? `${other} ${text}` : `${text} ${pick(["", "and", "然后", "then"])} ${other}`;
    const got = ownCheck(said, line, target);
    assert.equal(got.ok, false, `"${said}" bound to the task's own ${target.value}`);
  }
});

test("lexicon: a Latin word next to CJK stands as a word; inside a Latin word it doesn't", async () => {
  const { actionWords, nounWords } = await import("./cypher-permissions/lexicon.ts");
  for (const text of ["然后pr 给main", "pr给main", "就merge", "ci 没问题就merge了"]) assert.equal(nounWords("pr").test(text) || actionWords("merge").test(text), true, text);
  for (const text of ["premerge", "sprint", "emergency", "approve"]) assert.equal(actionWords("merge").test(text), false, text);
  assert.equal(nounWords("pr").test("sprint"), false);
});

test("attributes: a count or a measure is never a role-marked branch", () => {
  const read = targetsOf([["gh", "pr", "create", "--base", "main", "--head", "fix-badge", "--fill"]]);
  for (const text of ["Open a PR into main; rerun flaky checks up to 2x", "PR to main, wait up to 30s between polls", "PR 给 main，最多重试到 3x"]) {
    const got = checkExplicit({ quote: text, targets: [{ kind: "pr", value: "fix-badge → main" }] }, read, typedSays(text), [], explicitFacts);
    assert.equal(got.ok, true, `${text}: ${got.reason}`);
  }
  const got = checkExplicit({ quote: "Open a PR into release-2", targets: [{ kind: "pr", value: "fix-badge → main" }] }, read, typedSays("Open a PR into release-2"), [], explicitFacts);
  assert.equal(got.ok, false, "a real ref in the base role still conflicts");
});

test("env: a location variable is a benign prefix only when it names a read-only system directory", async () => {
  const { benignEnv, setSystemDirectory } = await import("./cypher-permissions/commands.ts");
  const before = setSystemDirectory((path) => path.startsWith("/Applications/"));
  try {
    assert.equal(benignEnv("DEVELOPER_DIR", "/Applications/Xcode.app/Contents/Developer"), true);
    for (const value of [undefined, "/tmp/xcode", "./Xcode", "/Applications/../tmp", "/Applications/X*", "$HOME/x"]) assert.equal(benignEnv("DEVELOPER_DIR", value), false, String(value));
    // Variables that are no location stay as they were, whatever the value.
    assert.equal(benignEnv("PATH", "/Applications/Xcode.app"), false);
    assert.equal(benignEnv("LD_PRELOAD", "/Applications/x.dylib"), false);
    assert.equal(benignEnv("RUST_LOG", "debug"), true);
  } finally {
    setSystemDirectory(before);
  }
});

// --- After blind set #1 (round 2, step 3): attributes, code nouns, remote effects, per-command consent ------

const hotfixFacts = { ...facts, git: () => ({ current: "hotfix", defaults: ["main"], isBranch: (name) => /^(main|hotfix|fix-badge)$/.test(name) }) };
const hotfixTargets = (line) => lineTargets(line.map((words) => ({ words, cwd: "/w/app", danger: true })), hotfixFacts);
const opened88 = [
  { type: "message", message: { role: "assistant", content: [{ type: "toolCall", id: "c1", arguments: { command: "gh pr create -H fix-badge -B main --fill" } }] } },
  { type: "message", message: { role: "toolResult", toolCallId: "c1", content: [{ type: "text", text: "https://github.com/me/app/pull/88" }] } },
];

test("attributes: force and a protection bypass must be named by the consent itself (ATTRIBUTE_FLAGS + lexicon attributeWords)", () => {
  const forced = hotfixTargets([["git", "push", "--force", "origin", "HEAD:hotfix"]]);
  assert.deepEqual(forced.attributes, ["force"]);
  const said = "push the hotfix branch";
  const no = checkExplicit({ quote: said, targets: [{ kind: "branch", value: "hotfix" }] }, forced, words({ text: said, source: "typed" }), [], explicitFacts);
  assert.equal(no.ok, false);
  assert.match(no.reason, /forces it/);
  for (const text of ["force-push the hotfix branch", "hotfix 分支强推一下", "hotfix ブランチを強制 push して", "push --force auf den Branch hotfix"]) {
    const yes = checkExplicit({ quote: text, targets: [{ kind: "branch", value: "hotfix" }] }, forced, words({ text, source: "typed" }), [], explicitFacts);
    assert.equal(yes.ok, true, `${text}: ${yes.reason}`);
  }
  assert.deepEqual(hotfixTargets([["git", "push", "origin", "+hotfix"]]).attributes, ["force"]);
  const admin = hotfixTargets([["gh", "pr", "merge", "88", "--merge", "--admin"]]);
  assert.deepEqual(admin.attributes, ["bypass"]);
  const asked = "pr 给main 没问题就合并";
  const merge = checkExplicit({ quote: asked, targets: [{ kind: "pr", value: "88" }] }, admin, words({ text: asked, source: "typed" }), opened88, explicitFacts);
  assert.equal(merge.ok, false);
  assert.match(merge.reason, /bypasses a protection/);
  const plain = checkExplicit({ quote: asked, targets: [{ kind: "pr", value: "88" }] }, hotfixTargets([["gh", "pr", "merge", "88", "--merge"]]), words({ text: asked, source: "typed" }), opened88, explicitFacts);
  assert.equal(plain.attributes, undefined);
  const byName = "用管理员权限绕过保护把 #88 合并了";
  assert.equal(checkExplicit({ quote: byName, targets: [{ kind: "pr", value: "88" }] }, admin, words({ text: byName, source: "typed" }), opened88, explicitFacts).ok, true);
});

test(`attributes, property (seed ${process.env.PERMISSIONS_SEED ?? 20261122}): a forced push passes only when the consent names force, whatever the flag spelling`, () => {
  for (let i = 0; i < 120; i++) {
    const flag = pick(["-f", "--force", "--force-with-lease", "--force-with-lease=hotfix", "-uf", "+"]);
    const line = flag === "+" ? ["git", "push", "origin", "+hotfix"] : ["git", "push", flag, "origin", "hotfix"];
    const named = rand() < 0.5;
    const text = named ? pick(["force push hotfix", "hotfix 强推", "push hotfix with --force", "forcer le push de hotfix"]) : pick(["push hotfix", "hotfix 推一下", "pousse la branche hotfix", "push den branch hotfix"]);
    const got = checkExplicit({ quote: text, targets: [{ kind: "branch", value: "hotfix" }] }, hotfixTargets([line]), words({ text, source: "typed" }), [], explicitFacts);
    if (!named) assert.equal(got.ok, false, `${line.join(" ")} / ${text}`);
  }
});

test("refs: a name next to a code noun (function, file, method, class, field…) is no branch target; a ref noun keeps it one", async () => {
  const { refMention } = await import("./cypher-permissions/explicit.ts");
  for (const text of ["update the `hotfix` function to handle null carts and push it", "fix hotfix() and push", "改一下 hotfix 函数然后推上去", "the file release.yml fails, fix it and push", "rename the method hotfix", "die Funktion hotfix anpassen und pushen"]) {
    assert.equal(refMention(text, text.includes("release") ? "release" : "hotfix"), false, text);
  }
  for (const text of ["push the hotfix branch", "push to hotfix", "推到 hotfix 分支", "push hotfix", "merge hotfix into main", "the hotfix branch's function is fine, push it"]) {
    assert.equal(refMention(text, "hotfix"), true, text);
  }
  const forced = hotfixTargets([["git", "push", "origin", "HEAD:hotfix"]]);
  const text = "update the `hotfix` function to handle null carts and push it";
  const got = checkExplicit({ quote: text, targets: [{ kind: "branch", value: "hotfix" }] }, forced, words({ text, source: "typed" }), [], explicitFacts);
  assert.equal(got.ok, false);
});

test("remote effects (tools.ts REMOTE_EFFECTS): naming a host is no consent to deleting, stopping or writing there", async () => {
  const { remoteEffect } = await import("./cypher-permissions/tools.ts");
  assert.equal(remoteEffect(["sudo rm -rf /var/log/nginx/*.gz /var/log/journal/*"]).action, "delete");
  assert.equal(remoteEffect(["sudo", "-u", "www", "systemctl", "restart", "nginx"]).action, "apply");
  assert.equal(remoteEffect(["df -h; du -sh /var/log/* | sort -h"]), undefined);
  assert.equal(remoteEffect(["journalctl --vacuum-size=200M"]).action, "delete");
  assert.equal(remoteEffect(["echo x > /etc/motd"]).action, "write");
  assert.equal(remoteEffect(["ls 2>/dev/null"]), undefined);
  assert.equal(remoteEffect(["bash -lc 'pkill -f worker'"]).action, "stop");
  const line = lineTargets([{ words: ["ssh", "deploy@web-prod.acme.io", "sudo rm -rf /var/log/nginx/*.gz /var/log/journal/*"], cwd: "/w/app", danger: true }], facts);
  const look = "看看 web-prod 磁盘为什么满了";
  const no = checkExplicit({ quote: look, targets: [{ kind: "host", value: "web-prod.acme.io" }] }, line, words({ text: look, source: "typed" }), [], explicitFacts);
  assert.equal(no.ok, false);
  assert.match(no.reason, /would delete/);
  const clean = "web-prod 磁盘满了，把旧的 nginx 日志删掉";
  assert.equal(checkExplicit({ quote: clean, targets: [{ kind: "host", value: "web-prod.acme.io" }] }, line, words({ text: clean, source: "typed" }), [], explicitFacts).ok, true);
  const reads = lineTargets([{ words: ["ssh", "deploy@web-prod.acme.io", "df -h"], cwd: "/w/app", danger: true }], facts);
  assert.equal(checkExplicit({ quote: look, targets: [{ kind: "host", value: "web-prod.acme.io" }] }, reads, words({ text: look, source: "typed" }), [], explicitFacts).ok, true);
});

test("per-command consent: every unreadable danger of a line needs its own target; handing a part to someone else refuses it", () => {
  const line = lineTargets(
    [
      { words: ["gh", "release", "create", "v2.3.0", "--generate-notes"], cwd: "/w/app", danger: true },
      { words: ["npm", "publish"], cwd: "/w/app", danger: true },
    ],
    facts,
  );
  assert.ok("unknown" in line);
  assert.deepEqual(line.unread.map((item) => item.text), ["npm publish"]);
  line.text = "gh release create v2.3.0 --generate-notes && npm publish";
  const said = "GitHub 上发一个 v2.3.0 的 release，npm 那边我自己来";
  for (const source of ["typed", undefined]) {
    const got = checkExplicit({ quote: said, ...(source ? { source } : {}), targets: [{ kind: "tag", value: "v2.3.0" }] }, line, words({ text: said, source: "typed" }), [], explicitFacts);
    assert.equal(got.ok, false, got.reason);
  }
  // Even without the refusal, the publish brings no target of its own.
  const both = "release v2.3.0 on GitHub and publish it";
  const got = checkExplicit({ quote: both, source: "typed", targets: [{ kind: "tag", value: "v2.3.0" }] }, line, words({ text: both, source: "typed" }), [], explicitFacts);
  assert.equal(got.ok, false);
  assert.match(got.reason, /npm publish/);
});

test(`delegation (lexicon DELEGATE, every language): "X I'll do myself" / "someone else will" takes X back`, async () => {
  const { listWords } = await import("./cypher-permissions/lexicon.ts");
  const retract = listWords("retract");
  for (const text of ["npm 那边我自己来", "I'll publish it myself", "someone else will deploy", "npm は自分でやります", "npm는 제가 직접 할게요", "npm mach ich selbst", "je m'en occupe pour npm", "npm lo hago yo", "npm eu mesmo faço", "npm lo faccio io", "npm я сам сделаю", "npm doe ik zelf", "npm sam zrobię", "npm'i ben yaparım"]) {
    assert.ok(retract.test(text), text);
  }
  const read = merge52();
  const text = "merge #52, I'll do the release myself";
  // The refusal is about the release, not the merge: the merge still passes (concerns, per sentence).
  const later = checkExplicit(claim52("merge #52"), read, words({ text: "merge #52", source: "typed" }, { text: "PR 52 我自己来合并", source: "typed" }), [], explicitFacts);
  assert.equal(later.ok, false, "a later 'I'll merge it myself' takes the merge back");
  assert.ok(text);
});

test("per-command consent: a claimed new pull request names its head, so pushing that head is its own command's target", () => {
  const line = lineTargets(
    [
      { words: ["git", "push", "-u", "origin", "cypher/unknown-here"], cwd: "/w/app", danger: true },
      { words: ["gh", "pr", "create", "-H", "cypher/unknown-here", "-B", "main", "--fill"], cwd: "/w/app", danger: true },
    ],
    facts,
  );
  assert.ok("unknown" in line);
  line.text = "git push -u origin cypher/unknown-here && gh pr create -H cypher/unknown-here -B main --fill";
  const said = "push cypher/unknown-here and open a PR to main";
  const got = checkExplicit({ quote: said, source: "typed", targets: [{ kind: "pr", value: "cypher/unknown-here → main" }] }, line, words({ text: said, source: "typed" }), [], explicitFacts);
  assert.doesNotMatch(got.reason ?? "", /nothing your words name is what `git push/, got.reason);
  const branch = checkExplicit({ quote: said, source: "typed", targets: [{ kind: "branch", value: "cypher/unknown-here" }] }, line, words({ text: said, source: "typed" }), [], explicitFacts);
  assert.equal(branch.ok, true, branch.reason);
});
