# Turn changes

When a round of conversation ends — from the prompt that set the agent
working to the moment it settles — the transcript lists the files that round
changed, under the round's last answer:

- The card's header shows how many files changed and the round's added and
  removed lines. Click it to open the round's whole diff.
- Up to six files are listed with their status (A added, M modified, D
  deleted, R renamed) and line counts. Click a file to open the round's diff
  scrolled to that file. A round that changed more files says how many more;
  the diff shows them all.
- The diff opens as its own tab in the session's right dock, pinned to that
  round (unified or side by side, like every Git diff tab). Clicking another
  file of the same round reuses the tab.

A round that changed nothing shows no card. A steer sent while the agent is
working belongs to the round it lands in; a message sent after the agent
stopped starts a new round.

## What counts as a change

The chat's host engine snapshots the checkout when a round starts and again
when it settles (`git write-tree` through a throwaway index: tracked and
untracked files, `.gitignore`d files excluded; the real index is never
touched). The card is the difference between the two snapshots, so it
includes every change made in the checkout during the round — by the agent's
tools, its shell commands, or anything else editing the same checkout at the
time. Files that were already modified or untracked before the round are not
reported unless the round changed them again.

Nothing is recorded for chats outside a Git checkout, temporary side chats, or
a round interrupted by an engine restart.

## Storage and sync

The changed-file list (at most 100 files; the totals cover every file) is
stored on the round's last assistant message as `turnChanges`, so it syncs to
every device with the transcript. The diff itself is read from the host
device's repository on demand.

The snapshots are unreferenced Git tree objects. `git gc` eventually prunes
them (after two weeks by default), after which the round's diff tab reports
that the snapshot is no longer in the repository. The card itself stays.
