#!/bin/sh
# Wedged pi for cypher-harness tests: answers setup + prompt, streams one
# delta, then hangs (ignores abort entirely) — the SIGTERM/SIGKILL escalation
# path. Driven by crates/harness/tests/pi.rs.

emit() { printf '%s\n' "$1"; }
# pi ids are strings — keep the surrounding quotes in the echoed id.
rid() { printf '%s' "$1" | sed 's/.*"id":"\([^"]*\)".*/"\1"/'; }

while read -r line; do
  [ -z "$line" ] && continue
  case "$line" in
  *'"type":"get_state"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"sessionFile\":\"/tmp/pi-test/session.jsonl\",\"model\":null}}"
    ;;
  *'"type":"set_model"'*|*'"type":"set_thinking_level"'*|*'"type":"switch_session"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"x\",\"success\":true}"
    ;;
  *'"type":"prompt"'*)
    emit "{\"id\":$(rid "$line"),\"type\":\"response\",\"command\":\"prompt\",\"success\":true}"
    emit '{"type":"message_start","message":{"role":"assistant","id":"m1","content":[]}}'
    emit '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"working"}}'
    exec sleep 30
    ;;
  esac
done
