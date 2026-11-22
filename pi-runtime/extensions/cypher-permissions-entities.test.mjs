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
const rand = prng(20261122);
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
    if (ab === "same" && bc === "same") assert.equal(relation(a, c, ctx), "same", `${JSON.stringify([a, b, c])}`);
    if ((ab === "contains" || ab === "same") && (bc === "contains" || bc === "same") && (ab === "contains" || bc === "contains")) {
      chains++;
      assert.equal(relation(a, c, ctx), "contains", `${JSON.stringify([a, b, c])}`);
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
  git: () => ({ current: "fix-badge", defaults: ["main"], isBranch: () => true }),
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
  const verdict = parseVerdict('{"decision":"approve-once","reason":"r","consent":{"source":"reply","quote":"merge PR 52"},"target":{"kind":"pr","value":"52"}}');
  assert.deepEqual([verdict.quote, verdict.source, verdict.targets], ["merge PR 52", "reply", [{ kind: "pr", value: "52" }]]);
  assert.equal(parseVerdict('{"decision":"approve","reason":"r","userQuote":"merge PR 52"}').quote, "merge PR 52", "the old field still reads");
  assert.ok(ENTITY_KINDS.includes("simulator"));
});
