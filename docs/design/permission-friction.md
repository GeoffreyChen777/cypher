# Permission control: ask only about what is dangerous

Status: phases 1 and 2 implemented (2026-10-20, Runtime extension `cypher-permissions`);
phase 3 open. Extends [permission-control.md](permission-control.md). §15 lists where the
implementation differs from this text and what is not done.

## 1. Goal

The user is asked only about operations that are **genuinely dangerous**. Everything else
either runs without a question (it is harmless, or the reviewing model approves it), or
asks **once** and is remembered.

Measured on the author's chats from 2026-10-12 to 2026-10-20 (`permission-audit.jsonl`,
61 Pi sessions):

| | |
|---|---|
| bash calls | 4,803 |
| escalations decided by a human | 212 (4.4 per 100 bash calls) |
| …of which genuinely dangerous (§3, replayed) | ~8 |
| …inspection only (§3, R0) | ~58 |
| …local and reversible (§3, R1) | ~144 |
| time an approved escalation waited | median 39 s, p90 8 min |
| reviewing-model failures that fell through to the user | 43 (expired OAuth, timeouts) |
| file-tool approvals for the chat's own worktree (`self` guard) | 15 + 3 denials |

Target: **under 0.5 human questions per 100 bash calls**, all of them R2 or a first
approval of a new kind of command, with no weaker guarantee than today for secrets,
persistence or Cypher's own data.

The log is one user's week on one repository (a Rust/Swift desktop app with Docker and
iOS checks). It shows *why* questions happen. It does not define the design. Tiers are
defined by an operation's **effect** (§3), and program names only reach the classifier
through data files (§3.3), so a new stack needs catalog entries, not new logic. §14
checks the design against common development setups, and §11's metrics are meant to
be compared across users before defaults are tuned.

## 2. Why the user is asked so often today

1. **Outside the sandbox always means a question.** Approval depends on *where* a command
   runs, not on *what it does*: `ps` and `git push origin main` take the same path.
2. **The sandbox cannot run the normal development loop**, so the agent escalates before
   trying anything (525 of 529 escalations came first, only 4 after a sandbox block):
   - In a git worktree the shared git directory (`<repo>/.git`: objects, refs, the
     worktree's index) is outside the workspace and not writable (`sandbox.ts`
     `compilePolicy`), so `git add`/`commit` fail.
   - Dev-instance data (`~/.cypher-development/...`) and worktrees under
     `~/.cypher-development/wt/` match the `self` pattern `~/.cypher*/**`: every edit
     asks a human, and the dev app cannot write its own data.
   - `/tmp` is writable inside the sandbox but classified *external* by the file tools and
     by the redirect check, so `cmd > /tmp/x.log` can only be approved once.
3. **Approvals often cannot be remembered.** The command reader treats as opaque, or
   refuses to name in a rule, the commands the AGENTS.md dev loop is made of:
   - `NAME=value cmd` (48 once-only approvals),
   - `nohup`, `timeout`, `nice`, `caffeinate` (22),
   - a trailing `&`.
4. **What is remembered is too broad, or useless.** "Always allow" saves a rule for every
   command in the line, labeled with the first task's purpose. Today's file holds
   `ls **`, `ps **`, `pgrep **`, `git push **`, `git tag **`, `gh run **` next to
   `head`, `head -2`, `head -3` and one-off `rm -rf <path>`.
5. **Reviewing-model outages land on the user.** Each request waits for the model's
   timeout, then asks anyway; after repeated failures the user switched `escalate` to `ask`.
6. **Parallel requests ask separately.** Dialogs are serialized, but a queued request is
   not checked again after an earlier dialog in the queue allowed the same pattern, and a
   rule saved by another Pi process is not seen until that process reloads.

## 3. Risk tiers

Every request gets a tier from a **deterministic classifier** (`risk.ts`, new) before any
model or human sees it. The tier decides who approves and how long an approval lasts.
The tiers are properties of the effect, independent of language or tool:

- **R0** changes nothing and moves no data off the machine.
- **R1** changes only things that are this machine's, the user's, and undoable: the
  projects, scratch space, caches, the agent's own processes, a feature branch.
- **R2** is irreversible, reaches other people or shared systems, persists beyond the
  task, or exposes secrets.

| Tier | What it is | Who decides | What an approval leaves |
|---|---|---|---|
| **R0 inspect** | Reads state that the sandbox cannot reach; changes nothing; sends no workspace data anywhere | Nobody: runs at once | — |
| **R1 local** | Changes confined to this machine and easy to undo, inside the user's projects, scratch space, or the agent's own processes | The model; if it cannot approve, the user, **once per kind** | A rule for this project (default) or a chat grant |
| **R2 dangerous** | Hard to undo, reaches other people or systems, persists, or touches secrets | **Always a human** | At most "this task" (until the user's next message) |

A command that matches no catalog entry and no R2 rule is **R1-unknown**: it goes to the
model and is never auto-run without review.

### 3.1 R0: inspection catalog

R0 is a shipped catalog (`commands.ts` `INSPECT`), each entry a pattern **with argument
constraints**. It is not a category rule, and a line runs outside the sandbox only if every
command in it is R0, a filter or inert. Initial entries (core, git and GitHub; the
other packs in §3.3 have the same shape):

- processes: `ps` (no `e`, `-E`, `eww`: those print environments), `pgrep` (no `-l`/`-a`:
  those print command lines), `lsof`
- git, read-only: `log`, `status`, `diff`, `show`, `rev-parse`, `ls-remote`, `fetch`
  (no `--upload-pack`), `cat-file`, `rev-list`, `branch --list`, `tag --list`
- GitHub, read-only: `gh run list|view|watch`, `gh pr list|view|checks`,
  `gh repo view`. **Not** `gh api`, which can write.
- containers and tools: `docker ps|images|system df|context ls|inspect`,
  `colima list|status`, `xcrun simctl list`, `launchctl list|print`, `df`, `du`
- file operands of any R0 command must classify as readable in the sandbox, which keeps
  sensitive paths out

Users may add entries in Settings; projects may not.

### 3.2 R2: danger rules

R2 is a shipped list of `danger` rules (`commands.ts` `DANGER`), checked against every
command in the line, opaque lines included (a word known only at run time counts as a
possible match, as `deny`/`ask` rules do today). A matching command is R2 even if an
`allow` rule covers it: dialog-created rules cannot vouch for R2. A rule the user writes
in Settings with `"acknowledgeDanger": true` can.

| Kind | Examples |
|---|---|
| Publish / share | `git push` to the default branch, any tag, `--force`, `--delete`, `:ref`; `gh release`, `gh pr merge`, `gh repo edit/delete`; `npm/cargo/pnpm publish`; deploy CLIs (`vercel`, `fly deploy`, `kubectl apply/delete`, `terraform apply/destroy`, cloud CLIs' write verbs) |
| Send | mail, chat and webhook CLIs; `curl`/`wget` with a request body or a non-GET method to a host the user did not name |
| Destroy outside scratch | `rm`, `mv`, `rsync --delete`, `truncate` on paths outside the workspace, temp and the agent's own dev data; `docker volume rm`, `docker system prune`; `git reset --hard`, `git clean -fd`, `git checkout -- .` in a checkout that is not the chat's workspace |
| Other people's processes | `kill`/`pkill`/`killall` of a process the agent did not start (§5.4) |
| Persistence and system | `sudo`, `launchctl load/bootstrap`, `crontab`, `defaults write`, `brew install/uninstall`, `npm -g`, writes to protected paths |
| Secrets | `security find-*-password`, `printenv`/`env` dumps outside the sandbox, sensitive-file reads the user did not ask for by name (file tools) |
| Code from the network | `curl … \| sh`, `bash <(curl …)`, `npx`/`uvx` of a package not in the project's lockfile |
| Cypher itself | writes to the running instance's own data and permission files (`self`, human-only, unchanged) |

The model may also put an R1 request in front of the user (§6). It never moves an R2
request to R1.

### 3.3 Catalog packs

R0 entries, R2 rules and the classifier's verb tables (which subcommands of a CLI only
read) ship as **packs**, one data file per ecosystem, in the same pattern syntax as
command rules:

| Pack | R0 examples | R2 examples |
|---|---|---|
| core | `ps`, `lsof`, `df`, `du`, `uname`, `sw_vers` | `sudo`, `crontab`, `defaults write`, `launchctl load`, `systemctl enable` |
| git hosting | `git log/status/fetch …`, `gh`/`glab` `… list/view/checks` | push to default/protected branches, tags, force; `gh`/`glab` `release`, `pr merge`, `repo delete` |
| JavaScript | `npm ls/view/outdated`, `pnpm why` | `npm/pnpm/yarn publish`, `npm -g`, `npx` of a package not in the lockfile |
| Python | `pip list/show`, `uv tree` | `twine upload`, `uv publish`, `pip install --user`/system-wide |
| Rust, Go, JVM, .NET | `cargo tree/metadata`, `go list/env`, `mvn dependency:tree` | `cargo publish`, `mvn deploy`, `gradle publish`, `dotnet nuget push` |
| Apple, Android | `xcrun simctl list`, `xcodebuild -list`, `adb devices` | `xcrun altool/notarytool` upload, `fastlane` release lanes, `adb uninstall` of other apps |
| containers | `docker ps/images/inspect/logs`, `docker compose ps` | `docker system/volume prune`, `volume rm`, `push`, `login` |
| Kubernetes, IaC | `kubectl get/describe/logs`, `helm list`, `terraform validate` | `kubectl apply/delete/scale/exec`, `helm install/upgrade`, `terraform apply/destroy` |
| cloud | `aws … describe-*/list-*/get-*` (not `s3 cp`), `gcloud … list/describe` | create/update/delete verbs, IAM changes, `s3 rm/sync --delete` |

Packs are reviewed like code and versioned with the Runtime. A command no pack knows is
R1-unknown: the model reviews it and the user approves the first one, so a missing
entry costs one question, never safety. Users can add entries in Settings, and the audit
log's most-approved unknown commands (§11) show which entries a pack is missing.

### 3.4 Protected targets

The same command can be harmless against a local database and dangerous against
production. Users mark **protected targets** once, in Settings. A matching command is R2
whatever its verb:

- kube contexts, AWS/GCP profiles or projects, Terraform workspaces;
- ssh hosts;
- git remotes and branches, beyond the default branch;
- database hosts (in a URL or `-h` argument).

Without any marked, the defaults treat the default branch, any `prod*` name, and any
remote host the user has not mentioned in the chat as protected. That last case goes to
the model first, as R1-unknown, which asks the user when the host is new.

## 4. Decision flow

```
request
  ├─ deny rule ................................................ refuse
  ├─ runs in the sandbox, and the sandbox allows it ........... run      (§5: make this the common case)
  ├─ tier R2 ──── task grant for this pattern? ── yes ......... run
  │                └─ no ─────────────────────────────────────── ask human: [Allow for this task] [Deny] [Deny and stop]
  ├─ tier R0 .................................................. run outside, note in result
  └─ tier R1 ─── project rule / chat grant / judge memo? ─ yes  run outside
                 └─ no ─ model ─ approve ─ memo for the chat ─ run
                                 └ ask-user / unavailable ──── ask human: [Always in <project>] [Once] [Deny]
```

Rules and grants are checked again **inside** the serialized dialog queue, so a request
waiting behind a dialog that just allowed its pattern runs without asking (§2.6).

## 5. Make harmless work succeed in the sandbox

The cheapest question is the one never asked. These changes widen the sandbox only to
places the agent can already reach through the file tools without asking.

### 5.1 Git worktrees

`compilePolicy` adds the chat's git directories as writable:

- the worktree's own `<common>/worktrees/<id>/` (index, HEAD, logs),
- `<common>/objects`, `<common>/refs`, `<common>/logs`, `<common>/packed-refs`,
  `<common>/FETCH_HEAD`, `<common>/ORIG_HEAD`.

`config`, `config.worktree`, `hooks/` and `info/` stay protected (deny-write, as today).
This makes `git add`, `commit`, `branch`, `merge` and `worktree add` local operations. Moving
a ref is undoable through the reflog. Pushing is still a network write with credentials:
it runs outside the sandbox and is R1 or R2 by §3.2.

### 5.2 `self` means the running instance only

`DEFAULT_SELF` becomes the **running** instance's data root (from the agent directory /
`CYPHER_DATA_DIR`) and `~/.pi/**`, instead of every `~/.cypher*/**`. A workspace root is
never `self`, wherever it lies. Other Cypher instances' data stops being `self`; their
secrets (`session.json`, `auth.json`, `profiles/`, `orgs/`, `sessions/`) stay
**sensitive** through the existing `~/.cypher*/…` sensitive patterns.

This is the Cypher-specific case of a general need: **an app under development keeps
its data outside the repository** (`~/Library/Application Support/<app>`, `~/.config/<app>`,
an emulator's data directory). The general mechanism is a write rule for that directory:
the first write asks (R1, "Always in `<project>`"), and the rule then also lets the
sandbox write there (rules already become sandbox grants in `compile`). Sensitive and
protected patterns inside it still apply. Cypher's repository ships the rule for its own
dev instances (`~/.cypher-development/local-*`) in its project defaults, as an example of
this mechanism, not a special case in the code. Because a project file may not widen
access, the user confirms it once.

### 5.3 One scratch zone

Temp directories (`tempDirs()`) become a **scratch** zone: readable and writable without
asking, for file tools and redirections alike, exactly as Landstrip already treats them.
Sensitive patterns still apply inside.

### 5.4 Processes the agent started

Every command the agent runs (sandboxed or not) gets `CYPHER_AGENT_CHAT=<chatId>` in its
environment; children inherit it. `kill`, `pkill` and `killall` with **literal** pids or
names are R0 (they stop only what the agent started) when every target process carries
this chat's marker (the extension reads the target's environment with `ps -E` on macOS,
`/proc/<pid>/environ` on Linux, same user). Otherwise the command is R2, and so is a target
whose environment cannot be read (macOS hides it for its own binaries, such as `sleep`).
Pids known only at run time (`kill $(pgrep …)`) are R1-unknown and go to the model. The
agent guideline says: look the pids up first, then kill them by number. Signal 0 (a
liveness check) is R0.

### 5.5 Toolchain locations

Package managers and toolchains write outside the workspace. Each of the following is
writable in the sandbox by default (`writableSupport`):

- caches already listed;
- `~/.rustup/toolchains`, `~/.m2/repository`, `~/.gradle` (caches, wrapper, daemon),
  `~/.nuget/packages`;
- `~/.local/share/pnpm`, `~/.deno`, `~/.bun/install`, `~/.pub-cache`, `~/.cocoapods/repos`;
- `~/Library/Developer/Xcode/DerivedData`, `~/.android/avd`, `~/.expo`;
- virtualenvs outside the project (`~/.virtualenvs`, `~/.local/share/virtualenvs`,
  pyenv/uv-managed Pythons).

Not added: places that hold code run later from `PATH` or at login
(`~/.cargo/bin`, `~/.local/bin`, `~/go/bin`, global npm prefixes, shell rc files).
Installing there is R2 (persistence).

Project-local installs (`npm install`, `uv sync`, `cargo build`, `bundle install
--path vendor`) need no special case: they write the workspace and caches, and their
install scripts run inside the sandbox.

### 5.6 Local services and sockets

- **TCP to localhost** (dev servers, a local Postgres or Redis, a browser driver) works
  in the sandbox today (`localBinding`, open network). Nothing changes.
- **Unix sockets** of user-level local services (Postgres `/tmp/.s.PGSQL.*`, MySQL,
  Redis) become allowable with one R1 approval per socket path; the approval adds it to
  `network.unixSockets`.
- **The Docker socket stays out of the sandbox.** Access to it is root-equivalent on
  Linux and equals host file access on macOS (bind mounts). `docker` and `docker compose`
  run outside the sandbox: R0/R1/R2 by verb (§3.3), with `run` R1 (the model sees its
  mounts and image; mounts of `/`, `~` or sensitive paths, and `--privileged`, are R2).

### 5.7 Later: a relaxed sandbox profile

Builds and tests escalated for PTYs (`openpty`), file watchers (FSEvents) and local Unix
sockets (Docker, colima). If Landstrip can grant those, a **relaxed profile** (the same
read and secret rules, plus PTY, FSEvents and allowed sockets) would run them without
leaving the sandbox. Running the project's code *outside* the sandbox (build scripts,
test harnesses) is the largest remaining risk in §9, so this is worth doing even though
R1 already avoids the questions. It is also what makes Linux dev servers (§14) work. Needs a Landstrip investigation; not in phase 1.

## 6. The reviewing model

- **Scope**: R1 requests only (file writes outside the workspace, network domains, MCP
  tools, R1-unknown commands), and sensitive file reads the user named (as before). R0 never reaches it; R2 skips it and
  goes straight to the user, which saves its 20 s.
- **Input adds** the tier and the reason the classifier gave, so it judges intent ("did
  the user ask for something that needs this?"), not whether `ps` is dangerous.
- **Memo**: an approval is remembered for the chat **per pattern** (judge grants,
  `approvedBy: "judge"`, `scope: "chat"`), so the same kind of command is not reviewed
  again in that chat. The model never creates project or global rules.
- **Speed**: `judge.model` defaults to a fast model of the chat's provider when the
  registry has one (falling back to the chat's model), `timeoutMs` 10 s.
- **Circuit breaker**: an authentication error disables review for the session.
  Two timeouts in a row disable it for 10 minutes. While disabled, requests skip the wait
  and the dialog says once, "Automatic review is unavailable (sign in again)", instead
  of a failure line in every dialog. R1 requests covered by rules are unaffected.

## 7. Human approval

### 7.1 Dialogs by tier

| Tier | Title | Options (default first) |
|---|---|---|
| R1 | *Run this outside the sandbox?* | **Always in `<project>`** · Once · Deny |
| R2 | *Dangerous: `<kind>`* (e.g. "publishes to GitHub") | **Allow for this task** · Deny · Deny and stop |
| self / sensitive file | as today | Allow once · Allow for this chat · Deny · Deny and stop |

"Allow for this chat" is dropped for R1: in the data, users chose *once* (161) or
*always* (19) far more than *chat* (24). A project rule is the useful middle ground. It is
kept for files.

### 7.2 What a rule covers

- **Only the commands that needed to leave**: R0 commands, filters and inert builtins in
  the line never become rules. `ls`, `head` and `ps` no longer accumulate.
- **Project scope**: a rule is stored with the repository identity (the git common dir,
  so all worktrees of a repository share it), not for every directory on the machine:

  ```jsonc
  { "match": ["scripts/dev-app.sh", "**"], "decision": "allow", "project": "/Users/me/Developer/cypher",
    "sha256": "…", "env": ["CYPHER_DEV_INSTANCE"], "label": "Start this worktree's dev app" }
  ```

- **Env prefixes and wrappers** (§8) are part of the pattern, so one approval covers
  `CYPHER_DEV_INSTANCE=x nohup scripts/dev-app.sh … &` every time.
- Labels describe the pattern ("Run cargo tests"), not the first task that created it.

### 7.3 Task grants (R2)

"Allow for this task" grants the R2 patterns in the dialog until the user's next message
(a `task` scope: in memory, cleared on the next user entry). A release (push `main`, then
four tags) is one question instead of five, and nothing dangerous carries over to the
next request.

## 8. Reading more command lines

Changes to `shell.ts` / `commands.ts`, each keeping the current rule: what the reader
cannot see, it treats as unknown or opaque.

1. **Assignments in front of a command** (`A=1 B=x cmd …`) are read when every name is
   *benign*: a literal value, and not a name that changes how programs load or run code
   (`PATH`, `*PATH`, `LD_*`, `DYLD_*`, `*_OPTIONS`, `*OPTS`, `PYTHON*`, `RUBY*`, `PERL*`,
   `NODE_*`, `BASH_ENV`, `ENV`, `IFS`, `HOME`, `XDG_*`, `SHELL`, `EDITOR`, `VISUAL`,
   `*PAGER`, `GIT_*`, `SSH_*`, `*_PROXY`, `*_COMMAND`, `*_WRAPPER`, `RUSTC*`, `CC`, `CXX`,
   `LESSOPEN`, `TMPDIR`). A rule lists the names it allows (`env`). An unlisted benign name
   still matches only an exact `allow`, never widens one.
2. **Argument wrappers**: `nohup`, `time`, `nice [-n N]`, `timeout [opts] DURATION`,
   `caffeinate [-dimsu]`, `env [-i] [benign NAME=v]…` that resolve to system binaries are
   unwrapped: the line is matched on the wrapped program. Wrappers that take a *string*
   (`sh -c`, `eval`, `xargs`, `script`) stay refused.
3. **A trailing `&`** on a single command is read as that command (background jobs
   elsewhere stay opaque).
4. **Redirections into scratch** (§5.3) no longer block a rule.

## 9. Safety invariants

These must hold after every change; each gets a test in `cypher-permissions.test.ts`.

1. Writes to the running instance's own data and permission files are decided by a human
   only (unchanged).
2. Neither the sandbox nor a rule ever hands a command a sensitive path. Sensitive file
   reads go to the model only when the user named the file, otherwise to the user
   (unchanged).
3. The model never creates a project or global rule, and never approves R2.
4. No dialog-created rule covers an R2 command. Only Settings, with an explicit
   acknowledgement, can.
5. A project's `.pi/permissions.json` can add `danger`, `ask` and `deny` rules, never
   `allow` or R0 catalog entries (unchanged in spirit).
6. A program in a writable place runs outside only at its pinned contents (unchanged).
   Wrapped programs (§8.2) are pinned the same way.
7. Opaque lines are never matched by an `allow` rule (unchanged). They can still be R2
   by a possible match.
8. Every decision is audited, with its tier (§11).

**Residual risk, unchanged by this design**: an R1 command run outside the sandbox (a
build, a test, a dev script) executes the project's code with the user's rights. The
model approves the *command*, not what it runs. §5.7 is the mitigation.

## 10. Migrating today's rules

On the first run of the new Runtime, `permissions.json` is backed up
(`permissions.json.bak-<date>`) and each command rule is reclassified:

| Rule today | Becomes |
|---|---|
| covered by R0 (`ls **`, `ps **`, `lsof **`, `pgrep **`, `readlink **`, `git log/status/diff/cat-file/rev-list …`) | removed (no longer needed) |
| a filter as written (`head`, `head -2`, `head -3`) | removed |
| a one-off path (`rm -rf <scratch path>`) | removed |
| R2 (`git push **`) | removed; listed in the notice ("now asks each task") |
| other `allow` rules | kept, scoped to the project they were created in when known, else global |

The user sees one notice listing what changed, and Settings → Permissions shows the
rules with their tier.

## 11. Audit and metrics

New fields on every audit record:
- `tier`;
- `latencyMs`, from request to decision;
- `prompted`: whether a human saw a dialog;
- `onceOnly`: why no rule could be offered;
- `offered`: the patterns shown;
- `choice`: the button pressed, `dismissed` or `timeout`;
- `judge`: approved, asked, or unavailable, and why.

On macOS, where the sandbox gives no denial events, a sandboxed command that failed with
a sandbox-looking error is recorded as `decidedBy: "sandbox"` with `paths: []`.

`/perm-log --stats` prints questions per 100 tool calls, by tier and by reason, for the
chat and for the last 7 days. This is the number §1 targets.

## 12. Phases

| Phase | Contents | Expected effect on the measured week |
|---|---|---|
| 1. No-prompt fixes | §5.1–5.3, §8, judge circuit breaker and timeout (§6), re-check in the dialog queue and config reload on change (§2.6), audit fields (§11) | worktree edits stop asking; git/dev-loop lines become rememberable; the model works again for escalations |
| 2. Tiers | `risk.ts`, catalog packs (§3.3) with protected targets (§3.4), toolchain locations and sockets (§5.5–5.6), tiered dialogs and project rules (§7), task grants, judge memo, process ownership (§5.4), migration (§10), desktop and iOS dialog changes | ~58 R0 questions → 0; ~144 R1 → the first of each kind per project (model-approved when it is up); ~8 R2 remain |
| 3. Relaxed sandbox | §5.7, after a Landstrip investigation | builds and tests stop leaving the sandbox |

Code touched: `shell.ts`, `commands.ts`, `risk.ts` (new), `core.ts`, `config.ts`,
`sandbox.ts`, `judge.ts`, `grants.ts`, `cypher-permissions.ts`; the dialog contract in
`crates/harness/src/pi/wire.rs` and `crates/ui/src/composer/permission_card.rs` plus the
iOS card (tier badge, new options); `permission-control.md` sections 6.3, 7.2–7.5 and 12.

## 13. Open questions

1. **Default branch pushes**: should a push to a *non-default* branch of a repository
   the user owns be R1? Proposed yes; the user may prefer R2 for all pushes.
2. **`gh run rerun/cancel`**: R1, since it is reversible and affects the user's own CI.
   Proposed R1.
3. **Judge model default**: a fast model is cheaper and faster but may judge intent less
   well. Measure approval agreement on the audit log before switching the default.
4. **Task scope end**: "until the user's next message" versus a time limit (for example
   30 minutes) for long unattended turns.

## 14. Scenario coverage

What a typical session in each setup asks the user after phase 2, assuming the model is
available. "First" means once per project, then remembered.

| Setup | Runs without questions | Asks once | Always asks (R2) |
|---|---|---|---|
| Web app (Node, Vite/Next) | install, dev server, tests, lint, build, local git | Playwright browser downloads outside caches, if any | publish, deploy, push to the default branch |
| Python service / data | `uv sync`, pytest, notebooks in the workspace, local DB over TCP | a local DB's Unix socket | migrations against a protected DB, `twine upload` |
| Rust / Go / JVM libraries | build, test, bench, fmt, local git | tests needing a PTY or file watcher (until §5.7) | `cargo publish`, `mvn deploy` |
| iOS / Android app | builds into DerivedData, simulator list | simulator boot/install/launch, the app's data directory | uploads to the stores, signing-identity changes |
| Desktop app with its own data (like Cypher) | build, edits in worktrees | the app's data directory, starting/killing its dev instances | — |
| Containers / compose | `docker ps/logs/inspect` | `docker compose up`, `docker run` with workspace mounts | prune, volume rm, push, privileged runs |
| Infra (Terraform, k8s) | validate, fmt, `kubectl get` on a non-protected context | `terraform plan` (credentials), a new context | apply, destroy, delete, any write to a protected target |
| Remote servers | — | the first command on a new ssh host | commands on protected hosts; destructive remote commands the model cannot clear |
| Monorepo, several roots | as single repo; extra roots via `/perm-root` | — | — |
| Not a git repository | everything in the workspace | — | as above |

Known gaps:

- Windows is unsupported (no Landstrip).
- On Linux, background processes lose file access when their command exits (§11 of
  permission-control.md). Dev servers there should be started outside the sandbox (R1)
  until Landstrip fixes it.
- Remote shells are only as safe as the model's reading of the remote command. A rule
  covers a host, not what runs on it.

## 15. Implementation notes

Code: `risk.ts` (tiers and the checks that need the machine), `packs.ts` (the catalog
packs), `shell.ts` (prefix assignments, `&`), `commands.ts` (`unwrap`, `benignEnv`, rule
fields), `core.ts` (`assess`, `checkCommand`, `requestEscalation`, `elevate`, review pause,
`migrateRules`, process ownership, repository trust), `sandbox.ts` (worktree git
directories), `config.ts` (instance-only `self`, toolchain locations, `protectedTargets`,
`inspect`), `audit.ts` (fields and `stats`), `grants.ts` (`task` scope). The engine keeps
the new rule fields when Settings saves (`crates/engine/src/pi/permissions.rs`); Settings
shows a rule's project; the permission card gives "Once" the O key.

Decided while implementing:

- **Open questions (§13)**: a push to a non-default branch is R1 (Q1). `gh run
  rerun/cancel` is R1 (Q2). The judge keeps the chat's model and its 20 s timeout until
  approval agreement is measured (Q3); the pause (§6) covers outages. A task lasts until
  the user's next message on the branch (Q4).
- **Stopping the agent's own processes is R0**, not R1 (§5.4): the marker proves whose
  they are, and the AGENTS.md dev loop restarts them constantly.
- **Protected writes are R2** for the file tools as for commands (§3.2): the model no longer
  reviews them, and `approval["protected.write"]` can only be `ask` or `deny`. §6's list
  of model-reviewed requests does not include them.
- **`self` (§5.2)** is the running instance's data root (minus `worktrees/`), the
  installed app's `~/.cypher` (minus `worktrees/`) and `~/.pi`. "A workspace root is never
  self" was dropped: a chat opened in an instance's own data directory would otherwise be
  able to edit its policy.
- **Git (§3.1, §5.1)**: a git command runs outside (as an inspection or by a rule) only
  when the repository it works in has its own `.git` directory, is one of the chat's, or
  lives where sandboxed commands cannot write: a `.git` file the agent wrote could point
  git at a config that runs a program. `-c`, `--config-env`, `--git-dir`, `--work-tree`
  and `--exec-path` never run outside without review. In a linked worktree the worktree's
  own `commondir`, `gitdir` and the repository's `info/` are protected too.
- **Lines inside lines**: `bash -c '…'`, `eval`, `watch` and the remote command of `ssh
  host '…'` are read as opaque lines (§8): dangerous if anything in them could be.
- **Task grants** carry the danger they were approved for (`dangers`): the same pattern
  with another danger asks again (approving a push to `main` does not approve `--force`).
  They also cover non-dangerous commands the pattern matches.
- **Migration (§10)** decides each rule by its tier: `git tag **` stays (creating a tag is
  local; pushing one is R2). It runs once (`rulesVersion: 2`), with a backup.

Review fixes (2026-10-21):

- **Redirections are checked on every command**, inert builtins and filters too: `echo x
  >> ~/.zshrc` is R2, never the model's.
- **Repository trust**: a `.git` directory counts only when it is the chat's own or lives
  where sandboxed commands cannot write; a worktree's git directory must point back at it
  (`gitdir`, or a submodule's `core.worktree`) before it gives the checkout a project or
  trust. Submodules' configs and hooks are protected.
- **No build tools in R0** (§3.3's JavaScript, Python, Rust, Go and JVM rows, `xcodebuild`,
  `terraform validate`): each reads project configuration that can name a program it runs,
  and all of them work in the sandbox. R0 refuses options that point a CLI at a config or
  server the line names (`--kubeconfig`, `helm --registry-config`, `aws --endpoint-url`,
  `docker -H/--config`, `systemctl -H`); `docker compose config` (it prints `.env`) and
  `aws` services whose listings carry environment variables (Lambda, ECS, …) are not R0.
- **Commands inside commands**: `ssh [options] host '…'`, `exec`, `command`, `xargs`,
  `script`, `env -S`, wrappers with options `unwrap` does not read, and compound commands in
  opaque lines are read for what they run.
- **Code-running options are R2**: `git -c`/`clone --config` with a key outside a short list
  of harmless ones, `--config-env`, `--exec-path`, `--git-dir`; ssh options `ruleCannotVouch`
  names. `git config` writing a harmless key (`user.email`) is R1.
- **More of §3.2**: `docker system prune`, secret managers (`gpg` secret exports and
  decryption, `sops`, `bw`, `doppler`, `infisical`, `chamber`, `aws-vault`, `git
  credential`), chat CLIs, `osascript` (it drives Mail and Messages), `open` with a link to
  another host, `gh auth status --show-token`, `git reflog expire`, `git gc --prune`,
  `git prune`, history rewrites, `git worktree remove` of a worktree outside the workspace,
  download-then-run through a file (`-o`, `-O`, `>`, `wget`).
- **The model's memo** covers the commands' own categories only, and the prompt lists them;
  patterns the agent proposed are never remembered by it.
- **Protected targets**: `gh`/`glab` are matched by their repository and host options and
  URLs only; the defaults dropped `live` and `production-*` and gained `*-prod-*`.
- The migration keeps rules with `acknowledgeDanger`.

Accepted risks: a process can change between the ownership check and the kill (pid reuse,
a new match for `pkill -f`); `kubectl get configmap` and `describe pod` may show values a
team treats as secret.

Not done:

- §5.2's project-shipped rule for Cypher's dev instances: the general mechanism (a write
  rule kept for the project, which the sandbox also honors) exists; the project file does
  not propose it.
- §5.6's one-approval Unix sockets: sockets are still added in Settings.
- §5.7 (relaxed sandbox, phase 3).
- Settings does not show a rule's tier (§10); it shows its project, variables and pins.
- The judge does not default to a faster model (§6, Q3).

