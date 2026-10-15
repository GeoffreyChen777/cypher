# Context recovery

Context recovery restarts a failed turn on its own. When the agent's answer or
an error contains a phrase you configured, Cypher asks a model you picked to
write supplementary context for the failed request. It then restarts the
conversation from just before your message and sends that message again with
the context ahead of it.

## Setting it up

Settings → Agents → **Context recovery** holds this device's settings. They
are device-scoped, like Message translation: the device that hosts a chat runs
its recovery.

- **Enable for session models**: only chats on one of these models are
  watched.
- **Write context with**: the model, from the device's Pi catalog, that
  writes the supplementary context.
- **Attempts per message** (1–3): how many times one message is restarted
  before a failure is left standing.
- **Trigger phrases**: one per line. A turn whose answer text (the agent's
  own words, or their translation) or error message contains one, in any
  letter case, counts as failed.
- **Context instruction**: what the model is asked to write. It always also
  receives your message, the agent's answer, any error, and the matched
  phrase. From the second attempt on, it also receives every earlier
  attempt's context with how that attempt failed, is told they all failed,
  and is asked to rewrite the context rather than repeat them.

The pickers and the attempt count save as they change. The two text fields
save with their **Save** button.

## What happens

When a round on a watched Pi chat settles (completed or errored; a turn you
stopped is never recovered) and a trigger matches, a spinner under the failed
reply says what is happening — "Writing context to retry…", then "Restarting
with added context…" — with the elapsed time and the phrase that matched.
Then:

1. The recovery model writes the context in a one-shot run, like automatic
   titles.
2. The chat is rewound in place to just before your message, with the same
   operation as **Restart from here**. Your message and the failed answer
   leave the transcript, and they leave the agent's context too.
3. Your message is sent again. The transcript shows it as you wrote it; the
   agent receives the context ahead of it as a reference block
   (`Supplementary context (JSON): …` before `User request:`). Comments that
   rode the original message are sent again with it. Attachments are re-sent
   through the message's attachment refs.

When the failed turn had already run tools, their effects are on disk and the
rewind cannot undo them, so the agent is also briefed on that work and asked
to carry on from it instead of doing it again. The same model that writes the
context writes the briefing, from the turn's own tool steps — what each call
did, whether it succeeded, failed or never finished, which files it changed,
and its recorded output. It reads like

```text
You have already done:
- Edited src/config.rs: added the --profile flag (+12 -3)
- Ran cargo test: 2 failures in config::tests
- Started npm install; it never finished, so check it before relying on it

Please continue.
```

and is written in the language of your message, that heading and closing line
included. It rides the resent prompt as a second block (`Work already done
(JSON): …`). A turn that ran no tools is restarted with no briefing, as
before. Later attempts carry the earlier ones' briefings on, so work from an
attempt whose round has left the transcript is not lost. When the model
answers with nothing usable, the briefing is written from the tool steps
themselves (in English) rather than dropped.

The resent message is captioned **Restarted with context** above its bubble
(with the attempt number from the second attempt on), in the same place and
style as a steer's caption. Hover the caption to see:

- **Restarted from**: the message the conversation restarted after, or the
  beginning of the chat;
- **Triggered by**: the phrase, where it matched (answer or error), and the
  text around it;
- **Already done**: the briefing about the work the failed turn had already
  done, when there was any;
- **Added context**: what the agent received, scrollable when long;
- the model that wrote it.

If a recovered message fails again, it is recovered again from your original
words (contexts never stack) until the attempt limit; the resent message's
record keeps the earlier attempts, so each new context is written knowing all
of them. Nothing changes when the
recovery cannot go ahead: the model gives no answer within three minutes, the
chat cannot be rewound (it is not a Pi chat, has no stored session, or is
hosted elsewhere), or you sent something while the context was being written.
In each case the failed turn stays as it was, and the engine log says why.

Referenced sessions and GitHub issues attached to the original message are not
sent again; only its text, comments and attachments are.

## Subagents

A subagent's chat is recovered too, when its model is one of the watched ones.
Its parent waits for the subagent's result, so while the context is written
the parent keeps waiting: it receives the answer of the restarted task, never
the failed one. A subagent restarted from its first task keeps the message
channel to its parent and its sibling subagents. When the recovery does not go
ahead, the parent receives the failed result as it was. **Restart from here**
stays unavailable in a subagent's chat.

## Storage and sync

The resent message stores its record on the user entry as an additive
`recovery` field (`ContextRecovery` in `cypher-proto`), so the caption syncs
with the transcript; the briefing rides it as an additive `alreadyDone`. A recovery under way rides the chat's session-status row
(`recovery`, a `RecoveryProgress`), so other desktops show the spinner too; a
row still claiming one after five minutes was left by an engine that stopped
mid-recovery and shows nothing. Older apps show neither. The settings live in the engine's data directory as
`context-recovery.json`.

## On iPhone

The iPhone app shows both. The caption sits above the resent message's bubble
like on the desktop; tap it (there is no hover) for the same card in a
popover. A recovery under way shows in the chat's status strip above the
composer, with the same words, elapsed time, phrase and attempt, unless a run
is working. The settings are only on the desktop.
