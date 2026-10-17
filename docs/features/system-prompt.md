# System prompt

Settings → Agents → **System prompt** holds instructions that every Pi chat on
the selected device adds to its system prompt. They are device-scoped, like
Message translation and Context recovery: the device that hosts a chat applies
its own text.

The text is that device's Pi `APPEND_SYSTEM.md`, in its Runtime agent
directory (`~/.cypher/pi-runtime/agent/APPEND_SYSTEM.md` by default; the card
shows the path). Editing the file by hand works too.

- **Save** writes the text, trimmed. Saving it empty removes the file, so no
  empty section is appended. Text over 64 KB is refused: it rides every turn.
- Pi reads the file when a chat's process starts. Saving drops parked
  processes, so each chat uses the new text from its next message; a turn
  already running finishes with the old one.
- It applies on every model. On Claude, pi-claude-bridge appends it after
  Claude Code's own system prompt, which it cannot replace.
- A subagent whose profile has a prompt of its own launches with that prompt
  appended instead, so it does not get this text. One without gets it.
- A project's `.pi/APPEND_SYSTEM.md` is normally not used: Pi loads project
  resources only for trusted projects, and under Cypher it has no way to ask,
  so a project it has no stored trust decision for stays untrusted.
