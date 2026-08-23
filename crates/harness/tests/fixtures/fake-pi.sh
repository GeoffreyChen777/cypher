#!/bin/sh
# Fake pi CLI for cypher-harness tests.
#
# Speaks pi's RPC protocol (strict JSONL over stdio) scripted per command:
# get_state / get_available_models / get_commands answer the discovery
# probes, then a scenario picked from the prompt text drives a run.
# Driven by crates/harness/tests/pi.rs. A switch_session whose path contains
# "missing" answers success:false (the loud-resume-failure test); a successful
# switch makes the loaded path the reported sessionFile.

emit() { printf '%s\n' "$1"; }
# pi ids are strings — keep the surrounding quotes in the echoed id.
rid() { printf '%s' "$1" | sed 's/.*"id":"\([^"]*\)".*/"\1"/'; }
has() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }
# Parse --session-dir out of the CLI args so fork/clone results (and the
# default session file) land INSIDE the managed root the controller validates
# against — real pi stores every session under its --session-dir.
SESSION_DIR=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--session-dir" ]; then SESSION_DIR="$arg"; fi
  prev="$arg"
done
[ -n "$SESSION_DIR" ] || SESSION_DIR=/tmp/pi-test
SESSION_FILE="${SESSION_FILE:-$SESSION_DIR/session.jsonl}"
# ---- command loop -----------------------------------------------------------
# Preamble commands (discovery + run setup) are answered inline; a `prompt`
# dispatches its scenario, which runs to completion and exits (EOF then ends
# the harness's stream cleanly, like the ACP fixture).
while read -r line; do
  [ -z "$line" ] && continue
  case "$line" in

  *'"type":"get_commands"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_commands\",\"success\":true,\"data\":{\"commands\":[{\"name\":\"compact\",\"description\":\"Compact the session\",\"source\":\"extension\"},{\"name\":\"skill:brave-search\",\"description\":\"Web search via Brave\",\"source\":\"skill\"}]}}"
    exit 0
    ;;

  *'"type":"get_available_models"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_available_models\",\"success\":true,\"data\":{\"models\":[{\"id\":\"claude-sonnet-4-20250514\",\"name\":\"Claude Sonnet 4\",\"provider\":\"anthropic\",\"reasoning\":true,\"contextWindow\":200000},{\"id\":\"gpt-4o-mini\",\"name\":\"GPT-4o Mini\",\"provider\":\"openai\",\"reasoning\":false,\"contextWindow\":128000}]}}"
    exit 0
    ;;

  *'"type":"get_state"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"model\":{\"id\":\"claude-sonnet-4-20250514\",\"name\":\"Claude Sonnet 4\",\"provider\":\"anthropic\",\"reasoning\":true},\"thinkingLevel\":\"medium\",\"isStreaming\":false,\"sessionFile\":\"$SESSION_FILE\",\"sessionId\":\"abc123\"}}"
    ;;

  *'"type":"switch_session"'*)
    # A path containing "missing" is the loud-resume-failure fixture: pi
    # answers success:false (the harness must NOT silently start fresh).
    case "$line" in
    *missing*)
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"switch_session\",\"success\":false,\"error\":\"no such session\"}"
      exit 0
      ;;
    esac
    # Remember the loaded session as the current session file — the test
    # asserts SessionStarted/Done carry the resumed path.
    SESSION_FILE=$(printf '%s' "$line" | sed 's/.*"sessionPath":"\([^"]*\)".*/\1/')
    [ -n "$SESSION_FILE" ] || SESSION_FILE=/tmp/pi-test/session.jsonl
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"switch_session\",\"success\":true,\"data\":{\"cancelled\":false}}"
    ;;

  *'"type":"set_model"'*)
    # Best-effort on the harness side, so accept anything (the tests' request
    # carries provider/anthropic modelId/claude-sonnet-4-20250514).
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"set_model\",\"success\":true,\"data\":{}}"
    ;;

  *'"type":"set_thinking_level"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"set_thinking_level\",\"success\":true}"
    ;;

  *'"type":"compact"'*)
    # The harness-synthesized /compact built-in: refuse unless the custom
    # instructions were carried (proving the interception relayed them).
    case "$line" in
    *'"customInstructions":"focus on api"'*)
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"compact\",\"success\":true,\"data\":{\"tokensBefore\":150000,\"estimatedTokensAfter\":32000}}"
      exit 0
      ;;
    *)
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"compact\",\"success\":false,\"error\":\"missing custom instructions\"}"
      exit 1
      ;;
    esac
    ;;

  *'"type":"export_html"'*)
    # The harness-synthesized /export-html built-in: refuse unless the
    # output path was carried.
    case "$line" in
    *'"outputPath":"/tmp/x.html"'*)
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"export_html\",\"success\":true,\"data\":{\"path\":\"/tmp/x.html\"}}"
      exit 0
      ;;
    *)
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"export_html\",\"success\":false,\"error\":\"missing output path\"}"
      exit 1
      ;;
    esac
    ;;

  *'"type":"get_entries"'*)
    # Session Fork fixture: a session with an abandoned branch (u2x under
    # a1) that the active branch (leaf a2) must ignore. The active user
    # entries are u1 ("first prompt") and u2 ("second prompt").
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_entries\",\"success\":true,\"data\":{\"entries\":[{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"timestamp\":\"t1\",\"message\":{\"role\":\"user\",\"content\":\"first prompt\"}},{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"timestamp\":\"t2\",\"message\":{\"role\":\"assistant\",\"content\":\"first reply\"}},{\"type\":\"message\",\"id\":\"u2x\",\"parentId\":\"a1\",\"timestamp\":\"t3\",\"message\":{\"role\":\"user\",\"content\":\"abandoned prompt\"}},{\"type\":\"message\",\"id\":\"u2\",\"parentId\":\"a1\",\"timestamp\":\"t4\",\"message\":{\"role\":\"user\",\"content\":\"second prompt\"}},{\"type\":\"message\",\"id\":\"a2\",\"parentId\":\"u2\",\"timestamp\":\"t5\",\"message\":{\"role\":\"assistant\",\"content\":\"second reply\"}}],\"leafId\":\"a2\"}}"
    ;;

  *'"type":"fork"'*)
    # Session Fork fixture: fork BEFORE a user entry by its id — a new
    # persisted session (SESSION_FILE moves to an id-specific path) is the
    # durable effect. Each ACTIVE user entry id yields its own session path,
    # so a test can assert WHICH entry the harness targeted; any other id
    # (abandoned branch, unknown) is refused.
    case "$line" in
    *'"entryId":"u1"'*)
      SESSION_FILE="$SESSION_DIR/forked-u1.jsonl"
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"fork\",\"success\":true,\"data\":{\"text\":\"first prompt\",\"cancelled\":false}}"
      ;;
    *'"entryId":"u2"'*)
      SESSION_FILE="$SESSION_DIR/forked-u2.jsonl"
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"fork\",\"success\":true,\"data\":{\"text\":\"second prompt\",\"cancelled\":false}}"
      ;;
    *)
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"fork\",\"success\":false,\"error\":\"no user entry for fork\"}"
      exit 1
      ;;
    esac
    # Real pi persists the forked session file immediately (every boundary
    # past the first user), so the harness requires it to EXIST. A test that
    # simulates pi NOT materializing the file — the empty-context fork
    # BEFORE THE FIRST USER, whose file pi only writes on the target's first
    # prompt — drops a `.unmaterialized` marker into the session dir and the
    # fixture skips creating the file.
    [ -e "$SESSION_DIR/.unmaterialized" ] || : > "$SESSION_FILE"
    ;;

  *'"type":"clone"'*)
    # Session Fork fixture: clone duplicates the active branch at its leaf —
    # the new persisted session file is the observable durable effect.
    SESSION_FILE="$SESSION_DIR/cloned.jsonl"
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"clone\",\"success\":true,\"data\":{\"cancelled\":false}}"
    [ -e "$SESSION_DIR/.unmaterialized" ] || : > "$SESSION_FILE"
    ;;

  *'"type":"prompt"'*)
    pid=$(rid "$line")
    case "$line" in

    *scenario:happy*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      # Thinking + text deltas; a non-text delta maps to nothing.
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","contentIndex":0,"delta":"thinking"}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Hello"}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":" world"}}'
      # The message ends to call a tool; the tool executes after message_end.
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"Hello world"}],"stopReason":"toolUse"}}'
      emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"bash","args":{"command":"cargo test -p cypher-harness"}}'
      emit '{"type":"tool_execution_end","toolCallId":"t1","toolName":"bash","result":{"content":[{"type":"text","text":"   Compiling cypher-harness v0.1.21\n    Finished `dev` profile"}]},"isError":false}'
      # A toolResult message is not an assistant message: internal only.
      emit '{"type":"message_start","message":{"role":"toolResult","toolCallId":"t1","content":[]}}'
      emit '{"type":"message_end","message":{"role":"toolResult","toolCallId":"t1","content":[]}}'
      # Extension noise: an error surfaces as an Error event, notify maps to
      # a TextDelta (its text is cleared by the next assistant message_start,
      # so Done's result stays the last assistant text).
      emit '{"type":"extension_error","extensionPath":"/x.ts","event":"tool_call","error":"boom"}'
      emit '{"type":"extension_ui_request","id":"u-ff","method":"notify","message":"hi","notifyType":"info"}'
      # A second assistant message completes the turn (LLM→tool→LLM).
      emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Done."}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"Done."}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      exit 0
      ;;

    *scenario:progress*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[],"stopReason":"toolUse"}}'
      # A long-running subagent: three progress updates, the first two 100ms
      # apart (< the 500ms throttle), the third 600ms after the first (>= the
      # throttle) — so exactly TWO ToolProgress should forward (first + the
      # post-throttle one).
      emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"subagent","args":{"agent":"planner","task":"plan the live card"}}'
      emit '{"type":"tool_execution_update","toolCallId":"t1","toolName":"subagent","partialResult":{"content":[{"type":"text","text":"step one"}]}}'
      sleep 0.1
      emit '{"type":"tool_execution_update","toolCallId":"t1","toolName":"subagent","partialResult":{"content":[{"type":"text","text":"step two"}]}}'
      sleep 0.5
      emit '{"type":"tool_execution_update","toolCallId":"t1","toolName":"subagent","partialResult":{"content":[{"type":"text","text":"step three"}]}}'
      # End settles the tool; a late update after end must NOT forward.
      emit '{"type":"tool_execution_end","toolCallId":"t1","toolName":"subagent","result":{"content":[{"type":"text","text":"done"}]},"isError":false}'
      emit '{"type":"tool_execution_update","toolCallId":"t1","toolName":"subagent","partialResult":{"content":[{"type":"text","text":"late after end"}]}}'
      emit '{"type":"message_start","message":{"role":"toolResult","toolCallId":"t1","content":[]}}'
      emit '{"type":"message_end","message":{"role":"toolResult","toolCallId":"t1","content":[]}}'
      emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"done."}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"done."}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      exit 0
      ;;

    *scenario:steer*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"toolUse"}}'
      emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"read","args":{"path":"src/main.rs"}}'
      emit '{"type":"tool_execution_end","toolCallId":"t1","toolName":"read","result":{"content":[{"type":"text","text":"// main"}]},"isError":false}'
      read -r steerline || exit 1
      if has "$steerline" '"type":"steer"' && has "$steerline" 'redirect please'; then
        emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":true}"
        # The steer reply is the next assistant message: the harness must
        # emit its Steered boundary at this message_start.
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"steered"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"steered"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"bad steer\"}"
      exit 1
      ;;


    *scenario:parked-noagent*)
      # A parked second turn whose prompt produces ZERO agent-lifecycle events
      # (notify only): the harness must re-arm the no-activity grace AFTER the
      # prompt response (never a stale turn-1 timer), wait out the fresh
      # window, and Done Completed with the SECOND turn's notify text (per-turn
      # result reset). The notify is delayed past the response so a stale/
      # absent re-arm would fire with empty (or turn-1) text instead. The
      # parked prompt must carry `streamingBehavior:"steer"`.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first turn"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first turn"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      read -r second || exit 1
      if has "$second" '"type":"prompt"' && has "$second" '"streamingBehavior":"steer"' && has "$second" 'notify only'; then
        emit "{\"id\":$(rid \"$second\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        # 150ms < the 200ms test grace: only a freshly re-armed timer reaches
        # this notify; a stale turn-1 timer fires immediately on re-poll.
        sleep 0.15
        emit '{"type":"extension_ui_request","id":"u-2","method":"notify","message":"second turn note","notifyType":"info"}'
        # Never settle, never emit lifecycle events: the harness must reap us.
        while read -r line; do :; done
        exit 0
      fi
      emit "{\"id\":$(rid \"$second\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"parked mailbox must restart via prompt\"}"
      exit 1
      ;;
    *scenario:parked-double*)
      # Two mailbox messages sent rapidly while parked: both must be honored
      # as sequential parked prompts (turn 2 then turn 3), never dropped,
      # never sent as idle steers.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      read -r second || exit 1
      if has "$second" '"type":"prompt"' && has "$second" '"streamingBehavior":"steer"' && has "$second" 'second turn'; then
        emit "{\"id\":$(rid \"$second\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"second"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"second"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        read -r third || exit 1
        if has "$third" '"type":"prompt"' && has "$third" '"streamingBehavior":"steer"' && has "$third" 'third turn'; then
          emit "{\"id\":$(rid \"$third\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
          emit '{"type":"message_start","message":{"role":"assistant","id":"m3","content":[]}}'
          emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"third"}}'
          emit '{"type":"message_end","message":{"role":"assistant","id":"m3","content":[{"type":"text","text":"third"}],"stopReason":"stop"}}'
          emit '{"type":"agent_settled"}'
          exit 0
        fi
        emit "{\"id\":$(rid \"$third\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"third must be a parked prompt\"}"
        exit 1
      fi
      emit "{\"id\":$(rid \"$second\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"second must be a parked prompt\"}"
      exit 1
      ;;
    *scenario:parked-stranded*)
      # The settle race: a steer ACCEPTED mid-turn whose reply never streams
      # because the turn settles first. The harness must not strand it — the
      # accepted steer is retried as a parked prompt (with
      # `streamingBehavior:"steer"`) after the park.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"toolUse"}}'
      emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"read","args":{"path":"x"}}'
      emit '{"type":"tool_execution_end","toolCallId":"t1","toolName":"read","result":{"content":[{"type":"text","text":"x"}]},"isError":false}'
      read -r steerline || exit 1
      if has "$steerline" '"type":"steer"' && has "$steerline" 'redirect'; then
        # Accepted, then settled WITHOUT ever delivering the steer reply.
        emit "{\"id\":$(rid \"$steerline\"),\"type\":\"response\",\"command\":\"steer\",\"success\":true}"
        emit '{"type":"agent_settled"}'
        read -r retry || exit 1
        if has "$retry" '"type":"prompt"' && has "$retry" '"streamingBehavior":"steer"' && has "$retry" 'redirect'; then
          emit "{\"id\":$(rid \"$retry\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
          emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
          emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"redirected"}}'
          emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"redirected"}],"stopReason":"stop"}}'
          emit '{"type":"agent_settled"}'
          exit 0
        fi
        emit "{\"id\":$(rid \"$retry\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"stranded steer must retry as a parked prompt\"}"
        exit 1
      fi
      emit "{\"id\":$(rid \"$steerline\"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"expected a mid-turn steer\"}"
      exit 1
      ;;
    *scenario:parked*)
      # The parked-session regression: the FIRST turn settles but the child
      # persists (a real pi keeps the session open across turns). The second
      # mailbox send MUST arrive as a `prompt` carrying
      # `streamingBehavior:"steer"` (atomic: idle → new turn, still-streaming
      # → queued steer) — an idle `steer` is rejected (a parked pi only queues
      # steers, so the harness would strand the message forever), and a plain
      # prompt without streamingBehavior would be rejected while pi streams.
      # A notify lands BEFORE the prompt response: the harness must have
      # emitted its Steered boundary at dispatch, so the notify folds into the
      # SECOND turn's segment.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      read -r second || exit 1
      if has "$second" '"type":"prompt"' && has "$second" '"streamingBehavior":"steer"' && has "$second" 'second message'; then
        # Pre-response notify, then acceptance, then the second turn.
        emit '{"type":"extension_ui_request","id":"u-2","method":"notify","message":"pre-response note","notifyType":"info"}'
        emit "{\"id\":$(rid \"$second\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"second"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"second"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      emit "{\"id\":$(rid \"$second\"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"parked mailbox must restart via prompt, not steer\"}"
      exit 1
      ;;
    *scenario:interrupt*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"working"}}'
      read -r abortline || exit 1
      if has "$abortline" '"type":"abort"'; then
        # Fire-and-forget abort carries no id — the response needs none.
        emit '{"type":"response","command":"abort","success":true}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"working"}],"stopReason":"aborted"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      exit 1
      ;;

    *scenario:ui-select*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"extension_ui_request","id":"u-1","method":"select","title":"Pick a runtime","options":["tokio","smol"]}'
      read -r ans || exit 1
      if has "$ans" '"type":"extension_ui_response"' && has "$ans" '"id":"u-1"' && has "$ans" '"value":"tokio"'; then
        emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"answered"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"answered"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      exit 1
      ;;

    *scenario:noagent*)
      # An extension command whose handler only notifies: pi forwards the
      # notify UI requests (info multi-line + error), accepts the prompt,
      # then stays silent FOREVER (never settles, never emits an agent
      # LIFECYCLE event). The `thinking_level_changed` and `extension_error`
      # lines mirror what real pi emits off the set_model/set_thinking_level
      # SETUP commands — informational events that must NOT count as agent
      # activity (live-verified: thinking_level_changed is what hung real
      # /subagents runs). The harness must terminate the run itself via the
      # no-activity grace path and reap this child.
      emit '{"type":"extension_ui_request","id":"u-ff","method":"notify","message":"Available subagents:\n- actor: does things\n- coder: writes code","notifyType":"info"}'
      emit '{"type":"extension_ui_request","id":"u-err","method":"notify","message":"command failed: no token","notifyType":"error"}'
      emit '{"type":"thinking_level_changed","level":"high"}'
      emit '{"type":"extension_error","extensionPath":"/x/ext.ts","event":"session_load","error":"boom"}'
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      # Never exit: the harness must kill us (no graceful EOF here).
      while read -r line; do :; done
      exit 0
      ;;

    *scenario:resumed*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"back again"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"back again"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      exit 0
      ;;

    *)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"unknown scenario\"}"
      exit 0
      ;;
    esac
    ;;

  esac
done
