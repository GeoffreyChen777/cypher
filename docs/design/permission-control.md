# Permission control (cypher-permissions)

Status: **phases 0–4 implemented** (Runtime 1.1.0.2; off by default since 1.1.0.3; §10).

The Runtime extension `pi-runtime/extensions/cypher-permissions.ts` (modules in
`pi-runtime/extensions/cypher-permissions/`) replaces the npm package
`pi-permission-control` 0.2.0, which Cypher used to ship disabled. That package is
dropped from the bundle; the engine prunes its stale `settings.json` entry like any
retired curated package.

**Master switch.** Permission control is off unless the user's `permissions.json` sets
`"enabled": true` (the switch at the top of Settings → Permissions; a project's file
cannot turn it on). The extension reads it when Pi starts, so it applies from the next
run; while off it registers nothing (no tool overrides, no `request_permission`, no
commands) and Pi's own tools run unchecked.

## 1. Goals

Control what an agent session can touch, with a model simple enough to explain in one
table:

- **Files**: which paths a session may read or write.
- **Commands**: every AI-issued shell command runs inside an OS sandbox
  ([Landstrip](https://github.com/landstrip/landstrip): Seatbelt on macOS, Landlock +
  seccomp on Linux) whose policy is compiled from the same rules.
- **Network**: whether a command may reach the network.
- **Elevation**: anything outside the defaults goes through one approval pipeline, whose
  behavior the user chooses: always deny, always ask, or model-assisted.
- **Leaving the sandbox**: a command that needs what the sandbox withholds (credentials,
  keys, other processes, system services) runs outside it, as the user, once the user
  allows it: for that run, or for matching commands from then on (§6.3).

Non-goals are listed in §11. This is not a VM: it protects against model mistakes and
prompt-injection-driven misuse, not against a determined attacker running as the same
user.

## 2. Concepts

| Term | Meaning |
|---|---|
| **Zone** | Classification of a path: `workspace`, `external`, `sensitive` |
| **Guard** | A write rule on top of the zone: `protected` or `self` (§3.4, §3.5) |
| **Capability** | `read`, `write` (write includes read) |
| **Decision** | Per zone × capability: `allow`, `elevate`, `deny` |
| **Approval mode** | How an elevation is decided: `deny`, `ask`, `assisted` |
| **Grant** | A recorded approval: what it covers and for how long (`once`, `chat`, `always`) |
| **Level** | A named preset of decisions, chosen per chat (§5.2) |
| **Command rule** | A pattern of command words: matching commands run outside the sandbox (`allow`), ask first (`ask`) or never run (`deny`) (§6.3) |

## 3. Zones and guards

### 3.1 Classification

Every path is canonicalized first: `~` expanded, a leading `@` stripped, made absolute
against the tool's cwd, `..` resolved, symlinks followed with `realpath` (native, so on
a case-insensitive macOS volume `~/.SSH` becomes `~/.ssh`). A path that does not exist yet
is resolved through its nearest existing ancestor. Then:

```text
sensitive  if it matches a sensitive pattern      (checked first, always wins)
workspace  else if it is under a workspace root
external   otherwise
```

On top of the zone, a path may be write-guarded: `self` is checked before `protected`.
Pattern matching ignores case on macOS.

An explicit exception inside a sensitive directory (`!~/.ssh/config`) splits it: the
sandbox denies the directory's other entries one by one instead of the whole directory.

**Pattern syntax**: `~/x` is in the home directory, `<root>/x` in each workspace root, a
pattern starting with `**` and `/` matches anywhere, `/**` at the end also matches the
directory itself, and a leading `!` is an exception within its list (`!**/.env.example`).

### 3.2 Workspace roots

- The chat's cwd.
- The Git top level of the cwd (`git rev-parse --show-toplevel`), so `cd ..` inside a
  repository is still the workspace.
- Extra roots the user adds for the chat with `/perm-root add <path>` (stored in the
  session, §7.5).

**Writable support paths** are not workspace, but builds break without them. Shell
commands may write them whenever the level lets them write the workspace:

- Temp: `os.tmpdir()`, `$TMPDIR`, `/tmp`, `/private/tmp`, `/var/tmp`, and devices
  (`/dev/null`, `/dev/tty`, …; on macOS also `/dev/stdout`, `/dev/stderr`, `/dev/fd`).
- Toolchain caches (editable): `~/.npm`, `~/.cache`, `~/Library/Caches`,
  `~/.cargo/registry`, `~/.cargo/git`, `~/go/pkg/mod`, `~/.gradle/caches`,
  `~/.pnpm-store`, `~/.bun/install/cache`, `~/.yarn/berry/cache`, and `~/.ssh/known_hosts`.

### 3.3 Sensitive patterns

Defaults (editable, `config.ts` `DEFAULT_SENSITIVE`):

| Group | Patterns |
|---|---|
| Credentials in the home directory | `~/.ssh/**` (except `config`, `known_hosts`, `known_hosts.old` and `*.pub`: not secret), `~/.gnupg/**`, `~/.aws/**`, `~/.azure/**`, `~/.config/gcloud/**`, `~/.kube/**`, `~/.docker/config.json`, `~/.netrc`, `~/.npmrc`, `~/.pypirc`, `~/.config/gh/**`, `~/.git-credentials`, `~/.config/git/credentials`, `~/Library/Keychains/**`, `~/Library/Cookies/**`, browser profiles (Chrome, Brave, Edge, Arc, Firefox, Safari, Chromium) |
| Secrets in any project | `**/.env`, `**/.env.*` (except `.env.example`, `.env.sample`, `.env.template`), `**/*.pem`, `**/*.key`, `**/id_rsa*`, `**/id_ed25519*`, `**/id_ecdsa*`, `**/*.p12`, `**/*.pfx`, `**/*.keystore`, `**/*.jks` |
| Cypher's and Pi's own secrets | `~/.cypher*/session.json`, `profiles/`, `orgs/` (and the same one level down, for development data dirs), `pi-runtime/agent/auth.json`, `mcp-auth.json`, `sessions/` (other chats' transcripts), `~/.pi/agent/auth.json`, `~/.pi/agent/sessions/**` |

### 3.4 Protected paths (write guard, judge may approve)

Some paths are not secret but are dangerous to write: a write there runs code outside the
sandbox later. A Git hook runs when the user commits; `.git/config` can set
`core.fsmonitor` or `core.sshCommand`; shell startup files run in every new terminal.

Protected paths keep their zone (`.git/config` stays workspace: readable, network
unaffected) and add one rule: **writes need elevation, `assisted` by default**. The judge
sees that the path is protected and why, plus the diff for file-tool edits. `git remote
add` after "publish this repo" passes; an unexplained `core.fsmonitor` goes to the human.

Defaults: `**/.git/config`, `**/.git/config.worktree`, `**/.git/hooks/**`,
`<root>/.gitmodules`, `<root>/.envrc`, `<root>/.vscode/tasks.json`, `<root>/.husky/**`,
`~/.ssh/config` (`ProxyCommand`), `~/.gitconfig`, `~/.config/git/**`, the zsh/bash/profile startup files,
`~/.config/fish/**`, `~/Library/LaunchAgents/**`, `~/.config/systemd/user/**`,
`~/.config/autostart/**`.

### 3.5 Self (write guard, only a human approves)

`~/.cypher*/**` and `~/.pi/**`, except `~/.cypher*/worktrees/**` (and one level down for
development data dirs), where chats work. This covers the policy file, the audit log,
Pi's `settings.json` (which lists the extensions, this one included), the Runtime's
binaries and every extension's code.

The shell must still read and execute the Runtime (its `bin/` directories are first on
`PATH`), so self data is readable; only its secrets (§3.3) are not. A write needs a human:
the judge must never approve an edit to its own policy. The list cannot be edited.

## 4. Default permissions

### 4.1 File matrix (level `standard`)

| | read | write |
|---|---|---|
| workspace | allow | allow |
| external | allow | **elevate** (`assisted`) |
| sensitive | **elevate** (`ask`) | **elevate** (`ask`) |
| protected guard | zone's rule | **elevate** (`assisted`) |
| self guard | zone's rule | **elevate** (human only) |

### 4.2 Network

Sandboxed commands never read a secret: a sensitive grant serves the file tools only
(§7.5). So they keep the network. A command that needs a secret and the network (`git
push` over SSH or HTTPS, `gh pr create`, a cloud CLI) runs outside the sandbox instead,
once the user allows it (§6.3).

Other network rules for every sandboxed command:

- Local binding (dev servers, tests on `localhost`): allowed (`network.localBinding`).
- Unix sockets: denied, which covers `docker.sock` (root-equivalent) and `SSH_AUTH_SOCK`.
  Commands that need one (the `docker` CLI, ssh-agent, GPG-signed commits) run outside the
  sandbox when allowed, or list a socket in `network.unixSockets`.
- The ripgrep behind `grep` never has the network.

**Filtered mode** (`network.mode: "filtered"`, Settings → Permissions → Network). Commands
lose direct network access and get `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` pointing at a
per-process Landstrip filter proxy on `127.0.0.1` (credentials in the URL, so only these
commands can use it; `NO_PROXY` covers localhost). The proxy refuses private and loopback
destinations (cloud metadata included). A domain on the allowlist passes: built in are
GitHub, npm, Yarn, PyPI, crates.io, the Rust and Go toolchains, Maven Central, RubyGems and
Docker Hub, plus `network.allowDomains` (`*.x` covers subdomains). Any other domain is a
`network` elevation (`assisted` by default) asked in the running command's context:
"Allow for this command", "Allow for this chat" (a `net:<domain>` grant), "Always allow"
(a `net:<domain>` rule). Programs that ignore the proxy variables (ssh) run outside the
sandbox when allowed (§6.3). A project's file may switch its chats to filtered mode, never
back.

### 4.3 Secrets in the environment

A sandboxed command's environment drops variables matching `*_TOKEN`, `*_SECRET`,
`*_SECRET_KEY`, `*_API_KEY`, `*_APIKEY`, `*_PASSWORD`, `*_ACCESS_KEY`, `*_PRIVATE_KEY`,
`AWS_*`, `GH_TOKEN`, `GITHUB_TOKEN`, `SSH_AUTH_SOCK`, `CYPHER_ENGINE_SOCKET` (editable).
A command that needs one runs outside the sandbox, with the user's whole environment.

## 5. Configuration

### 5.1 Files

`<agent-dir>/permissions.json` (the Pi agent directory, under Cypher's data directory, so
the file is self-guarded). Every field is optional; lists take `{ "add": [], "remove": [] }`
(a bare array adds):

```json
{
  "version": 1,
  "defaultLevel": "standard",
  "levels": { "standard": { "external": { "read": "allow", "write": "elevate" } } },
  "approval": {
    "external.write": "assisted",
    "protected.write": "assisted",
    "sensitive.read": "ask",
    "sensitive.write": "ask",
    "network": "assisted",
    "tools": "ask"
  },
  "judge": { "model": null, "timeoutMs": 20000, "minConfidence": 0.8 },
  "ask": { "timeoutMs": null },
  "sensitive": { "add": [], "remove": [] },
  "protected": { "add": [], "remove": [] },
  "writableSupport": { "add": [], "remove": [] },
  "envScrub": { "add": [], "remove": [] },
  "network": { "mode": "open", "allowDomains": { "add": [] }, "localBinding": true, "unixSockets": [] },
  "tools": { "mcp": "allow", "overrides": { "mcp:github": "elevate" } },
  "rules": []
}
```

- Approval modes are keyed `<zone>.<capability>`, `protected.write`, `self.write`,
  `network`, `tools`. A missing key means `ask`.
- `rules` holds "always allow" grants (§7.5); `tool:<name>` rules allow a tool,
  `net:<domain>` rules a domain.
- `tools.mcp` is the default for MCP and other extension tools; `overrides` set one tool
  (`mcp__github__create_issue`) or one MCP server (`mcp:github`).

**A project's file only tightens.** `<root>/.pi/permissions.json` may add sensitive and
protected patterns (not exceptions), make level decisions and approval modes stricter, and
tighten tool decisions. Nothing else is read: the agent can write the project, and the
project itself may be hostile.

**Safety rules no file can change**: sensitive and self approvals are never `assisted`
(`assisted` reads as `ask`); `self.write` is `ask` unless set to `deny`.

### 5.2 Levels and Cypher's per-chat sandbox setting

Cypher stores a `SandboxLevel` per chat (`crates/proto/src/agent.rs`). The harness passes
it to Pi as `CYPHER_PERMISSION_LEVEL` (`crates/harness/src/pi/spawn.rs`); discovery and
slash-command processes get none and use `defaultLevel`.

| `SandboxLevel` | Shown as | Extension level | Difference from `standard` |
|---|---|---|---|
| `ReadOnly` (`read-only`) | Ask before edits | `read-only` | workspace `write: elevate` |
| `WorkspaceWrite` (`workspace-write`) | Edit workspace (default) | `standard` | §4.1 |
| `DangerFullAccess` (`danger-full-access`) | Full access | `full-access` | no checks: file tools and bash run unsandboxed and unchecked |

The level is picked with **`/permissions`** in the composer's `/` menu: a composer-local
entry leading "Agent modes" for Pi chats, listed whatever Settings → Commands turns on
(not in side chats, which run with their parent's level). Its badge shows the level in
effect (amber at Full access); its choices (strictest first, the one in effect checked)
set the level and type nothing. `/permissions ask|workspace|full` typed in full and sent
applies the same way and never reaches the agent. The composer toolbar shows nothing for
the first two levels; at Full access an amber shield appears, which opens the same
choices. For a new chat the pick rides `createChat` and the first `RunRequest`; for an
existing chat it is written to the row with `setChatConfig`. The engine runs a chat with
its **row's** level whatever the request says (`execute_run` in
`crates/engine/src/host/doc_host/commands.rs`): every client sends a fixed default, older
iOS builds included. Title generation runs `ReadOnly`.

## 6. Enforcement

| Surface | Mechanism | Strength |
|---|---|---|
| `read`, `write`, `edit`, `ls` | Re-registered under their own names, built from Pi's factories with checked filesystem operations (§6.1) | Enforced at each filesystem call, in the Pi process |
| `grep` | Same, and ripgrep runs inside Landstrip (Runtime patch, §6.4) | **OS-enforced** |
| `find` | Search root checked; fd runs as is: file names are not secrets | Policy check |
| `apply_patch` (pi-codex-tools) | Patch headers pre-checked; its file operations go through the same checks (Runtime patch, §6.4) | Enforced at each filesystem call |
| AI `bash` | Landstrip with a policy compiled per command (§6.2) | **OS-enforced**, inherited by every child process |
| Nested calls (codemode) | The same tools | Same |
| MCP and extension tools | Tool-level decision: `allow` (default), `elevate` or `deny` | The tool's internals are not sandboxed |
| Subagents (child chats) | The same extension in the child's Pi; dialogs reach the UI like any child question | Same |
| User `!` / `!!` commands | Not intercepted (the user's own action) | — |
| Pi itself, other extensions, MCP server processes | Not sandboxed | Out of scope (§11) |

### 6.1 File tools

Pi lets an extension register a tool under a built-in's name, and that definition replaces
the built-in. The built-in factories accept filesystem operations (`ReadOperations`,
`WriteOperations`, `EditOperations`, `LsOperations`, `GrepOperations`), so the tools keep
Pi's behavior, rendering and output; only the disk access changes. Three layers:

1. **Authorization before the call.** The tool's target path is classified; an
   elevation runs the approval pipeline (§7) and yields the canonical paths approved for
   this call. A blocked call throws, and the error tells the agent how to elevate.
2. **Checked operations.** Every `readFile`, `writeFile`, `mkdir`, `readdir` and `access`
   re-checks the absolute path it is about to touch, after `realpath`, against the level,
   the call's approvals, the grants and the rules. Paths the tool assembled itself
   (Pi's NFD and curly-quote fallbacks, `mkdir -p` parents) are covered. Against a
   symlink swapped between the check and the open, a file is opened first
   (`O_NOFOLLOW` on the last component) and verified second: the opened file's device and
   inode must equal what is at the checked canonical path, still reached without a
   symlink. Writes open without truncating and truncate through the verified descriptor.
3. **The OS sandbox for ripgrep** (§6.4).

`read`/`write`/`edit`/`ls` run inside Pi's process, so their guarantee rests on this code,
not on the kernel; the agent cannot bypass it because it can only pass tool arguments.

Results of a sensitive read go into the model's context and so to the model provider; the
dialog says so.

### 6.2 Bash

The bash tool is created per call with Pi's `spawnHook`: Pi runs
`exec <landstrip> run -p <policy> -- <shell> -c <command>` (the same shell Pi would use:
`shellPath` or `/bin/bash`; `shellCommandPrefix` stays in front of the command). The
transcript and the UI show the agent's command, not the wrapper. The environment is
scrubbed (§4.3). The policy file lives in a private temp directory and is removed after
the call.

The policy for one command:

```text
allowRead   = ["/"]
denyRead    = sensitive paths (no sensitive grant reaches a command)
allowWrite  = workspace roots + writable support + temp + devices + write grants
              ("/" when the level lets the external zone be written)
denyWrite   = (sensitive + protected + self) paths inside a writable root − write grants
              + every ancestor of those, between the writable root and the path,
                as a single-entry pattern
network     = allowNetwork (filtered mode: only the filter proxy);
              local binding per config; no Unix sockets but those listed
```

Facts measured with the bundled Landstrip 0.18.38 that shaped this:

- **Reads stay open.** A `denyRead` without `allowRead: ["/"]` makes Landstrip enumerate
  every sibling of every denied path at launch; a file the command creates afterwards in
  those directories (a fresh `node_modules`) would be unreadable to it.
- **Exact paths, found by ripgrep.** A glob in the policy is expanded by Landstrip walking
  the tree at every launch: 3.1 s on this repository with a 42 GB `target/`. The extension
  resolves patterns itself (`discover.ts`): `~/x` and `<root>/x` directly, single-component
  globs by reading the directory, and anywhere-patterns with ripgrep's parallel walker
  over the workspace roots on every command (0.1 s for 104k files). The rest of the home
  directory is covered by a cached index (`<agent-dir>/cypher-permissions/home-index.json`,
  6 s to build, refreshed in the background after 10 minutes, skipping caches and
  `node_modules`). Within one turn the result is reused for 1.5 s.
- **Ancestors are protected.** Without that, `mv config config2 && cat config2/.env`,
  `ln config/.env x`, or renaming `.git` and editing a copy of its config moves a
  protected file out from under its rule. A path whose last character is a one-character
  class (`/w/.gi[t]`) becomes an anchored pattern that denies the directory entry, not its
  contents: Git still commits, branches, tags and gcs normally.
- **Git's `config.lock` rename** over a denied `.git/config` is blocked too.
- Writing outside `allowWrite` fails with `EPERM`, including `$TMPDIR` and `/dev/null`
  unless listed. `allowNetwork: false` blocks DNS and connects. A launch costs ~66 ms.

Missing patterns: a protected path that does not exist yet (`<root>/.envrc`) is still
write-denied (creating it is the risk); a missing sensitive path needs no read rule.

### 6.3 Bash denials and running outside the sandbox

macOS Seatbelt is fixed at process start and reports no per-access events, so a command
that needs more than the sandbox gives fails first:

1. The command hits `EPERM`, or fails like a wrong credential (a key or token the sandbox
   hides).
2. If its output looks like the sandbox's doing (`Operation not permitted`, `Permission
   denied`, DNS failures, `Host key verification failed`, `Authentication failed`, `not
   logged in`, the keychain), the tool result gets a hint: what the sandbox blocks, and to
   run the command again with `escalate`. An ordinary failure gets no hint.
3. The agent runs it again with `escalate` (below); the user decides.

On Linux, Landstrip also prints JSON denial events (`FILESYSTEM_DENIED`,
`NETWORK_DENIED`): the extension strips them from the output, lists the blocked paths in
the hint and records them in the audit log.

**Leaving the sandbox.** Many tasks reach something the sandbox cannot express: a server
over SSH (keys, agent socket, known hosts), a Git remote (the keychain's credential
helper), `gh`, cloud CLIs, `launchctl`, `kill` of another process, a simulator. Rather than
widening the sandbox piece by piece, such a command runs **outside it, as the user**. The
extension does not need to understand the program; the user approves the command (and,
optionally, a pattern for similar ones).

The bash tool takes an optional `escalate: { label, reason, patterns? }`. `label` says what
the command is for in plain words, `patterns` are command rules the agent proposes for
later commands. Command rules (`commands.ts`, in `permissions.json`'s `commands`, a
project's `.pi/permissions.json`, or the chat's grants):

```jsonc
{ "match": ["git", "push", "**"], "decision": "allow", "label": "Push to GitHub" }
{ "match": ["git", "push", "**", "--force", "**"], "decision": "ask" }
{ "match": ["kubectl", "delete", "**"], "decision": "deny" }
{ "match": ["./scripts/deploy.sh", "**"], "decision": "allow", "sha256": "9f2c…" }
```

- A pattern is the program, then words: `*` and `?` glob within a word, `**` stands for
  any number of words. The program is literal and matches by name (`git` matches
  `/usr/bin/git`); a pattern naming a path matches that path as written.
- Shells, interpreters and wrappers cannot start a pattern (`sh`, `bash`, `env`, `sudo`,
  `xargs`, `python`, `node`, `npx`, `awk`, `sed`, `find`, …): a rule on them would cover
  any command. `cd`, `echo` and other inert builtins never need to leave.
- **Reading the line** (`shell.ts`): words, quotes, `~`, comments, and `&&`, `||`, `;`,
  `|`, newlines between commands; `2>&1` and redirection to `/dev/null` are dropped.
  Anything else (expansions, substitutions, other redirections, unquoted globs and braces,
  subshells, background jobs, assignments, `if`/`for`/…) makes the line **opaque**: no rule
  matches it, so it runs in the sandbox, or outside once if the user approves it as
  written.
- **Deciding**: every command in the line is checked (inert builtins skipped). A `deny`
  anywhere blocks the line before it runs, escalated or not. If every command matches an
  `allow` and none an `ask`, the line runs outside without asking, with a note in the
  result (`[permissions] Ran outside the sandbox (allowed by …)`). Otherwise it runs in
  the sandbox, unless the agent escalates and the user approves.
- **The program must be the one approved**: it is resolved as bash would (a path against
  the working directory, else `PATH`). A program that does not exist is never trusted. A
  program inside a place sandboxed commands can write (the workspace, temp, the writable
  caches) is trusted only at the contents pinned when it was approved (`sha256`); a
  changed file asks again. The agent cannot change `PATH` in the line (an assignment is
  opaque).
- **The request**: proposed patterns are checked before anyone is asked: each must be
  valid, match a command in the line, and together they must cover every command. The
  dialog reads *Run this command outside the sandbox?* with `Purpose`, `Command`, one
  `Pattern` line each, `Reason`, and notes (it runs as the user; a pinned program; an
  opaque line). Options: Run once, Allow for this chat, Always allow (patterns only),
  Deny, Deny and stop. Always a human (`approval.escalate`: `ask` or `deny`; never the
  judge). Without a UI it is denied.
- **What an approval leaves behind**: Run once, nothing. Allow for this chat, one chat
  grant per pattern (§7.5). Always allow, one `allow` rule per pattern in
  `permissions.json`.
- A project's file may add `ask` and `deny` rules, never `allow`.
- The level still applies inside the sandbox; outside it nothing does. Full access runs
  everything outside without asking.

### 6.4 Runtime patches

- `patches/pi-coding-agent-grep-spawn-hook.mjs` adds a `spawnHook` option to
  `createGrepToolDefinition`. ripgrep reports which file and line matched before Pi reads
  the line through the checked operations, so without the sandbox a pattern tried one
  character at a time would reveal a secret. The extension runs ripgrep inside Landstrip
  (reads only, no network) and also passes exclusion globs for the denied paths (ripgrep
  exits with an error on the first unreadable file). As defense in depth it drops result
  lines from files the agent may not read.
- `patches/pi-codex-tools-permissions.mjs` routes `apply_patch`'s `open`, `writeFile`,
  `mkdir` and `unlink` through a hook the extension installs on
  `globalThis[Symbol.for("cypher.permissions.fs")]`. It also lets pi-codex-tools keep
  hiding `edit`/`write` from grammar-capable OpenAI models: it recognized the built-ins by
  their `builtin:` source, which the replacements no longer have.

### 6.5 Unsupported hosts

The extension runs `landstrip doctor` once per Pi process. If Landstrip is missing or the
OS sandbox does not work (Linux without Landlock: kernels before 5.13), the extension stands
down: tools run as they would without it, and each chat is told once ("Permission control
is unavailable on this device: …"). `/perm` shows the error.

## 7. Elevation

### 7.1 Requests

- **Automatic**: a file tool call whose path needs elevation.
- **`request_permission`**: `{ capability: "read" | "write", paths, reason }`, ahead of
  file tool calls. Not for bash: the dialog says the approval serves the file tools only,
  and the agent's answer says a command that needs the paths runs with `escalate`
  (approving `~/.ssh` this way never let ssh connect, which confused users).
- **`escalate`** on a bash call: run it outside the sandbox (§6.3).
- **Tool-level**: an MCP or extension tool whose decision is `elevate`.

### 7.2 Approval modes

| Mode | Behavior |
|---|---|
| `deny` | Rejected at once; the agent is told the access is not available |
| `ask` | A human decides |
| `assisted` | The judge first. It has two answers: approve automatically (once), or ask the user. It never rejects |

Safety rules:

1. Sensitive and self requests never use the judge.
2. The judge never rejects. Its only answers are `approve` and `ask-user`; anything but a
   confident `approve` (unsure, low confidence, any other word, unreadable output, an
   error, a timeout, no model available) asks the user, and the dialog says *"The
   reviewing model could not approve this automatically: <reason>"*. The reason is also
   recorded in the audit log (`reviewer`).
3. Judge approvals are `once`. Only a human creates `chat` or `always` grants.
4. Running a command outside the sandbox always goes to a human.

When several paths are requested together, the strictest mode wins.

### 7.3 The judge

- **Model**: `judge.model` (`provider/id`), else the chat's current model, through Pi's
  model registry with the user's existing credentials (any provider, including the Claude
  bridge).
- **Trusted input**: the user's last six messages, the request (capability, paths, zone,
  guard and why, tool, command, level, workspace roots).
- **Untrusted input**, fenced in `<untrusted>` (nested tags stripped): the agent's
  justification, its last eight tool calls, and the change for writes.
- **Never sent**: contents of files being read.
- **Output**: `{"decision": "approve" | "ask-user", "reason", "confidence"}`. It approves
  only on `approve` with confidence ≥ `judge.minConfidence` (0.8); a less confident
  `approve` asks the user ("Not sure enough to approve on its own: …"). Timeout 20 s.
- When the judge asks the user, its reason is shown in the dialog.

### 7.4 Human approval

`ctx.ui.select`, which Cypher bridges to a question in the desktop and iOS apps
(`crates/harness/src/pi/session/ui.rs`): the chat shows *Awaiting input* and the push
notification is sent. Dialogs are serialized, so parallel tool calls never stack them.

The dialog names what is asked (`write outside the workspace`, `read a sensitive path`,
`write a protected path`, `write Cypher's own data`), the paths and the matching pattern,
the tool and command, the network effect, the agent's reason, the change, and the judge's
verdict.

| Option | Grant | Offered for |
|---|---|---|
| Allow once (Run once outside the sandbox) | `once` | all |
| Allow for this chat | `chat` | all; for a command, its patterns |
| Always allow | `always` (a rule in `permissions.json`) | file tools, not sensitive, not self; commands with patterns |
| Deny | — | all |
| Deny and stop | — (aborts the turn) | all |

No timeout by default: an unattended run waits for a remote approval. `ask.timeoutMs`
denies after a delay. Without a UI (print mode) a request is denied.

### 7.5 Grants

```ts
interface Grant {
  id: string;
  capability: "read" | "write";   // write implies read
  paths: string[];                // canonical; a directory covers its subtree
  pattern?: string[];             // a command rule: matching commands run outside the sandbox
  label?: string;                 // what the pattern is for
  sha256?: string;                // the pinned program, when it lives in a writable place
  sensitive?: boolean;
  scope: "once" | "chat" | "always";
  approvedBy: "user" | "judge" | "rule";
  createdAt: string;
  chatId?: string;
}
```

- `once`: in memory, consumed by the next matching use. Pi runs one process per turn, and
  a once grant is asked for and used in the same turn.
- `chat`: a `cypher-permissions.grant` custom entry in the Pi session file, tagged with
  `CYPHER_CHAT_ID`. Cypher forks a chat by copying its session file
  (`crates/harness/src/pi/fork.rs`), so entries from another chat id are ignored: a fork
  starts without grants. Revocation appends `cypher-permissions.revoke`.
- `always`: a rule in `permissions.json`.
- A sandboxed command runs with the non-sensitive path grants; a sensitive grant serves
  the file tools only. Grants bound to one command's text (from older Runtimes) are
  dropped when a session is restored.
- Pseudo-paths: `net:<domain>` (the filter proxy), `tool:<name>` (an MCP or extension
  tool). They never become filesystem rules.
- A child chat (subagent) does not inherit its parent's grants.

## 8. Audit and commands

`<agent-dir>/permission-audit.jsonl`, one JSON line per decision that was not a plain
default allow: time, chat id, tool, command, capability, zone, guard, paths, decision,
who decided (`policy`, `grant`, `rule`, `judge`, `user`, `sandbox`), reason, grant id and
scope. Rotated at 10 MB (one previous file kept).

**Settings → Permissions** (desktop, per device; `crates/ui/src/settings/permissions.rs`)
edits the part of `permissions.json` it shows through `GetPiPermissionSettings` /
`SetPiPermissionSettings` (`crates/engine/src/pi/permissions.rs`, forwardable to any
device) and keeps everything else in the file as written. It shows whether the device can
sandbox (the engine runs the Runtime's `landstrip doctor`), the approval mode per request
kind (sensitive rows offer no "Model first"; Cypher's own data is always "Ask me"), the
reviewing model (the chat's model or any catalog model), the MCP/extension tool default
and a per-server override for each configured MCP server, the network mode, local servers,
the extra sensitive / protected / writable / allowed-domain / Unix-socket lists, the
command rules (what runs outside the sandbox, what always asks, what never runs) with
Remove, and the always-allow rules with Remove. Running outside the sandbox is "Ask me" or
"Deny".

**Approval dialogs** ride Cypher's question channel (`ctx.ui.select` → the harness's
`UserInputQuestion` → the composer's question card, iOS's question panel, the push
notification), but are not questions. The title is `question\n\nContext:\n` followed by
one `Label: value` line per fact (`Path`, `Matches`, `Domain`, `Tool`, `Command`,
`Purpose`, `Pattern`, `Reason`, `Note`, `Review`; `Network` from older Runtimes), with a
`Change:` or `Input:` block last; paths show the home directory as `~`. Its last line is for
Cypher only: `cypher-permission: {"kind": "escalate" | "access" | "network" | "tool",
"toolCallId": "…"}`. The harness strips it (`pi/wire.rs` `permission_prompt`) and sets
`UserInputQuestion.permission` (`cypher_proto::PermissionPrompt`): what the prompt is about
and the tool call it gates. The desktop lays a permission prompt out as a permission card
(`crates/ui/src/composer/permission_card.rs`, `question_card.rs`; a prompt from an older
Runtime is recognised by its "Deny and stop" option): the facts as rows, the sentences as
notes, the change in a code block, and the options as buttons weighted by what they grant
(the narrowest approval filled, broader ones outlined, Deny quiet, Deny and stop in red).
Other clients show the same text as a headline over a context block.

**In the transcript** a permission prompt never reads as a question. Each tool result
carries how permission control treated it, as a `cypherPermissions` record in its details
(`{ outcome: "outside" | "blocked" | "denied" | "allowed", scope?: "once" | "chat" |
"always" | "rule", rule?, purpose? }`), which the harness folds into the tool part's
`permission` (`cypher_proto::ToolPermission`) and strips the `[permissions]` notes meant
for the model from the shown output. The desktop (`transcript/rows.rs`
`PermissionFold`):

- files an answered prompt under the call it gated (`toolCallId`): no row of its own, so
  it does not split the work run;
- folds a command the sandbox blocked into the run of the same command that followed
  (outside the sandbox, or refused): one row, the attempt's output first in its detail;
- puts a badge on the call's row ("Outside the sandbox · this chat", "Blocked by the
  sandbox", "Denied", "Allowed for this chat"; outside in the warning tint), and shows a
  blocked or denied call calm, with a muted cross, not as a failure;
- counts them in the run's summary ("Ran 3 commands · 2 outside the sandbox · 1 denied");
- shows a prompt it cannot file under a call as a "Permission" row with a shield, not a
  "Question".

A call the extension keeps no record of (an MCP tool) that a prompt gated shows
"Allowed" or "Denied" by whether it ran. The new fields are additive and older readers
ignore them.

**iOS** does the same: the transcript (`Transcript/TranscriptRows.swift`
`PermissionFold`, `PermissionBadge` with the same shield icons), a permission card in
place of the question panel (`Composer/PermissionPanel.swift`: the facts as rows, one tap
answers), `/permissions` leading Agent modes in the `/` menu with the amber shield beside
the model chip at Full access, and Permissions in the account menu
(`Permissions/PermissionSettingsView.swift`): a device's settings through the relay's
`GetPiPermissionSettings` / `SetPiPermissionSettings`, kept as raw JSON so a save never
drops a field the phone does not know. The tool rows carry no output on iOS, so a folded
attempt shows only in its badge and the summary.

**Shortcuts** (desktop permission card; letters shown as keycaps on the buttons): `Y` the
narrowest approval (Allow once / Run once / Allow for this command), `C`
for this chat, `A` always (when offered), `N` deny, `S` deny and stop, `⌘↵` the narrowest
approval, `esc` dismiss (no approval), and number keys by position. Plain `↵` approves
nothing. The card takes keyboard focus wherever focus lands on its composer (opening, chat
switches), never from another chat or a terminal; keys within 500 ms of the card appearing
and held-key repeats are ignored, so typing into the composer as a request arrives never
answers it.

Commands:

- `/perm`: level, sandbox, workspace roots, defaults, approval modes, grants, command
  rules, warnings.
- `/perm-grants`: list; `/perm-grants revoke <id>`; `/perm-grants clear`.
- `/perm-root add|remove <path>`: extra workspace roots for this chat. A slash command is
  typed by the user, so it is itself the approval.
- `/perm-log`: this chat's recent decisions.

## 9. Code map

| File | Role |
|---|---|
| `extensions/cypher-permissions.ts` | Pi wiring: tool re-registration, `request_permission`, events, the apply_patch hook, commands |
| `cypher-permissions/core.ts` | State, the approval pipeline, checked operations, launch plans |
| `cypher-permissions/config.ts` | Defaults, user and project merge, levels, approval modes |
| `cypher-permissions/paths.ts`, `glob.ts` | Canonical paths, patterns, zones and guards |
| `cypher-permissions/policy.ts` | Zone × capability → decision |
| `cypher-permissions/grants.ts` | Grants and their persistence |
| `cypher-permissions/discover.ts` | Concrete paths for Landstrip; the home index |
| `cypher-permissions/sandbox.ts` | Landstrip status, policy compiler, wrapper, env scrub, denial events |
| `cypher-permissions/judge.ts` | Judge prompt and verdict parsing |
| `cypher-permissions/audit.ts` | The audit log |
| `cypher-permissions/shell.ts` | Reading a command line into simple commands, or opaque |
| `cypher-permissions/commands.ts` | Command rules: patterns, validation, the banned programs, resolving a program |
| `crates/engine/src/pi/permissions.rs` | The settings Settings → Permissions edits; `landstrip doctor` |
| `crates/ui/src/settings/permissions.rs` | Settings → Permissions |
| `crates/ui/src/composer/slash_menu.rs`, `slash.rs` | `/permissions` in the composer's `/` menu |
| `crates/ui/src/pickers/` (`PERMISSION_LEVELS`, `PickerKind::Permissions`) | The level names; the Full access shield and its menu |

Tests: `cypher-permissions.test.mjs` (engine, no Pi or Landstrip) and
`cypher-permissions.staged.test.mjs` (a real `bin/pi` in RPC mode from a staged Runtime,
a scripted faux model that also answers as the judge, the real Landstrip, the patched
grep and apply_patch, dialogs answered over RPC). The package script runs both on macOS and Linux builds; the
staged suite skips its sandbox cases where `landstrip doctor` fails. Rust:
`pi::permissions` (the settings file merge), `tests/chat_permission_level.rs` (the row's
level wins), the harness's `CYPHER_PERMISSION_LEVEL` test, and the picker and Settings
page unit tests.

## 10. Phases

- **Phase 0, spike**: done (the measurements in §6.2 and §11).
- **Phase 1, core**: done. Engine side: `CYPHER_PERMISSION_LEVEL` from
  `RunRequest.sandbox`; `cypher-permissions.ts` in `CYPHER_EXTENSIONS`; the
  `pi-permission-control` special cases removed from `runtime.rs` and `packages.rs`;
  `@landstrip/*` is bundled but, like `@earendil-works/*`, not a Pi plugin
  (`package-pi-runtime.sh`, `scripts/ci/release.py`).
- **Phase 2, Cypher UI**: done. `/permissions` in the composer (§5.2) and the engine
  honoring the row's level; Settings → Permissions with the device's sandbox status
  (§8); structured approval cards. A chat's grants are listed and revoked with
  `/perm-grants` (the composer's `/` menu).
- **Phase 3**: done. The filtered network (§4.2); per-MCP-server decisions in Settings →
  Permissions. (Linux live approvals, which held a denied syscall while the user was
  asked, were removed in phase 4.)
- **Phase 4, leaving the sandbox**: commands that need more than the sandbox run outside it
  once the user allows them, by command rules the agent proposes (§6.3). The per-command
  grants to sensitive paths, the network and environment variables, and Linux live
  approvals, are gone: a sandboxed command never holds a secret.

## 11. Limits

- **The Pi process is trusted.** `read`/`write`/`edit`/`ls` are enforced by this code in
  Pi's process, not by the kernel. MCP servers and other extensions run unsandboxed;
  MCP tools are allowed by default.
- **Same-user attacker**: not defended; use a VM (for example Docker `sbx`).
- **Secrets outside the workspace, for bash**: a secret created outside the workspace is
  denied to file tools at once, but to bash only after the home index refreshes (up to
  10 minutes; caches and `node_modules` are not indexed).
- **No taint tracking across a turn**: a secret read into the model's context (a file
  tool approval) can later leave through a command's network.
- **Outside the sandbox nothing applies**: an allowed command has the user's files,
  credentials, network and processes within what its pattern matches. A program that reads
  workspace files as code (`make`, `npm` scripts, `cargo` build scripts) runs them with
  those rights; the dialog shows the command, not what it will read. Git hooks and config
  are protected (§3.4), so the agent cannot plant code for an allowed `git` command.
- **A command that needs more fails once first** (§6.3), on both platforms.
- **Filtered network**: only programs that honor `HTTP(S)_PROXY` reach allowed domains;
  others run outside the sandbox when allowed. Domain filtering sees host names (CONNECT
  and the `Host` header), not URLs.
- **`/permissions`** is still offered on a device where permission control is
  unavailable; Settings → Permissions says so, and the chat runs unrestricted there.
- **Linux (Landstrip 0.18.38)**:
  - A background process (`npm run dev &`) loses file access once the command that
    started it exits: the seccomp broker ends with it. Keep long-running processes in the
    foreground of their own command.
  - Writing through `/dev/stdout`, `/dev/stderr` or `/dev/fd/N` by name is denied (a rule
    on them makes Landlock reject the policy); `>&2` and redirects are unaffected.
  - Inside a Docker container with Docker's default seccomp profile every connect fails;
    run the container with `--security-opt seccomp=unconfined` or on the host.
- **grep**: ripgrep exits with an error if it meets an unreadable file that was created
  after the policy was compiled (within 1.5 s).
- **Hard links into the workspace** of external files: Landstrip blocks links to denied
  paths; linking other external files is not restricted.

## 12. Decisions and open questions

Decided (2026-10-11):

- **D1.** `external.write` defaults to `assisted`.
- **D2.** MCP and extension tools are allowed by default; overrides tighten.
- **D3.** Persistence paths (`.git/config`, hooks, shell startup files) are protected, not
  sensitive: writes go to the judge first.
- **D4.** `full-access` runs everything unchecked and unsandboxed.
- **D5.** On hosts where Landstrip cannot sandbox, the extension stands down and says so.
- **D6.** The code lives in this repository as a Runtime extension; the npm
  `pi-permission-control` package is dropped.
- **D7.** Cypher's and Pi's data is split: its secrets are sensitive, the rest is
  self-guarded (readable, human-only writes), worktrees are neither.
- **D8.** The judge defaults to the chat's own model.
- **D9.** (2026-10-13) Composite resources (SSH, Git remotes, keychains, processes,
  system services) are reached by running the command outside the sandbox with the
  user's approval, by command patterns the agent proposes, not by widening the sandbox
  piece by piece. The sandbox never hands a command a secret.

Open:

- **Q1. Command-scoped "always" rules**: resolved by command rules (§6.3, decided
  2026-10-13): approved commands run outside the sandbox; the shell reader refuses lines it
  cannot read reliably.
