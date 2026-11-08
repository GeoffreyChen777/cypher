# Moving sessions between projects

A session can move to another project, and so can every session of a
project. The destination may be on the same device or on another device of
the same account. A moved session keeps its id, transcript and agent
context: the next message continues the conversation in the destination
project's folder, on the device that owns that project.

## Moving

- **One session**: right-click it in the sidebar → **Move to Project…**.
- **A whole project**: right-click the project card → **Move Sessions to
  Project…**. Archived sessions move too; the project itself stays, empty,
  until you remove it.

Pick the destination in the palette (↑↓ and ⏎, or click). Projects on
offline devices are listed but can't be picked. The palette follows the move
and closes when every session has moved. Sessions that couldn't move stay
where they were, with the reason listed. Press esc during a move to hide the
palette: the move carries on, and a failure shows in the sidebar notice.

Both devices must be online: the device that runs the sessions sends them,
and the destination device receives them. Moving between devices needs sync
(a signed-in account); local-only Cypher moves sessions between projects of
this device only.

## What moves

| | Same device | Another device |
| --- | --- | --- |
| Transcript | stays (it syncs) | stays (it syncs) |
| Pi session (the agent's context) | rewritten for the new folder | copied, rewritten for the new folder |
| Run journal (full tool inputs) | stays | copied |
| Images in the transcript | stay | copied; the new host serves them under their old paths |
| Subagent sessions | move with their parent | move with their parent |

The Pi session's header names the folder Pi resumes in, so a moved session
always gets the destination project's folder there; every other entry is
copied as it was. The old device deletes its copies once the new one hosts
the session. Project files are not copied: the destination project's folder
is what the agent works in from then on.

## What can't move

- A session that is running or waiting for an answer — move it once the turn
  settles.
- A session with a message still waiting to run.
- Quick chats (they have no project to leave) and temporary side chats.
- Subagent sessions on their own — they follow their parent.

## How it works

The sessions' current host runs the move (`MigrateChats`, a stream of
progress events). For each root session and its subagent sessions it:

1. holds their command drains, so nothing new runs during the move, and
   parks an idle Pi process;
2. opens a staging folder on the destination (`BeginChatMigration`) and
   streams the Pi session, journal and images to it in relay-sized slices
   (`WriteChatMigrationChunk`);
3. asks the destination to commit (`CommitChatMigration`). The destination
   installs the files, writes each session's new home — host device,
   project, folder, Pi session — in one registry write, and starts hosting;
4. writes the same homes itself, deletes its copies and releases the holds.

Both ends write the same home, so the registry agrees whichever write wins,
and each device stops or starts hosting the moment its own write lands. A
failure before the commit leaves the sessions where they were. A message
queued before the move still names the old folder; the new host runs it in
the destination folder instead. Wire types: `crates/proto/src/session_migration.rs`;
engine: `crates/engine/src/session/migrations.rs`.
