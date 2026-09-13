# Cypher 0.3.2 — Fix the Runtime setup installation crash

- Fix an immediate desktop crash when clicking **Download runtime** in the
  first-run setup screen. The progress task now uses GPUI's timer rather than
  a Tokio timer on the GUI foreground thread.
- Stop the progress polling task when installation succeeds or fails, so it
  cannot interfere with a subsequent retry.
- Add a GUI interaction regression covering the actual install button,
  progress updates, error recovery, retry, duplicate-start protection, and
  successful completion. Run the setup UI tests in macOS CI.
- Keep the isolated Runtime at **0.85.1.1**, containing **Pi 0.85.1**.

macOS ARM64 and Linux x86_64/ARM64 are included. iOS is not included.
Existing chats, credentials, and configuration are preserved.
Release tag: `cypher-v0.3.2`; previous release artifacts are not overwritten.
