# Cypher for iPhone 0.4.0 (52) — messages from the phone always arrive

## Sessions and workspaces

- **A message sent from the iPhone no longer stays grey forever.** If
  Cypher was closed or stopped by iOS just after a send, before the server
  confirmed it, that send was kept only on the phone. Every later message in
  that chat built on it, so the device running the chat set them all aside:
  they showed grey, nothing ran, and reopening the app didn't help, while the
  chat itself kept updating normally. Now, each time a chat syncs, the phone
  sends again whatever it holds that the server lacks, ahead of anything new.
  Chats already stuck this way recover by themselves the first time they
  open on build 52. Messages that waited longer than a day have expired and
  won't run.

iOS-only build for TestFlight, from main at PR #96. Desktop stays at 0.4.10
and Pi Runtime at 1.5.2.6. The desktop side of PR #96 (a log line on the
device running a chat when it has to set such a message aside) ships with
the next desktop release.
