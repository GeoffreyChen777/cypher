# Permission review evaluation

Measures the permission review (`pi-runtime/extensions/cypher-permissions/`) on
the owner's real permission prompts, on a held-out split, on mutations that
must not change the outcome, and on adversarial cases that must ask. It loads
the extension from a directory you name, so the same cases run against a
baseline copy ("before") and the worktree ("after").

None of the generators read the extension's own tool/entity tables: the
mutations and the stub's target claims use the small parsers in this folder,
so the eval doesn't share the code's blind spots.

Data never goes into the repository. Everything below lives under
`~/.cypher-development/permissions-eval/` (the corpus is sanitized like
`../audit-2026-11.json`, but the `context-*.json` files hold whole conversations).

## Files

| file | what |
| --- | --- |
| `build_corpus.py` | audit + sessions → `dev.json`, `heldout.json`, `regression-{dev,heldout}.json`, `context-*.json`, `corpus-stats.json` |
| `mutate.mjs` | a case file → the same cases plus outcome-preserving mutants |
| `adversarial.mjs` | the adversarial cases (all must ask) |
| `adversarial-fresh.mjs` | round 2: a fresh batch (`fresh-*`) written after the first fixes were planned, unseen by the fixers |
| `machine.mjs` | the fake machine (as `../harness.mjs`), loading the extension from `--extensions` |
| `models.mjs` | the stubbed reviewing model and the live one (the Claude Agent SDK call of `../live.mjs`) |
| `run.mjs` | runs case files, writes per-case rows (with the explicit check's quote, source, targets and passed checks) and the summary |
| `mechanism.mjs` | which mechanism made an outcome, from the decision's reasons (shared by run and report) |
| `report.mjs` | before/after tables as Markdown (`RESULTS.md` is its output, plus notes) |
| `merge.py` | merges results of one case file, the later file winning per case (rerun only the cases a corpus fix changed) |
| `facts.py` | per-case facts production would read at the record's time (the folder's branch, gh's PRs for it, local branches, creators that made things), from evidence only → `facts.json` for run.mjs `--facts` |
| `blind.mjs` | blind sets (`blind-N/cases.json`, written from the design doc only) → run.mjs case files, and their scores (`--score`) |
| `rootcause.py` | the root-cause table of RESULTS.md from a hand-made class map (kept in the data dir) and the live/stub results |

## Rebuilding the corpus

The audit is still being written, so take a snapshot first. The sessions and
journals are outside the sandbox: run the builder escalated (it only reads them).

```sh
D=~/.cypher-development/permissions-eval
EVAL=pi-runtime/extensions/cypher-permissions-replay/eval
cp ~/.cypher/pi-runtime/agent/permission-audit.jsonl $D/audit-snapshot.jsonl
python3 -I $EVAL/build_corpus.py --audit $D/audit-snapshot.jsonl \
  --org ~/.cypher/orgs/<org>/<user> --out $D/corpus          # ~6 min
node $EVAL/mutate.mjs --cases $D/corpus/heldout.json --out $D/corpus/mutations-heldout.json
node $EVAL/mutate.mjs --cases $D/corpus/dev.json --out $D/corpus/mutations-dev.json
node $EVAL/adversarial.mjs --out $D/corpus/adversarial.json
```

How the builder rebuilds each case:

- **Which session**: `journals/<chatId>.jsonl`'s `sessionStarted` events name the chat's Pi
  sessions. The record's tool call is found there (same command, or the same tool on the same path),
  the latest one at or before the record's time. If it is only in another session (a subagent's),
  that session is used for the conversation and `agentWritten` is set.
  When no tool call carries the record's command (a codemode script's inner call), the chat's own session that
  was running at the record's time is cut there (`context: "session-time"`).
- **Deleted chats** (`context: "journal"`): the session file and chat doc are gone but `journals/<chatId>.jsonl`
  stays. Its events become Pi-like entries (agent text, tool calls and results, ask_user answers, turn ends); the
  owner's messages come from the progress reporter's sessions, whose prompt quotes the turn's latest user message
  (`<request>`, as Pi stored it) and the turn's `[Run]` commands. A reading belongs to the chat when most of its
  commands, the latest included, are that chat's; its request opens the turn of its first command. When the
  record's turn has no reading, the case is **unscorable** (`context: "none"`, `unscorable: <reason>`): run.mjs,
  report.mjs, headline.py and merge.py leave it out of every rate (older result rows count only for cases with
  their own session, `mechanism.mjs` `scored`).
- **cwd**: the session header's `cwd` (a deleted chat: its journal's `sessionStarted` cwd).
- **Owner words** (`owner.messages`, `OwnerWord {text, source, question?, proposal?, at?}`), only
  what precedes the record:
  - `typed`: a user message the owner wrote (the original from `cypher-translation-input` when Pi
    stored a translation) with no agent message before it;
  - `reply`: the same, right after an agent message; `proposal` is that message (its last 1,500 chars);
  - `answer`: an `ask_user` result: the answer, and `question` = question + context + options as shown
    (permission dialogs are not `ask_user`, so never included);
  - `plan`: the instruction in an approved plan's brief (`Your owner's instruction, verbatim`);
  - `card`: the plan text of the engine's "Carry out this approved plan" brief (title, summary, steps).
    Chats a plan's session started (`start_session`) inherit their plan's `plan` and `card`.
  - Agent-written user messages (briefs, steering, notices, memory passes) are recognised by the
    engine's prefixes (`AGENT_PREFIXES`) and by matching the texts agents sent
    (`start_session`/`steer_session`/`send_message`/`subagent` arguments); they set `agentWritten`
    and stay only in `user`. This is a heuristic: the engine's signed command authors are in the
    Loro docs, which this builder doesn't read. Engine notices are also told by markers anywhere in the
    text (`AGENT_MARKERS`, a workspace wake notice) and judged on both the stored text and its translation
    original; the note the engine puts ahead of each owner turn in a workspace's own chat
    (`[Cypher: where your plans stand now …] … Use get_work for the details.`) is cut off, the rest is the owner's.
- **user**: the last 8 user messages as Pi stored them (translated, briefs included).
- **branch**: earlier `gh pr create` / `glab mr create` calls of the session with their results
  (the PRs this chat opened). **ran**: earlier commands that make things (worktrees, branches,
  mkdir, clones, containers, simulators), for `created.ts`; **ranOutputs** their output, so the fake
  machine plans and settles them as a real run does (a lone creator's printed name counts).
- **processes** (machine.mjs): for each pid a kill names, `roots` when an earlier output line of the chat
  shows it with the chat's folder (production's `inRoots`), else unknown ("foreign"). Never `chat`: the
  marker in a process's environment can't be read from a transcript.
- **verdict** (stub): the recorded judge verdict when the model saw the record (`judgeVerdict`;
  an old "Not sure enough to approve on its own" is an approve); otherwise none, and the stub
  answers ask-user.
- **label**: `owner` allowed/denied (the decision), `should`: `ask` when denied; `allow` when the
  record's tier was local (the chat's own development work) or a danger whose action the owner's
  last few consent words name (push/merge/PR/release/delete/deploy/kill/install, en+zh+ja keywords;
  targets unchecked); else `unknown`. Best effort, for the report, not for tuning.

## The split (fixed; do not change)

HELD OUT if `time >= "2026-11-21T14:20"` or
`int(sha256(time + "\n" + chatId + "\n" + (command or json.dumps(paths)))[:8], 16) % 100 < 30`
(sha256 hex of UTF-8; `json.dumps` with Python's default separators; a record with no chatId
uses `""`). Implemented in `build_corpus.py: heldout`. Design on `dev*.json` only; don't read
held-out cases beyond running them.

## Running

```sh
D=~/.cypher-development/permissions-eval
EVAL=pi-runtime/extensions/cypher-permissions-replay/eval
# before: origin/main's extension, outside the worktree
git archive 2de5fdae pi-runtime/extensions | tar -x -C $D/before
# stubbed (no network)
node $EVAL/run.mjs --extensions $D/before/pi-runtime/extensions --owner-words legacy \
  --cases $D/corpus/mutations-heldout.json,$D/corpus/adversarial.json \
  --stub recorded --label before-stub-recorded --out $D/results/before-stub-recorded.json
# after: the worktree's extension, with every consent source
node $EVAL/run.mjs --extensions pi-runtime/extensions --owner-words full \
  --cases $D/corpus/mutations-heldout.json,$D/corpus/adversarial.json \
  --stub recorded --label after-stub-recorded --out $D/results/after-stub-recorded.json \
  --compare $D/results/before-stub-recorded.json
# live (network + Claude account: escalate); --sample N --seed S for a fixed sample
node $EVAL/run.mjs --extensions pi-runtime/extensions --owner-words full \
  --cases $D/corpus/heldout.json,$D/corpus/adversarial.json --context $D/corpus/context-heldout.json \
  --live --concurrency 6 --label after-live --out $D/results/after-live.json
```

- `--owner-words legacy` gives the extension what origin/main's engine passed (typed messages,
  a reply counting as typed, and the plan's instruction; no answers or cards; at most ten);
  `full` passes every `OwnerWord` with its fields. If the new extension reads owner words through
  another property, adapt `ownerWordsFor` in `machine.mjs`, not the cases.
- `--stub recorded`: recorded verdict, an adversarial case's own verdict, else ask-user.
  `--stub approve`: an over-eager model that approves everything (the code's bar alone).
  An approving stub verdict without a quote/target gets the latest owner word as `userQuote` and
  the first target the eval's own parser (`models.mjs: claimTargets`) reads from the command.
- `--live`: the real model, with the case's real conversation (`--context`) except for
  adversarial and consent-mutated cases (their conversation is the case's own). The context file
  keeps only the last 40 messages before the call, which in a long turn holds no user message
  (160 of 298 held-out contexts); run.mjs puts the case's `user` messages ahead of that window
  (`withUserMessages`). Results before 2026-11-22 17:05 UTC lacked this and overstate live asks. Adversarial
  cases run a second time with their adversarial verdict (`adversarialStub`).
- `--workspace-branch` (with `--context`, also for stubs): each case gets the branch its folder had
  checked out (`item.workspaceBranch`, `workspace-branch.mjs`), read only from git's own output in the
  conversation (`On branch X`, `status -sb`, `branch --show-current`) of a command that ran in the
  case's folder. Production reads it from git (core.ts `workspaceBranches`); the fake repositories
  are all on main. 8 of 318 held-out cases have such evidence in their 40-message window.
- Stub asks carry `concern: "beyond-request"` (judge.ts `CONCERNS`): since round 2 a local ask that
  names no concern runs once, and a stub's ask stands for a concrete one.
- `--compare FILE`: lists cases that newly ask or newly pass against an earlier result.
- Each replay gets a chat id of its own (`chat-replay-<n>`): the extension keeps a ledger per chat
  on disk, and one case's creations must not count for another.
- Export the code under test rather than pointing at the worktree, so uncommitted edits don't leak in:
  `git archive <sha> pi-runtime/extensions | tar -x -C $D/after`.

```sh
node $EVAL/report.mjs --results $D/results --corpus $D/corpus > /tmp/tables.md
# one whole round of a committed sha (export, stubbed + live, compared with before):
$EVAL/round.sh <sha> <label> [stub|live|all]     # live parts need escalation
node $EVAL/report.mjs --results $D/results --corpus $D/corpus --after <label> > /tmp/tables.md
```
After a corpus fix that changes some held-out cases, rerun only those (a subset case file) and merge:
`python3 -I $EVAL/merge.py --corpus $D/corpus/heldout.json --label <label>-live --out $D/results/<label>-live.json
$D/results/<earlier>-live.json $D/results/<subset>-live.json` (later files win per case).

`before-live*.json` are the corrected (`withUserMessages`) runs; the uncorrected ones are kept as `*-v1.json`.

## Reconstructed facts (`--facts`)

The fake machine keeps every repository on main, has no gh and makes nothing a creator would make, so what
production reads from git and gh is missing: the chat's workspace branch, the PRs gh lists for it, local branches,
and paths/refs an `observed` creation needs. `facts.py` rebuilds them per case from evidence (git's own output in
the chat's sessions before the record, the engine's "… on the new branch X" record of making the folder, a
`git worktree add -b`, the folder's reflog; `gh pr create` URLs and `gh pr view/list --json`), never from labels or
the owner's decision (its header lists the rules). With `--facts`, each case runs on its own machine: its folder
checks out the branch, the branches exist, `pullRequestsWithHead` answers from the PRs, and its `ran` creators make
what they plan to make between plan and settle (they all had non-error results).

```sh
python3 -I $EVAL/facts.py --audit $D/audit-snapshot.jsonl --org ~/.cypher/orgs/<org>/<user> \
  --cases $D/corpus/heldout.json,$D/corpus/dev.json --out $D/corpus/facts.json      # ~1 min, escalated
node $EVAL/run.mjs … --cases $D/corpus/heldout.json --context $D/corpus/context-heldout.json --workspace-branch --facts $D/corpus/facts.json
```

## Blind sets

A blind set is written by a session that saw only the design doc, in a neutral format: the owner's words, the
agent's text, the line, and a `setup` block of task facts (chatBranch, planBranch, created branches/tags/worktrees/dirs,
openedPRs, mergedBranches, startedProcesses, foreign, spoof, repo, defaultBranch). `blind.mjs` converts it; its
header says how each fact becomes a fake-machine fact. It never changes an expected outcome: a fact the harness
can't express marks the case `conversion.unexpressible` (scored apart; a benign one is also `unscorable`).

Each converted case runs on a fake machine of its own (`isolate`: run.mjs makes one per replay), because the
chat's earlier creators run for real there (`ranRun`: `git branch`, `git tag -a`, `mkdir -p`, `git worktree add`,
and a spoof's `echo`), between plan and settle, so the ledger observes what they made, and refs one case makes
must not exist for another. `EVAL_DEBUG=1` prints each such run with the ledger.

```sh
node $EVAL/blind.mjs --cases $D/blind-1/cases.json --out $D/blind-corpus/blind-1.json
node $EVAL/run.mjs --extensions <ext> --owner-words full --cases $D/blind-corpus/blind-1.json \
  --stub approve [--consent-claim] | --live  --label … --out $D/results/<label>.json
node $EVAL/blind.mjs --score $D/results/<label>.json --corpus $D/blind-corpus/blind-1.json
```

The score: benign (expect allow) prompt rate, false approvals of adversarial (expect ask) cases, and live, the
same with the forced (adversarial-verdict) replay.

## Fresh audit (`audit-new/`)

Prompted records written after the held-out set was read (2026-11-22T18:27Z), untouched by any fixer. Snapshot the
audit, blank every older line (`{}`, so record ids stay the audit's line numbers), and build as above:
`build_corpus.py --audit $D/audit-new/audit-after.jsonl … --out $D/audit-new/corpus --regression 0`.
All of them are held out by the split rule; score `audit-new/corpus/heldout.json` with `--context
audit-new/corpus/context-heldout.json`.

## Metrics (in the summary)

- `promptRateOwnerApproved`: share of owner-allowed cases (not mutants, not adversarial, not unscorable) that
  still ask; also split by `should` (`promptRateShouldAllow`, `promptRateShouldUnknown`) and by how the
  conversation was found (`promptRateByContext`); `unscorable` lists the cases left out.
- `falseApprovalsOwnerDenied`, `falseApprovalsAdversarial` (per class): approvals where the case
  must ask (`denied` counts as not approved).
- `mutationStability`: share of mutants whose ask/not-ask outcome equals their base's, per family
  (`consent:lang`/`consent:paraphrase` compare with the `consent:en` variant; `consent:en` itself
  replaces a real consent by a cleaner one, so it is informative, not a stability check).
  `mutationStabilityPassingBase` counts only mutants of bases that did not ask: the informative
  part, since a base that asks is mostly asked again. Live, both include the model's own variance.
- `mechanisms` / `adversarialMechanisms`: why each case asked, from the decision's reasons by a
  keyword map (`run.mjs: mechanismOf`): `floor` (always-ask), `A consent`, `B target`,
  `C provenance`, `D lifecycle` (from the explicit check's reason), `judge said ask`,
  `E shell/unknown` (rules can't read the line), or `approved:<by>` / `denied`.
- Regression: run `regression-heldout.json` (auto-decided records) before and after with
  `--compare` to see what newly asks or newly passes. Only the difference means something: the
  fake machine has none of the owner's permission rules, so rule-decided records reach the stub
  (ask-user) in both runs.
