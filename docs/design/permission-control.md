# Permission control (cypher-permissions)

Status: **phases 0–1 implemented** (Runtime 1.1.0.2; off by default since 1.1.0.3). Phases 2–3 (§10) are planned.

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

Non-goals are listed in §11. This is not a VM: it protects against model mistakes and
prompt-injection-driven misuse, not against a determined attacker running as the same
user.

## 2. Concepts

| Term | Meaning |
|---|---|
| **Zone** | Classification of a path: `workspace`, `external`, `sensitive` |
| **Guard** | A write rule on top of the zone: `protected` or `self` (§3.4, §3.5) |
| **Capability** | `read`, `write` (write includes read); network is per command (§4.2) |
| **Decision** | Per zone × capability: `allow`, `elevate`, `deny` |
| **Approval mode** | How an elevation is decided: `deny`, `ask`, `assisted` |
| **Grant** | A recorded approval: what it covers and for how long (`once`, `chat`, `always`) |
| **Level** | A named preset of decisions, chosen per chat (§5.2) |

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
  `~/.pnpm-store`, `~/.bun/install/cache`, `~/.yarn/berry/cache`.

### 3.3 Sensitive patterns

Defaults (editable, `config.ts` `DEFAULT_SENSITIVE`):

| Group | Patterns |
|---|---|
| Credentials in the home directory | `~/.ssh/**`, `~/.gnupg/**`, `~/.aws/**`, `~/.azure/**`, `~/.config/gcloud/**`, `~/.kube/**`, `~/.docker/config.json`, `~/.netrc`, `~/.npmrc`, `~/.pypirc`, `~/.config/gh/**`, `~/.git-credentials`, `~/.config/git/credentials`, `~/Library/Keychains/**`, `~/Library/Cookies/**`, browser profiles (Chrome, Brave, Edge, Arc, Firefox, Safari, Chromium) |
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
`~/.gitconfig`, `~/.config/git/**`, the zsh/bash/profile startup files,
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

### 4.2 Network is per command

A path cannot have network access; a process can. So the sensitive zone's "no network" is
a rule on commands:

> A shell command has the network unless it holds a grant to a sensitive path. A command
> that can read a secret runs **offline**, unless the network was approved together with
> it.

A command can have the secret or the network, not both, unless a human says so. A bash
grant to a sensitive path is therefore always bound to one exact command (§7.5). Common
cases need both (`git push` over SSH, `gh pr create`); `request_permission` with
`network: true` asks the human, and the dialog says *"This command will be able to read
these paths AND use the network."*

Other network rules for every sandboxed command:

- Local binding (dev servers, tests on `localhost`): allowed (`network.localBinding`).
- Unix sockets: denied, which covers `docker.sock` (root-equivalent) and `SSH_AUTH_SOCK`.
  It also breaks the `docker` CLI, ssh-agent and GPG-signed commits; list a socket in
  `network.unixSockets` to allow it.
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
(a `net:<domain>` rule). A command that can read a secret gets no proxy either. Programs
that ignore the proxy variables (ssh) need `request_permission` with `network: true` for
the exact command. A project's file may switch its chats to filtered mode, never back.

### 4.3 Secrets in the environment

A sandboxed command's environment drops variables matching `*_TOKEN`, `*_SECRET`,
`*_SECRET_KEY`, `*_API_KEY`, `*_APIKEY`, `*_PASSWORD`, `*_ACCESS_KEY`, `*_PRIVATE_KEY`,
`AWS_*`, `GH_TOKEN`, `GITHUB_TOKEN`, `SSH_AUTH_SOCK`, `CYPHER_ENGINE_SOCKET` (editable).
`request_permission` with the path `env:NAME` restores one variable for one command; it
counts as sensitive, so the command runs offline unless the network was also approved.

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
denyRead    = sensitive paths − read grants
allowWrite  = workspace roots + writable support + temp + devices + write grants
              ("/" when the level lets the external zone be written)
denyWrite   = (sensitive + protected + self) paths inside a writable root − write grants
              + every ancestor of those, between the writable root and the path,
                as a single-entry pattern
network     = allowNetwork unless a sensitive grant applies (and no network grant);
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

### 6.3 Bash denials

macOS Seatbelt is fixed at process start and reports no per-access events, so elevation for
bash is **request, then retry**:

1. The command hits `EPERM`.
2. If a sandboxed command fails with a denial in its output (`Operation not permitted`,
   `Permission denied`, DNS failures), the tool result gets a hint: what the sandbox blocks,
   whether the network is off, and how to call `request_permission`.
3. The agent calls `request_permission` with the exact command, the paths and whether it
   needs the network.
4. The approval creates a grant bound to that command, and the agent reruns it.

On Linux, Landstrip also prints JSON denial events (`FILESYSTEM_DENIED`,
`NETWORK_DENIED`): the extension strips them from the output, lists the blocked paths in
the hint and records them in the audit log.

**Linux: live approvals** (`live.ts`). Landstrip's seccomp broker can hold a denied
syscall and ask over a socket (`--trap-fd 3`): one JSON query per syscall, one
`{"query_id","action"}` line back. Pi's bash executor spawns with three stdio streams
only, so on Linux the extension supplies its own `exec` with Pi's contract (output
streaming, process-group kill on abort and timeout, `aborted` / `timeout:<seconds>`
errors, 128 + signal for a killed shell). A query goes through the approval pipeline in
the running command's context while the command waits ("The command is paused until you
answer"): "Allow for this command" lasts until it ends, "Allow for this chat" grants later
commands (a sensitive path only to this exact command). Not asked:

- a sensitive read while the command has the network (the offline rule; the agent gets
  the request-then-retry path instead, which compiles an offline policy);
- a path the zones allow but the sandbox held (an ancestor guard against a rename);
- network queries outside filtered mode, DNS (port 53) and binds; in filtered mode a
  direct connect from a command without secrets is a `network` elevation.

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
- **`request_permission`**: `{ capability: "read" | "write", paths, command?, network?,
  reason }`. With `command`, the approval is bound to that exact command string (bash).
  Without it, it covers later file tool calls. `env:NAME` names an environment variable.
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
4. A sensitive + network request always goes to a human.

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
| Allow once (Allow this command once) | `once` | all |
| Allow for this chat (Allow this command for this chat) | `chat` | all |
| Always allow | `always` (a rule in `permissions.json`) | file tools, not sensitive, not self |
| Deny | — | all |
| Deny and stop | — (aborts the turn) | all |

No timeout by default: an unattended run waits for a remote approval. `ask.timeoutMs`
denies after a delay. Without a UI (print mode) a request is denied.

### 7.5 Grants

```ts
interface Grant {
  id: string;
  capability: "read" | "write";   // write implies read
  paths: string[];                // canonical; a directory covers its subtree; env:NAME
  command?: string;               // bash: the exact command
  network?: boolean;              // bash: network despite a sensitive path
  sensitive?: boolean;
  scope: "once" | "chat" | "always";
  approvedBy: "user" | "judge" | "rule";
  createdAt: string;
  chatId?: string;
}
```

- `once`: in memory, consumed by the next matching use (a bash grant by the next run of
  its command). Pi runs one process per turn, and a once grant is asked for and used in
  the same turn.
- `chat`: a `cypher-permissions.grant` custom entry in the Pi session file, tagged with
  `CYPHER_CHAT_ID`. Cypher forks a chat by copying its session file
  (`crates/harness/src/pi/fork.rs`), so entries from another chat id are ignored: a fork
  starts without grants. Revocation appends `cypher-permissions.revoke`.
- `always`: a rule in `permissions.json`.
- Bash runs with the unbound non-sensitive grants plus the grants bound to its exact
  command. A sensitive grant is never unbound for bash.
- Pseudo-paths: `env:NAME` (an environment variable), `net:<domain>` (the filter
  proxy), `tool:<name>` (an MCP or extension tool). They never become filesystem rules.
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
the extra sensitive / protected / writable / allowed-domain / Unix-socket lists, and the
always-allow rules with Remove.

**Approval dialogs** ride Cypher's question channel (`ctx.ui.select` → the harness's
`UserInputQuestion` → the composer's question card, iOS's question panel, the push
notification). The title is `question\n\nContext:\n` followed by one `Label: value` line
per fact (`Path`, `Matches`, `Domain`, `Tool`, `Command`, `Reason`, `Network`, `Note`,
`Review`), with a `Change:` or `Input:` block last; paths show the home directory as `~`.
The desktop recognises a permission request by its "Deny and stop" option and lays it out
as a permission card (`crates/ui/src/composer/permission_card.rs`, `question_card.rs`):
the facts as rows, the sentences as notes, the change in a code block, and the options as
buttons weighted by what they grant (the narrowest approval filled, broader ones outlined,
Deny quiet, Deny and stop in red). Other clients show the same text as a headline over a
context block. The transcript's answered chip shows only the question.

**Shortcuts** (desktop permission card; letters shown as keycaps on the buttons): `Y` the
narrowest approval (Allow once / Allow this command once / Allow for this command), `C`
for this chat, `A` always (when offered), `N` deny, `S` deny and stop, `⌘↵` the narrowest
approval, `esc` dismiss (no approval), and number keys by position. Plain `↵` approves
nothing. The card takes keyboard focus wherever focus lands on its composer (opening, chat
switches), never from another chat or a terminal; keys within 500 ms of the card appearing
and held-key repeats are ignored, so typing into the composer as a request arrives never
answers it.

Commands:

- `/perm`: level, sandbox, workspace roots, defaults, approval modes, grants, warnings.
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
| `cypher-permissions/live.ts` | Linux live approvals: the trap socket and Pi's `exec` contract |
| `crates/engine/src/pi/permissions.rs` | The settings Settings → Permissions edits; `landstrip doctor` |
| `crates/ui/src/settings/permissions.rs` | Settings → Permissions |
| `crates/ui/src/composer/slash_menu.rs`, `slash.rs` | `/permissions` in the composer's `/` menu |
| `crates/ui/src/pickers/` (`PERMISSION_LEVELS`, `PickerKind::Permissions`) | The level names; the Full access shield and its menu |

Tests: `cypher-permissions.test.mjs` (engine, no Pi or Landstrip) and
`cypher-permissions.staged.test.mjs` (a real `bin/pi` in RPC mode from a staged Runtime,
a scripted faux model that also answers as the judge, the real Landstrip, the patched
grep and apply_patch, dialogs answered over RPC; the live-approval and request-retry cases
run on their own platform). The package script runs both on macOS and Linux builds; the
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
- **Phase 3**: done. The filtered network (§4.2); Linux live approvals (§6.3);
  per-MCP-server decisions in Settings → Permissions.

## 11. Limits

- **The Pi process is trusted.** `read`/`write`/`edit`/`ls` are enforced by this code in
  Pi's process, not by the kernel. MCP servers and other extensions run unsandboxed;
  MCP tools are allowed by default.
- **Same-user attacker**: not defended; use a VM (for example Docker `sbx`).
- **Secrets outside the workspace, for bash**: a secret created outside the workspace is
  denied to file tools at once, but to bash only after the home index refreshes (up to
  10 minutes; caches and `node_modules` are not indexed).
- **No taint tracking across a turn**: a secret read into the model's context can later
  leave through an allowed network command. The offline rule covers shell processes only.
- **Bash elevation on macOS is request-then-retry** (§6.3); Linux asks live.
- **Filtered network**: only programs that honor `HTTP(S)_PROXY` reach allowed domains;
  others need direct network for their command. Domain filtering sees host names (CONNECT
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

Open:

- **Q1. Command-scoped "always" rules** for credential + network tools (`git push` may
  use `~/.ssh` with the network). Only safe for a single simple command, so it needs a
  shell parser that rejects compound commands. Not built yet.
