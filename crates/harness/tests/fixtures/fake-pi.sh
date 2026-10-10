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
# The harness reads the context gauge (`get_session_stats`) in the background
# after assistant messages, settles, and compactions, so those requests can
# interleave with any scripted command. Answered inline with $STATS_DATA —
# empty by default (no gauge event), so scenarios stay deterministic.
STATS_DATA='{}'
stats_reply() {
  emit "{\"id\":$(rid "$1"),\"type\":\"response\",\"command\":\"get_session_stats\",\"success\":true,\"data\":$STATS_DATA}"
}
# `next_cmd VAR`: read the next scripted command into VAR, answering any
# gauge reads on the way.
next_cmd() {
  while read -r __line; do
    if has "$__line" '"type":"get_session_stats"'; then
      stats_reply "$__line"
      continue
    fi
    eval "$1=\$__line"
    return 0
  done
  return 1
}
# Parse --session-dir out of the CLI args so fork/clone results (and the
# default session file) land INSIDE the managed root the controller validates
# against — real pi stores every session under its --session-dir.
SESSION_DIR=""
CURRENT_PROVIDER="anthropic"
CURRENT_MODEL="claude-sonnet-4-20250514"
CURRENT_NAME="Claude Sonnet 4"
CURRENT_THINKING="medium"
prev=""
for arg in "$@"; do
  if [ "$prev" = "--session-dir" ]; then SESSION_DIR="$arg"; fi
  if [ "$prev" = "--model" ]; then
    case "$arg" in
      anthropic/claude-sonnet-4-20250514)
        CURRENT_PROVIDER="anthropic"
        CURRENT_MODEL="claude-sonnet-4-20250514"
        CURRENT_NAME="Claude Sonnet 4"
        ;;
      openai/gpt-4o-mini)
        CURRENT_PROVIDER="openai"
        CURRENT_MODEL="gpt-4o-mini"
        CURRENT_NAME="GPT-4o Mini"
        ;;
    esac
  fi
  if [ "$prev" = "--thinking" ]; then CURRENT_THINKING="$arg"; fi
  prev="$arg"
done
[ -n "$SESSION_DIR" ] || SESSION_DIR=/tmp/pi-test
SESSION_FILE="${SESSION_FILE:-$SESSION_DIR/session.jsonl}"
if [ -f "$SESSION_DIR/.ignore-startup-model" ]; then
  CURRENT_PROVIDER="openai"
  CURRENT_MODEL="gpt-4o-mini"
  CURRENT_NAME="GPT-4o Mini"
fi
# ---- command loop -----------------------------------------------------------
# Preamble commands (discovery + run setup) are answered inline; a `prompt`
# dispatches its scenario, which runs to completion and exits (EOF then ends
# the harness's stream cleanly).
while read -r line; do
  [ -z "$line" ] && continue
  case "$line" in

  *'"type":"get_commands"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_commands\",\"success\":true,\"data\":{\"commands\":[{\"name\":\"compact\",\"description\":\"Compact the session\",\"source\":\"extension\"},{\"name\":\"skill:brave-search\",\"description\":\"Web search via Brave\",\"source\":\"skill\"}]}}"
    exit 0
    ;;

  *'"type":"get_available_models"'*)
    # A marker in --session-dir makes the FIRST snapshot empty (real pi's RPC
    # catalog is empty until the async refresh finishes). Discovery must poll.
    if [ -f "$SESSION_DIR/.empty-models-once" ]; then
      rm -f "$SESSION_DIR/.empty-models-once"
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_available_models\",\"success\":true,\"data\":{\"models\":[]}}"
    elif [ -f "$SESSION_DIR/.partial-models-once" ]; then
      rm -f "$SESSION_DIR/.partial-models-once"
      touch "$SESSION_DIR/.partial-models-grow"
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_available_models\",\"success\":true,\"data\":{\"models\":[{\"id\":\"claude-opus-5\",\"name\":\"Claude Opus 5\",\"provider\":\"claude-bridge\",\"reasoning\":true,\"contextWindow\":1000000}]}}"
    elif [ -f "$SESSION_DIR/.partial-models-grow" ]; then
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_available_models\",\"success\":true,\"data\":{\"models\":[{\"id\":\"claude-opus-5\",\"name\":\"Claude Opus 5\",\"provider\":\"claude-bridge\",\"reasoning\":true,\"contextWindow\":1000000},{\"id\":\"claude-sonnet-4-20250514\",\"name\":\"Claude Sonnet 4\",\"provider\":\"anthropic\",\"reasoning\":true,\"contextWindow\":200000},{\"id\":\"gpt-4o-mini\",\"name\":\"GPT-4o Mini\",\"provider\":\"openai\",\"reasoning\":false,\"contextWindow\":128000},{\"id\":\"kimi\",\"name\":\"Kimi\",\"provider\":\"mvp-lab\",\"reasoning\":false,\"contextWindow\":128000}]}}"
    else
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_available_models\",\"success\":true,\"data\":{\"models\":[{\"id\":\"claude-sonnet-4-20250514\",\"name\":\"Claude Sonnet 4\",\"provider\":\"anthropic\",\"reasoning\":true,\"contextWindow\":200000,\"thinkingLevelMap\":{\"minimal\":null,\"xhigh\":\"xhigh\"}},{\"id\":\"gpt-4o-mini\",\"name\":\"GPT-4o Mini\",\"provider\":\"openai\",\"reasoning\":false,\"contextWindow\":128000}]}}"
    fi
    ;;

  *'"type":"get_state"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"model\":{\"id\":\"$CURRENT_MODEL\",\"name\":\"$CURRENT_NAME\",\"provider\":\"$CURRENT_PROVIDER\",\"reasoning\":true},\"thinkingLevel\":\"$CURRENT_THINKING\",\"isStreaming\":false,\"sessionFile\":\"$SESSION_FILE\",\"sessionId\":\"abc123\"}}"
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
    # A historical session can restore a different model than the new Run
    # requested. The harness must detect this state and switch back before it
    # sends the prompt.
    case "$SESSION_FILE" in
    *other-model*)
      CURRENT_PROVIDER="openai"
      CURRENT_MODEL="gpt-4o-mini"
      CURRENT_NAME="GPT-4o Mini"
      CURRENT_THINKING="low"
      ;;
    esac
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"switch_session\",\"success\":true,\"data\":{\"cancelled\":false}}"
    ;;

  *'"type":"set_model"'*)
    if [ -f "$SESSION_DIR/.reject-model-switch" ]; then
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"set_model\",\"success\":false,\"error\":\"model switch rejected\"}"
    elif has "$line" '"provider":"anthropic"' && has "$line" '"modelId":"claude-sonnet-4-20250514"'; then
      CURRENT_PROVIDER="anthropic"
      CURRENT_MODEL="claude-sonnet-4-20250514"
      CURRENT_NAME="Claude Sonnet 4"
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"set_model\",\"success\":true,\"data\":{}}"
    elif has "$line" '"provider":"openai"' && has "$line" '"modelId":"gpt-4o-mini"'; then
      CURRENT_PROVIDER="openai"
      CURRENT_MODEL="gpt-4o-mini"
      CURRENT_NAME="GPT-4o Mini"
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"set_model\",\"success\":true,\"data\":{}}"
    else
      emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"set_model\",\"success\":false,\"error\":\"unknown model\"}"
    fi
    ;;

  *'"type":"set_thinking_level"'*)
    CURRENT_THINKING=$(printf '%s' "$line" | sed 's/.*"level":"\([^"]*\)".*/\1/')
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"set_thinking_level\",\"success\":true}"
    ;;

  *'"type":"get_session_stats"'*)
    stats_reply "$line"
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

    *scenario:mcp-login*)
      # Pi's built-in `/mcp login`: the link arrives in a notify, then an
      # input dialog races the paste against the loopback callback.
      emit '{"type":"extension_ui_request","id":"oauth-link","method":"notify","message":"Sign in to MCP server \"docs\" in your browser:\nhttps://auth.example/authorize?state=attempt&redirect_uri=http%3A%2F%2Flocalhost%3A8976%2Fcallback","notifyType":"info"}'
      emit '{"type":"extension_ui_request","id":"oauth-dialog","method":"input","title":"Waiting for sign-in to \"docs\". If the browser cannot reach this machine, paste the URL it was redirected to.","placeholder":"http://127.0.0.1:.../callback?code=..."}'
      next_cmd answer
      if has "$answer" '"id":"oauth-dialog"' && has "$answer" '"value":"http://localhost:8976/callback?state=attempt&code=fixture"'; then
        emit '{"type":"extension_ui_request","id":"oauth-done","method":"notify","message":"Signed in to MCP server \"docs\" (1 tools).","notifyType":"info"}'
        emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      else
        emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"callback not forwarded\"}"
      fi
      exit 0
      ;;

    *scenario:happy*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      # Thinking + text deltas; a non-text delta maps to nothing.
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","contentIndex":0,"delta":"thinking"}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Hello"}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":" world"}}'
      # The message ends to call a tool; the tool executes after message_end.
      # It reports its output tokens; m2 below does not (the estimate stands).
      # The provider answered m1 with another model than requested.
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","model":"gpt-6-astra","responseModel":"gpt-5.4","content":[{"type":"text","text":"Hello world"}],"usage":{"output":7},"stopReason":"toolUse"}}'
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
      emit '{"type":"message_end","message":{"role":"assistant","id":"m2","model":"gpt-6-astra","content":[{"type":"text","text":"Done."}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      exit 0
      ;;

    *scenario:codemode*)
      # Pi 1.0 codemode, as Pi emits it: the script call, the calls the
      # script makes (ids `{script}/{n}`, `parentToolCallId`), script updates
      # that carry only `details.calls`, and a result that opens with the
      # status header. The nested MCP call uses Pi's sanitized tool name.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[],"stopReason":"toolUse"}}'
      emit '{"type":"tool_execution_start","toolCallId":"s1","toolName":"codemode","args":{"code":"const v = await tools.read({ path: \"release.json\" });\nreturn await tools.mcp__mvp_lab_discord__search({ query: v });"}}'
      emit '{"type":"tool_execution_start","toolCallId":"s1/1","toolName":"read","args":{"path":"release.json"},"parentToolCallId":"s1"}'
      emit '{"type":"tool_execution_update","toolCallId":"s1","toolName":"codemode","partialResult":{"content":[],"details":{"calls":[{"id":"s1/?","name":"read","status":"running"}]}}}'
      emit '{"type":"tool_execution_end","toolCallId":"s1/1","toolName":"read","result":{"content":[{"type":"text","text":"1.0.0.2"}]},"isError":false,"parentToolCallId":"s1"}'
      emit '{"type":"tool_execution_start","toolCallId":"s1/2","toolName":"mcp__mvp_lab_discord__search","args":{"query":"1.0.0.2"},"parentToolCallId":"s1"}'
      emit '{"type":"tool_execution_end","toolCallId":"s1/2","toolName":"mcp__mvp_lab_discord__search","result":{"content":[{"type":"text","text":"3 messages"}]},"isError":false,"parentToolCallId":"s1"}'
      emit '{"type":"tool_execution_end","toolCallId":"s1","toolName":"codemode","result":{"content":[{"type":"text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"text","text":"3 messages"}],"details":{"calls":[]}},"isError":false}'
      emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"found 3"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"found 3"}],"stopReason":"stop"}}'
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
      next_cmd steerline || exit 1
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

    # ---- pi >= 0.99: every accepted input reports its disposition ----------
    # Having seen one on the first prompt, the harness routes a mid-turn
    # message as an atomic `prompt` with `streamingBehavior:"steer"` and acts
    # on what pi says it did. A raw `steer` here is a failure.
    *scenario:routed-queued*)
      # pi is running, so it QUEUES the message; the reply is the next
      # assistant message, where the Steered boundary fires.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"toolUse"}}'
      emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"read","args":{"path":"src/main.rs"}}'
      emit '{"type":"tool_execution_end","toolCallId":"t1","toolName":"read","result":{"content":[{"type":"text","text":"// main"}]},"isError":false}'
      next_cmd steerline || exit 1
      if has "$steerline" '"type":"prompt"' && has "$steerline" '"streamingBehavior":"steer"' && has "$steerline" 'redirect please'; then
        emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"queued\"}}"
        emit '{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"redirect please"}]}}'
        emit '{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"redirect please"}]}}'
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"steered"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"steered"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"mid-turn message must ride an atomic prompt\"}"
      exit 1
      ;;
    *scenario:routed-settled*)
      # The settle race: pi settles the turn before it reads the routed
      # message, so the prompt STARTS a fresh run — and its response follows
      # the settle on the wire, back to back. The harness must Done the first
      # turn, open the next with one Steered boundary, and never retry the
      # message (pi already ran it). Exiting right after keeps a retry from
      # going unnoticed: it would surface as an extra boundary and an error.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"toolUse"}}'
      emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"read","args":{"path":"x"}}'
      emit '{"type":"tool_execution_end","toolCallId":"t1","toolName":"read","result":{"content":[{"type":"text","text":"x"}]},"isError":false}'
      next_cmd steerline || exit 1
      if has "$steerline" '"type":"prompt"' && has "$steerline" '"streamingBehavior":"steer"' && has "$steerline" 'redirect'; then
        emit '{"type":"agent_settled"}'
        emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}"
        emit '{"type":"agent_start"}'
        emit '{"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"redirect"}]}}'
        emit '{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"redirect"}]}}'
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"redirected"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"redirected"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"mid-turn message must ride an atomic prompt\"}"
      exit 1
      ;;
    *scenario:routed-handled*)
      # A slash command routed mid-turn: pi runs it at once and reports it
      # `handled` — nothing will stream for it. It still owes the engine one
      # Steered boundary (its ledger retires one per routed message), at the
      # next assistant message, and is never retried.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"toolUse"}}'
      emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"read","args":{"path":"x"}}'
      emit '{"type":"tool_execution_end","toolCallId":"t1","toolName":"read","result":{"content":[{"type":"text","text":"x"}]},"isError":false}'
      next_cmd steerline || exit 1
      if has "$steerline" '"type":"prompt"' && has "$steerline" '"streamingBehavior":"steer"' && has "$steerline" '/fast'; then
        emit '{"type":"extension_ui_request","id":"fast-1","method":"notify","message":"GPT Fast mode enabled.","notifyType":"info"}'
        emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"handled\"}}"
        emit '{"type":"message_start","message":{"role":"toolResult","toolCallId":"t1","content":[]}}'
        emit '{"type":"message_end","message":{"role":"toolResult","toolCallId":"t1","content":[]}}'
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"carried on"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"carried on"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"mid-turn command must ride an atomic prompt\"}"
      exit 1
      ;;
    *scenario:routed-inert*)
      # A notify-only command turn (pi ran no agent) is still inside its
      # no-activity grace when the next message arrives. The harness routes
      # it mid-turn; pi is idle, so it STARTS a fresh run. The inert turn must
      # close (Done carrying its notify) BEFORE the new turn's boundary — not
      # absorb the new run's output. The notify follows the response so the
      # test's send is certain to be routed mid-turn.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"handled\"}}"
      emit '{"type":"extension_ui_request","id":"goal-1","method":"notify","message":"No goal set.","notifyType":"info"}'
      next_cmd steerline || exit 1
      if has "$steerline" '"type":"prompt"' && has "$steerline" '"streamingBehavior":"steer"' && has "$steerline" 'next message'; then
        emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{\"disposition\":\"started\"}}"
        emit '{"type":"agent_start"}'
        emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"fresh run"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"fresh run"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"mid-turn message must ride an atomic prompt\"}"
      exit 1
      ;;
    *scenario:inert-stranded*)
      # No dispositions (pi < 0.99): a steer routed inside a notify-only
      # command's grace reaches an IDLE pi, which only queues it. When the
      # grace settles the inert turn, the harness must clear pi's queue and
      # retry the message as a parked prompt — a stranded steer also blocked
      # every later parked restart.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"extension_ui_request","id":"goal-1","method":"notify","message":"No goal set.","notifyType":"info"}'
      next_cmd steerline || exit 1
      if has "$steerline" '"type":"steer"' && has "$steerline" 'later message'; then
        emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":true}"
        next_cmd cleared || exit 1
        if has "$cleared" '"type":"clear_queue"'; then
          next_cmd retry || exit 1
          if has "$retry" '"type":"prompt"' && has "$retry" '"streamingBehavior":"steer"' && has "$retry" 'later message'; then
            emit "{\"id\":$(rid "$retry"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
            emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
            emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"later reply"}}'
            emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"later reply"}],"stopReason":"stop"}}'
            emit '{"type":"agent_settled"}'
            exit 0
          fi
          emit "{\"id\":$(rid "$retry"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"stranded steer must retry as a parked prompt\"}"
          exit 1
        fi
        emit "{\"id\":$(rid "$cleared"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"stranded steer must clear the queue before its retry\"}"
        exit 1
      fi
      emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"legacy pi expects a raw steer\"}"
      exit 1
      ;;


    *scenario:parked-compact*)
      # A `/compact` sent to a PARKED child must run pi's `compact` RPC (pi's
      # `prompt` never executes TUI built-ins), stream its summary, settle,
      # and stay parked — while every gauge read reports context usage.
      STATS_DATA='{"contextUsage":{"tokens":150000,"contextWindow":200000,"percent":75}}'
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"first"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"first"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      next_cmd second || exit 1
      if has "$second" '"type":"compact"'; then
        # Post-compaction pi reports no token count until the next LLM
        # response: the harness falls back to the compaction's estimate.
        STATS_DATA='{"contextUsage":{"tokens":null,"contextWindow":200000,"percent":null}}'
        emit '{"type":"compaction_start","reason":"manual"}'
        emit '{"type":"compaction_end","reason":"manual","result":{"summary":"s","firstKeptEntryId":"e1","tokensBefore":150000,"estimatedTokensAfter":32000},"aborted":false,"willRetry":false}'
        emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"compact\",\"success\":true,\"data\":{\"tokensBefore\":150000,\"estimatedTokensAfter\":32000}}"
        while next_cmd line; do :; done
        exit 0
      fi
      emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"parked /compact must run the compact RPC\"}"
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
      next_cmd second || exit 1
      if has "$second" '"type":"prompt"' && has "$second" '"streamingBehavior":"steer"' && has "$second" 'notify only'; then
        emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        # 150ms < the 200ms test grace: only a freshly re-armed timer reaches
        # this notify; a stale turn-1 timer fires immediately on re-poll.
        sleep 0.15
        emit '{"type":"extension_ui_request","id":"u-2","method":"notify","message":"second turn note","notifyType":"info"}'
        # Never settle, never emit lifecycle events: the harness must reap us.
        while read -r line; do :; done
        exit 0
      fi
      emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"parked mailbox must restart via prompt\"}"
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
      next_cmd second || exit 1
      if has "$second" '"type":"prompt"' && has "$second" '"streamingBehavior":"steer"' && has "$second" 'second turn'; then
        emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"second"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"second"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        next_cmd third || exit 1
        if has "$third" '"type":"prompt"' && has "$third" '"streamingBehavior":"steer"' && has "$third" 'third turn'; then
          emit "{\"id\":$(rid "$third"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
          emit '{"type":"message_start","message":{"role":"assistant","id":"m3","content":[]}}'
          emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"third"}}'
          emit '{"type":"message_end","message":{"role":"assistant","id":"m3","content":[{"type":"text","text":"third"}],"stopReason":"stop"}}'
          emit '{"type":"agent_settled"}'
          exit 0
        fi
        emit "{\"id\":$(rid "$third"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"third must be a parked prompt\"}"
        exit 1
      fi
      emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"second must be a parked prompt\"}"
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
      next_cmd steerline || exit 1
      if has "$steerline" '"type":"steer"' && has "$steerline" 'redirect'; then
        # Accepted, then settled WITHOUT ever delivering the steer reply. The
        # steer now sits in pi's queue, which its next run drains at start:
        # without a clear first, the retry would deliver it twice.
        emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":true}"
        emit '{"type":"agent_settled"}'
        next_cmd cleared || exit 1
        if ! has "$cleared" '"type":"clear_queue"'; then
          emit "{\"id\":$(rid "$cleared"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"stranded steer must clear the queue before its retry\"}"
          exit 1
        fi
        next_cmd retry || exit 1
        if has "$retry" '"type":"prompt"' && has "$retry" '"streamingBehavior":"steer"' && has "$retry" 'redirect'; then
          emit "{\"id\":$(rid "$retry"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
          emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
          emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"redirected"}}'
          emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"redirected"}],"stopReason":"stop"}}'
          emit '{"type":"agent_settled"}'
          exit 0
        fi
        emit "{\"id\":$(rid "$retry"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"stranded steer must retry as a parked prompt\"}"
        exit 1
      fi
      emit "{\"id\":$(rid "$steerline"),\"type\":\"response\",\"command\":\"steer\",\"success\":false,\"error\":\"expected a mid-turn steer\"}"
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
      next_cmd second || exit 1
      if has "$second" '"type":"prompt"' && has "$second" '"streamingBehavior":"steer"' && has "$second" 'second message'; then
        # Pre-response notify, then acceptance, then the second turn.
        emit '{"type":"extension_ui_request","id":"u-2","method":"notify","message":"pre-response note","notifyType":"info"}'
        emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"second"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"second"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"parked mailbox must restart via prompt, not steer\"}"
      exit 1
      ;;
    *scenario:interrupt*)
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"working"}}'
      next_cmd abortline || exit 1
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
      next_cmd ans || exit 1
      if has "$ans" '"type":"extension_ui_response"' && has "$ans" '"id":"u-1"' && has "$ans" '"value":"tokio"'; then
        emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"answered"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"answered"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      exit 1
      ;;
    *scenario:ui-select-before-ack*)
      # Real pi: extension commands ACK the prompt only AFTER the handler
      # returns, and `/subagent-config` blocks on ctx.ui.select first. The
      # harness must drain the UI request while the prompt RPC is still in
      # flight or the run deadlocks (Working forever, no question panel).
      emit '{"type":"extension_ui_request","id":"u-1","method":"select","title":"Pick a runtime","options":["tokio","smol"]}'
      next_cmd ans || exit 1
      if has "$ans" '"type":"extension_ui_response"' && has "$ans" '"id":"u-1"' && has "$ans" '"value":"tokio"'; then
        emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
        emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"answered"}}'
        emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"answered"}],"stopReason":"stop"}}'
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      exit 1
      ;;

    *scenario:status-before-ack*)
      # Real pi at startup: the goal, MCP and subagents extensions push
      # `setStatus` furniture BEFORE the prompt is acknowledged, and an input
      # extension (translation) delays the ACK by seconds. A plain prompt then
      # streams a normal turn after a short pause. That status traffic must
      # not shorten the no-activity grace: the ONLY Done is the real settle,
      # after the assistant text. (2026-09-20 regression: the harness Done'd
      # at the ACK, parking the session before the agent's first event.)
      emit '{"type":"extension_ui_request","id":"s-1","method":"setStatus","statusKey":"goal","statusText":""}'
      emit '{"type":"extension_ui_request","id":"s-2","method":"setStatus","statusKey":"cypher.subagents.v1","statusText":""}'
      emit '{"type":"extension_ui_request","id":"s-3","method":"setStatus","statusKey":"mcp","statusText":"3 servers"}'
      sleep 0.3
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      # Shorter than the test grace, longer than a zero-length sleep: a
      # collapsed grace fires here; an intact one waits for the turn.
      sleep 0.1
      emit '{"type":"agent_start"}'
      emit '{"type":"turn_start"}'
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"real turn"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"real turn"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      exit 0
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

    *scenario:stateful-notify*)
      # Stateful notify-only extension command (the /fast regression). The
      # state lives only in this process. Cypher must settle the first turn
      # without reaping us, then route the second command back here as a
      # parked prompt so the state can toggle in the opposite direction.
      emit '{"type":"extension_ui_request","id":"fast-1","method":"notify","message":"GPT Fast mode disabled.","notifyType":"info"}'
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      next_cmd second || exit 1
      if has "$second" '"type":"prompt"' && has "$second" '"streamingBehavior":"steer"' && has "$second" '/fast'; then
        emit '{"type":"extension_ui_request","id":"fast-2","method":"notify","message":"GPT Fast mode enabled (service_tier: priority).","notifyType":"info"}'
        emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
        while read -r line; do :; done
        exit 0
      fi
      emit "{\"id\":$(rid "$second"),\"type\":\"response\",\"command\":\"prompt\",\"success\":false,\"error\":\"stateful command must reuse the parked process\"}"
      exit 1
      ;;

    *scenario:silent-thinking*)
      # A long reasoning step: pi streams nothing while the provider thinks.
      # The harness's liveness probe (get_state) finds pi still streaming and
      # must heartbeat each time; the answer lands after the second probe.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"agent_start"}'
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      probes=0
      while [ "$probes" -lt 2 ]; do
        next_cmd probe || exit 1
        if has "$probe" '"type":"get_state"'; then
          emit "{\"id\":$(rid "$probe"),\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"isStreaming\":true,\"isCompacting\":false}}"
          probes=$((probes + 1))
        fi
      done
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"thought it through"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"thought it through"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      exit 0
      ;;

    *scenario:lost-settle*)
      # The run finishes but its agent_settled is never written (pi marks the
      # run inactive before an extension's settle handler throws). The probe
      # finds pi idle: the harness must settle the turn itself.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"agent_start"}'
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"all done"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"all done"}],"stopReason":"stop"}}'
      next_cmd probe || exit 1
      if has "$probe" '"type":"get_state"'; then
        emit "{\"id\":$(rid "$probe"),\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"isStreaming\":false,\"isCompacting\":false}}"
        exit 0
      fi
      exit 1
      ;;

    *scenario:self-run*)
      # The prompt turn settles; then pi wakes on its own (a background task
      # finished) and runs with no prompt outstanding. That run thinks
      # silently (one probe: still streaming), then settles: the harness
      # must close it with a Done of its own.
      emit "{\"id\":$pid,\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
      emit '{"type":"agent_start"}'
      emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"started the build"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m1","content":[{"type":"text","text":"started the build"}],"stopReason":"stop"}}'
      emit '{"type":"agent_settled"}'
      emit '{"type":"agent_start"}'
      emit '{"type":"message_start","message":{"role":"assistant","id":"m2","content":[]}}'
      emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"build is green"}}'
      emit '{"type":"message_end","message":{"role":"assistant","id":"m2","content":[{"type":"text","text":"build is green"}],"stopReason":"stop"}}'
      next_cmd probe || exit 1
      if has "$probe" '"type":"get_state"'; then
        emit "{\"id\":$(rid "$probe"),\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"isStreaming\":true,\"isCompacting\":false}}"
        emit '{"type":"agent_settled"}'
        exit 0
      fi
      exit 1
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
