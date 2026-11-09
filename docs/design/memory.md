# Memory

Status: **built** (M1–M5; decisions in §11, implementation notes in §12). It replaced the
workspace notebook ([Agent bots](agent-bots.md) §7) and gives sessions memory of their own.

Memory is what an agent learns from working with you and keeps between sessions, as text
you can read and edit. It follows the shape Anthropic describes in
[The components of agent memory](https://www.anthropic.com/research/components-of-agent-memory)
and ships in Claude Code and Claude Managed Agents:

| Layer | Claude | Cypher |
|---|---|---|
| 1. Transcripts | Session logs, searchable | Chat docs (chat2) plus the full-text index; `search_history` (§5) |
| 2. Notes | Files the agent writes in a memory folder | Topic files in a memory store (§2) |
| 3. Representations | Linked pages, mostly written while dreaming | Topic files that link each other; no separate "pages" concept |
| 4. Injected context | `MEMORY.md`, first 200 lines or 25 KB, loaded every session | Each store's `MEMORY.md` in the prompt, within a budget (§4) |
| Dreaming | An offline pass that rereads transcripts and repairs memory | Consolidation (§6) |

Two lessons from that work set the rules here. Memory is plain, inspectable files with a
small index always loaded and detail read on demand. And the agent decides what is worth
keeping: steering it to save only some kinds of things (only failures) lost much of the
gain.

Memory is not `AGENTS.md` / `CLAUDE.md`. Those are instructions people write and commit
for everyone working on a repository; Cypher keeps loading them as today
(`pi-coding-agent-cwd-context-files.mjs`). Memory is learned by the agent, belongs to one
user, syncs across that user's devices and is never committed. Claude Code's own memory
files stay excluded on the Claude routes (`pi-claude-bridge-context-files.mjs`) so there is
one memory, Cypher's.

## 1. Scopes

| Store | Who reads and writes it | Holds | Like Claude Code's |
|---|---|---|---|
| **You** | Every session and workspace of the user | Preferences and habits across projects ("answer in English", "prefer small PRs") | `~/.claude/CLAUDE.md` + user memory |
| **Project** | Every session in that project, on any device | How this codebase works: commands, pitfalls, decisions | `~/.claude/projects/<repo>/memory/` |
| **Workspace** | That workspace's agent (chat, work and routine threads) | What it coordinates: who owns what, what is blocked, standing decisions | (the notebook today) |
| **Agent role** | One subagent profile (`reviewer`, `planner`…) | What that role learned, e.g. a reviewer's list of a codebase's pitfalls | `memory:` in a subagent's frontmatter |

A session sees You + its Project (+ its role's store, for a child subagent).

A workspace sees You + its own store, and **reads, never writes,** the Project store of
each project in its scope: that is the project's base memory, what its sessions learned.
What the workspace learns about a project goes into **its own store**, one file per
project (`/workspace/projects/<project>.md`), because a workspace carries context the
project's sessions don't (plans, owners, other projects) and that must not leak into the
project's memory. A project's sessions never see a workspace's project files, including
sessions the workspace started. Peer threads (another person's workspace asking) see no
store at all.

A project store is keyed by the project (space), not the folder, so worktrees and every
device of the same project share it, as Claude Code keys memory by repository.

## 2. Stores

A store is a small tree of markdown files addressed by path, like a Managed Agents memory
store:

```
/MEMORY.md            the index: always injected, kept short
/build-and-test.md    topic files: detail, read on demand
/decisions/rounding.md
```

**Storage: one Loro doc per store, synced through the chat2 relay.** Each file's text is a
`LoroText` (character-level merges) with per-file metadata (author, times). The engine opens
a store through its doc host like a chat, so it persists on the device and syncs through a
chat2 room of its own (`chat2/{storeId}`, the same append-only ChatRoom protocol): a store
syncs to every device, works offline and in local mode, and is loaded only by an engine that
runs something reading it. The registry holds one small row per store (`memoryStores`: id,
scope, counts, a preview of `MEMORY.md`, consolidation settings and the last pass) so devices
can list stores without loading them. Loro's history gives undo for free (§6).

Why not the alternatives:
- Registry rows (what notes are today) sync, but every device holds every row, and the
  registry is the index of all your work; memory text would bloat it.
- Plain files on the host (Claude Code's model) are simplest but stay on one device: a
  project's sessions on your laptop and desktop would remember different things.

Limits: 64 KB per file, 512 KB per store (both reported to the agent when hit, never
silently cut).

## 3. The memory tool

One tool, `memory`, with the commands of Claude's memory tool, so models already trained
on it use it well: `view` (a file, or a folder listing with sizes), `create`, `str_replace`,
`insert`, `delete`, `rename`, plus `search` (§5). Paths are virtual: `/you/…`, `/project/…`,
`/workspace/…`, `/role/…`, one root per store the run can see. A workspace sees the
projects in its scope read-only, as `/projects/<project>/…`; a write there is refused with
a pointer to `/workspace/projects/<project>.md`. It is a Pi extension tool
(like the bot tools) answered by the engine, so it works on every model and provider, needs
no filesystem access and never touches the sandbox.

A run that may only read memory gets `view` and `search`. Every write records its author
(you, a session, a workspace thread, consolidation) in the file's metadata and in the
Activity log of the store's owner.

## 4. Injected context

Each store the run sees contributes its `/MEMORY.md` to the system prompt, under a heading
naming the store, then a one-line listing of its other files. A workspace's chat gets You
and its own store, and a listing of the project stores it may read; a work thread also gets
the `MEMORY.md` of the projects its plan names, and the workspace's file on each. Budget: the first 200 lines
or 16 KB per store, at most 32 KB in all (You first, then Project or Workspace, then role);
past the budget the prompt says so and where the rest is.

The prompt's guidance stays short and unsteered: keep `MEMORY.md` an index of what you will
need again, put detail in topic files and link them, fix or delete what turns out wrong,
and never store secrets. It does not prescribe what kinds of things to save.

Injection happens when a run starts, like the appended prompt today, so a write is seen by
the next run; the writing run already knows what it wrote.

## 5. Transcripts and search

`memory search {query, store?}` ranks files and lines by how many query words they match,
inside longer words and across forms (the notebook's `search_notes` rules), and returns
paths and line numbers to `view`.

`search_history {query}` searches past conversations the run may read: a session searches
its project's sessions; a workspace searches its own chat and threads (today excluded from
its index) plus the sessions in its scope. It reuses the full-text index
(`bots/index.rs`), widened from "bot scopes" to "any reader's scope". Peer threads stay out
of every result.

## 6. Consolidation (dreaming)

A background run per store that rereads what happened and repairs memory: it checks notes
against the transcripts they came from, merges duplicates, removes what is stale, moves
detail out of `MEMORY.md` into topic files (leaving a link), and promotes what keeps
mattering into it. It never edits transcripts.

- **On by default, nightly**, for every store with new activity since its last pass; a
  store with none is skipped, so an idle project costs nothing. Each store's setting can
  turn it off or change the time.
- **Workspace store:** run on the workspace's host over its own chat and threads and the
  sessions it may read, writing only its own store. It never moves anything into a project's base memory: that stays
  the project sessions' to write.
- **Project store:** run on the project's host device over that project's sessions since
  the last pass.
- **You store:** run over all sessions since the last pass, keeping only what holds across
  projects.
- **Inputs:** sessions and threads since the last pass, at most 100; never peer threads.
- **Output:** applied in place. The run's short summary shows in the store's memory view
  (and a workspace's Activity), every change is in the change log, and "Undo" restores the
  store from the doc history before the pass.
- **Cost:** real model runs; the setting shows the last pass's token use and the month's
  total.

## 7. Trust

Memory is injected into every later run, so a bad write persists. The rules:

- **Tainted runs don't write memory freely.** A run that read untrusted content (a web
  page, another workspace's answer; the permission system already tracks this) may still
  call `memory`, but its writes land as **pending**: kept, shown in the memory view and in
  Needs you, and left out of injection until you accept them. Consolidation never accepts
  pending writes on its own.
- **Your text can be corrected, never silently.** Agents may edit or remove lines you
  wrote; each such change is logged in Activity with the old text and can be undone.
- **No secrets.** Writes are scanned like peer answers (Agent bots §11.6) and a match is
  refused with the reason.
- **Peers never.** Peer threads get no `memory` tool and no injected memory.

## 8. UI

- **Memory panel** (replaces the Notebook): the store's `MEMORY.md` rendered first, its
  files below as a list; open a file to read or edit it as markdown; pending writes on top
  with Accept / Discard; each file shows who last changed it and when.
- **Where it opens:** a workspace's sidebar card "Memory"; a project's page (Memory
  section); Settings › Memory for the You store; the subagent editor gets a Memory switch
  (user or project scope).
- **In a session:** `/memory` opens the session's stores; tool chips name the file
  ("Updated memory · build-and-test.md").

## 9. Migration

The workspace notebook becomes the workspace store: notes become lines in `/MEMORY.md`,
newest first, each keeping its author and date as metadata, and the notebook tools are
removed in the same release (the agent's prompt and tools change together, so nothing
calls them). The Notebook card and panel become Memory.

## 10. Phases

| Phase | Ships |
|---|---|
| M1 | Stores (doc + relay), the `memory` tool, injection; the workspace store replaces the notebook; the Memory panel |
| M2 | You and Project stores for sessions; Settings › Memory and the project Memory section; pending writes for tainted runs |
| M3 | `search_history` for sessions and workspaces |
| M4 | Consolidation, workspace first, then projects |
| M5 | Agent-role stores (`memory:` on subagent profiles) |

## 11. Decisions

1. **Storage:** one Loro doc per store over the chat2 relay.
2. **Workspaces and projects:** a workspace reads a project's base memory and never writes
   it; it keeps what it learns about a project in its own store, one file per project, which
   the project's sessions never see.
3. **Consolidation:** on by default, nightly, skipping stores with no new activity.
4. **Your text:** agents may edit it; every change is logged and undoable.
5. **Untrusted writes:** held as pending, out of prompts until you accept them.

## 12. As built

- **Engine** (`crates/engine/src/memory*`). A store's id is its room id: `mem-u-<user key>`
  (a hash of org and user), `mem-p-<space>`, `mem-w-<workspace>`, `mem-r-…` for roles. Its
  doc uses root containers only (`memoryFiles` for metadata, one `mem:<path>` text per file,
  `memoryPending`, `memoryChanges`, `memoryMeta`), so two devices that create the same file
  merge its text instead of keeping one side. Writes are serialized per store; a device
  opening a store for the first time waits briefly for its room to catch up before writing.
  Each change logs its author, a one-line summary, the text before (clipped) and the doc
  version before it, so Undo restores exactly; the log keeps 300 entries.
- **Who sees what** is resolved per call from the chat row (`memory/access.rs`), like the
  workspace tools: sessions `/you` and `/project`; subagents also `/role` when their profile
  says `memory: user|project`; workspace threads `/you`, `/workspace` and read-only
  `/projects/<name>` (a write there is refused with a pointer to
  `/workspace/projects/<name>.md`); a routine thread writes `/workspace` only when its
  routine's `writeNotes` is set; a consolidation pass sees only its store; peer threads
  nothing (no tool, no prompt).
- **Tools**: one Pi extension (`memory.ts`) loaded into every run and switched on by the
  engine (`CYPHER_MEMORY`) when the run has memory; calls go to `MemoryTool` on the agent
  socket. A child subagent with an allowlist gets the three memory tools added.
- **Injection**: the memory section is appended to every run's prompt when it spawns. A
  workspace's read-only project stores appear when they hold something, or, for a work
  thread, when its plan names the project.
- **History**: the search index now holds every session and the workspaces' own threads
  hosted here (never peer threads or passes); who may read which is decided per query.
- **Untrusted content**: a run is tainted by a web fetch or search, an MCP tool call, or a
  shell command that fetches from the network (curl, wget, a URL), and, in a workspace,
  by another workspace's answer; the taint lasts until the chat's next run.
- **Consolidation** runs where its sources live (a project's device, a workspace's host;
  user-level stores on the device that made them). A pass is a hidden chat (`Chat.memory`)
  run unattended with only the memory tools; its token use comes from Pi's session stats.
  "Run now" sets the row's `passRequestedAt`, which the host picks up.
- **Migration**: at start, each workspace host folds its notebook into `/MEMORY.md` under
  "From the notebook" (newest first, source and date per line), once, and drops the rows.
